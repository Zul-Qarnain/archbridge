use crate::archive::{snapshot_directory, snapshot_tarball_bytes, FileReview};
use crate::build::{
    execute_chroot_build, run_runtime_smoke_test, snapshot_foreign_package, SmokeTestReport,
};
use crate::config::Config;
use crate::discovery::{search, DecisionReport};
use crate::inspect::inspect_package;
use crate::process::{begin_phase, bump_phase_total, end_phase, run_step, run_step_bytes, Step};
use crate::recipe::{
    detect_build_system, detect_build_system_in_archive, generate_pkgbuild, sanitize_entry_name,
    BuildSystem, PkgbuildParams,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Upstream source archives are downloaded into memory to be hashed and
/// snapshot-reviewed, so an unbounded fetch would let a remote host decide how
/// much of the machine's RAM is consumed.
const MAX_SOURCE_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;

/// A reviewed plan is a promise about bytes that were on disk minutes ago: the
/// sources it pins, the PKGBUILD shown to the user, and the dependency state
/// the recommendation was computed from all go stale. Executing a plan long
/// after it was staged would let the host drift under the review, so consent
/// expires with the review.
const PLAN_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FilesystemChange {
    pub path: String,
    pub action: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Plan {
    pub protocol_version: String,
    pub plan_id: String,
    pub action: String,
    pub summary: String,
    pub steps: Vec<Step>,
    pub filesystem_changes: Vec<FilesystemChange>,
    pub reviews: Vec<FileReview>,
    pub warnings: Vec<String>,
    pub decision: Option<DecisionReport>,
    pub downstream: Option<String>,
    pub blocked: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecutionResult {
    pub ok: bool,
    pub plan_id: String,
    pub message: String,
    pub output: Option<String>,
    pub next_plan: Option<Plan>,
}

pub struct StagedBuild {
    pub job_id: String,
    pub target: String,
    pub pkgbuild: String,
    pub source_tarball: Vec<u8>,
    pub entry: String,
    pub smoke_args: Vec<String>,
    pub run_smoke_test: bool,
    pub artifact_path: Option<PathBuf>,
}

pub struct Engine {
    pub config: Config,
    pub pending_plans: HashMap<String, (Plan, Instant, Option<StagedBuild>)>,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

/// Strip interactive password prompts from sudo's stderr so GUI surfaces can
/// show a single human-readable line.
fn sanitize_sudo_stderr(stderr: &str) -> String {
    stderr
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty() && !line.starts_with("[sudo]") && !line.starts_with("sudo:")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Keeps the cached sudo timestamp alive while a privileged plan executes.
/// Long clean-chroot builds can exceed sudo's `timestamp_timeout`; without
/// refreshes, later `sudo -n` steps fail with "a password is required".
struct SudoKeepalive {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl SudoKeepalive {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let stop = stop.clone();
            std::thread::spawn(move || loop {
                for _ in 0..24 {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(Duration::from_secs(5));
                }
                let step = Step::new(
                    "sudo",
                    vec!["-n".to_string(), "true".to_string()],
                    "Refresh cached sudo timestamp",
                    15,
                );
                if run_step(&step, None).is_err() {
                    return;
                }
            })
        };
        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for SudoKeepalive {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Engine {
    /// Validate the administrator password and establish a short-lived sudo timestamp.
    /// The password is written only to sudo's stdin and is never stored in the engine.
    ///
    /// Note: hosts with `pam_fprintd` in `/etc/pam.d/sudo` make sudo wait ~30 s for a
    /// fingerprint before the password prompt is served, so the step budget must stay
    /// above that window or authentication spuriously "times out".
    pub fn authenticate(&mut self, password: &str) -> Result<(), String> {
        if password.is_empty() {
            return Err("Authentication password is required".to_string());
        }
        let step = Step::new(
            "sudo",
            vec!["-S".to_string(), "-v".to_string()],
            "Authenticate administrator session",
            180,
        );
        let mut input = password.as_bytes().to_vec();
        input.push(b'\n');
        let result = match run_step(&step, Some(&input)) {
            Ok(result) => result,
            Err(err) => {
                input.fill(0);
                return Err(if err.contains("timed out") {
                    format!(
                        "Administrator authentication timed out. {err} This host waits for a \
                         fingerprint before serving the password prompt — tick \
                         'skip fingerprint wait' and retry, or run the fix in Settings."
                    )
                } else {
                    err
                });
            }
        };
        input.fill(0);
        if result.exit_code == 0 {
            // Confirm the cached timestamp is usable by the non-interactive
            // `sudo -n` invocations inside execution plans. Without this check
            // authentication can appear to succeed while every later step
            // fails with "sudo: a password is required".
            let verify = Step::new(
                "sudo",
                vec!["-n".to_string(), "true".to_string()],
                "Verify cached sudo credential",
                15,
            );
            let verified = run_step(&verify, None)?;
            if verified.exit_code == 0 {
                Ok(())
            } else {
                Err("Administrator password accepted, but the credential could not be cached for non-interactive use. Check sudo `timestamp_type`/`timestamptimeout` settings.".to_string())
            }
        } else {
            let wrong_password = result.stderr.contains("incorrect password")
                || result.stderr.contains("Sorry, try again")
                || result.stderr.contains("no password was provided");
            let message = if wrong_password {
                "Incorrect administrator password.".to_string()
            } else {
                let detail = sanitize_sudo_stderr(&result.stderr);
                if detail.is_empty() {
                    "Administrator authentication failed".to_string()
                } else {
                    format!("Administrator authentication failed: {detail}")
                }
            };
            Err(message)
        }
    }

    /// Invalidate any cached sudo credentials for this user.
    pub fn deauthenticate(&mut self) -> Result<(), String> {
        let step = Step::new(
            "sudo",
            vec!["-k".to_string()],
            "Invalidate cached sudo credentials",
            15,
        );
        let result = run_step(&step, None)?;
        if result.exit_code == 0 {
            Ok(())
        } else {
            let detail = result.stderr.trim();
            Err(if detail.is_empty() {
                "Failed to invalidate sudo credentials".to_string()
            } else {
                format!("Failed to invalidate sudo credentials: {detail}")
            })
        }
    }

    /// True when `/etc/pam.d/sudo` asks `pam_fprintd` to authenticate before the
    /// password module. On such hosts every `sudo` invocation blocks ~30 s
    /// waiting for a finger, which is what makes authentication look like it
    /// "times out" even with a correct password.
    pub fn fingerprint_before_password() -> bool {
        let Ok(text) = fs::read_to_string("/etc/pam.d/sudo") else {
            return false;
        };
        let mut first_fprintd: Option<usize> = None;
        let mut first_password: Option<usize> = None;
        for (i, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if !trimmed.starts_with("auth") {
                continue;
            }
            if first_fprintd.is_none() && trimmed.contains("pam_fprintd") {
                first_fprintd = Some(i);
            }
            if first_password.is_none()
                && (trimmed.contains("pam_unix") || trimmed.contains("system-auth"))
            {
                first_password = Some(i);
            }
        }
        matches!((first_fprintd, first_password), (Some(f), Some(p)) if f < p)
    }

    /// One-time, opt-in system tweak: comment out the eager `pam_fprintd`
    /// prompt in `/etc/pam.d/sudo` so the password is checked immediately.
    /// A numbered backup of the original file is kept by `install -b`.
    /// Fingerprint auth stays available in every other PAM stack (login, sudo
    /// via other tools, etc.) — only the interactive `sudo` wait is removed.
    pub fn optimize_sudo_auth(password: &str) -> Result<String, String> {
        if !Self::fingerprint_before_password() {
            return Ok(
                "sudo already checks the password before the fingerprint reader.".to_string(),
            );
        }
        if password.is_empty() {
            return Err("Administrator password is required to adjust PAM settings".to_string());
        }
        let original = fs::read_to_string("/etc/pam.d/sudo")
            .map_err(|e| format!("Cannot read /etc/pam.d/sudo: {e}"))?;
        let patched = original
            .lines()
            .map(|line| {
                let trimmed = line.trim();
                if !trimmed.starts_with('#') && trimmed.contains("pam_fprintd") {
                    format!("# archbridge: fingerprint wait removed for password-first sudo\n#{trimmed}")
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";

        let staged =
            std::env::temp_dir().join(format!("archbridge-pam-sudo-{}", std::process::id()));
        fs::write(&staged, &patched).map_err(|e| format!("Cannot stage updated PAM file: {e}"))?;

        let step = Step::new(
            "sudo",
            vec![
                "-S".to_string(),
                "install".to_string(),
                "-b".to_string(),
                "--backup=numbered".to_string(),
                "-m".to_string(),
                "0644".to_string(),
                staged.to_string_lossy().to_string(),
                "/etc/pam.d/sudo".to_string(),
            ],
            "Disable fingerprint wait for sudo",
            90,
        );
        let mut input = password.as_bytes().to_vec();
        input.push(b'\n');
        let result = run_step(&step, Some(&input));
        input.fill(0);
        let _ = fs::remove_file(&staged);
        let result = result?;
        if result.exit_code == 0 && !Self::fingerprint_before_password() {
            Ok("sudo now checks the password immediately; the original PAM file was backed up as /etc/pam.d/sudo.~N~.".to_string())
        } else {
            let detail = sanitize_sudo_stderr(&result.stderr);
            Err(if detail.is_empty() {
                "Failed to disable the sudo fingerprint wait".to_string()
            } else {
                format!("Failed to disable the sudo fingerprint wait: {detail}")
            })
        }
    }

    pub fn new() -> Self {
        let config = Config::load().unwrap_or_default();
        Self {
            config,
            pending_plans: HashMap::new(),
        }
    }

    /// An engine whose preferences live at `config_path` instead of the user's
    /// real config file. Tests and sandboxed runs must be able to exercise
    /// `config.set` without rewriting someone's discovery settings.
    pub fn with_config_path(config_path: &Path) -> Self {
        let config = Config::load_from(config_path).unwrap_or_else(|_| Config {
            path: config_path.to_path_buf(),
            ..Config::default()
        });
        Self {
            config,
            pending_plans: HashMap::new(),
        }
    }

    /// Stage a plan so it becomes executable, dropping any plan whose review
    /// window has already closed. A daemon can serve searches for days; without
    /// this the pending store grows without bound and keeps stale consent alive.
    fn stage_plan(&mut self, plan: Plan, staged: Option<StagedBuild>) -> Plan {
        self.purge_expired();
        self.pending_plans
            .insert(plan.plan_id.clone(), (plan.clone(), Instant::now(), staged));
        plan
    }

    /// Forget every staged plan whose review window has closed.
    fn purge_expired(&mut self) {
        self.pending_plans
            .retain(|_, entry| entry.1.elapsed() < PLAN_TTL);
    }

    pub fn prepare_search(&mut self, target: &str, repo: Option<&str>) -> Result<Plan, String> {
        let report = search(target, repo, &self.config);
        let plan_id = generate_plan_id("search", target);

        let summary = format!(
            "Search target '{}': recommended source is '{:?}'",
            target, report.recommended_source
        );

        let plan = Plan {
            protocol_version: "v1.0".to_string(),
            plan_id: plan_id.clone(),
            action: "search".to_string(),
            summary,
            steps: vec![],
            filesystem_changes: vec![],
            reviews: vec![],
            warnings: vec![],
            decision: Some(report),
            downstream: None,
            blocked: None,
        };

        let plan = self.stage_plan(plan, None);
        Ok(plan)
    }

    /// Stage an inspection plan and return the reviewable report alongside it.
    /// The report is parsed once: foreign packages are large and re-reading them
    /// for a second caller doubles the work.
    pub fn prepare_inspect(
        &mut self,
        file_path: &str,
    ) -> Result<(Plan, crate::inspect::InspectionReport), String> {
        let report = inspect_package(file_path)?;
        let plan_id = generate_plan_id("inspect", file_path);

        let summary = format!(
            "Inspect package '{}': format={}, pkg={}, version={}",
            file_path, report.format, report.package_name, report.version
        );

        let mut reviews = Vec::new();
        for script in &report.maintainer_scripts {
            reviews.push(FileReview {
                path: format!("script:{}", script.name),
                sha256: format!("{:x}", Sha256::digest(script.content.as_bytes())),
                bytes: script.content.len() as u64,
                executable: false,
                content: Some(script.content.clone()),
            });
        }

        let plan = Plan {
            protocol_version: "v1.0".to_string(),
            plan_id: plan_id.clone(),
            action: "inspect".to_string(),
            summary,
            steps: vec![],
            filesystem_changes: vec![],
            reviews,
            warnings: vec!["Foreign scripts inspected but will NEVER be executed".to_string()],
            decision: None,
            downstream: None,
            blocked: None,
        };

        let plan = self.stage_plan(plan, None);
        Ok((plan, report))
    }

    pub fn prepare_config_set(&mut self, key: &str, value: &str) -> Result<Plan, String> {
        let mut test_config = self.config.clone();
        test_config.set(key, value)?;

        let plan_id = generate_plan_id("config.set", key);
        let summary = format!("Set configuration key '{}' to '{}'", key, value);

        // The plan has to name the file execution will actually write.
        let config_path = self.config.path.clone();

        let plan = Plan {
            protocol_version: "v1.0".to_string(),
            plan_id: plan_id.clone(),
            action: "config.set".to_string(),
            summary,
            steps: vec![],
            filesystem_changes: vec![FilesystemChange {
                path: config_path.to_string_lossy().to_string(),
                action: "modify".to_string(),
                description: format!("Update configuration key '{}' = '{}'", key, value),
            }],
            reviews: vec![],
            warnings: vec![],
            decision: None,
            downstream: None,
            blocked: None,
        };

        let plan = self.stage_plan(plan, None);
        Ok(plan)
    }

    pub fn prepare_build(
        &mut self,
        target: &str,
        name_opt: Option<&str>,
        ver_opt: Option<&str>,
        entry_opt: Option<&str>,
        deps: &[String],
        smoke_args: &[String],
    ) -> Result<Plan, String> {
        let inferred_name = name_opt.map(str::to_string).unwrap_or_else(|| {
            if let Some(pos) = target.rfind('/') {
                target[pos + 1..].to_string()
            } else {
                target.to_string()
            }
        });
        let name = inferred_name
            .strip_suffix(".deb")
            .or_else(|| inferred_name.strip_suffix(".rpm"))
            .unwrap_or(&inferred_name);
        let default_entry = sanitize_entry_name(name);
        let entry = entry_opt.unwrap_or(&default_entry);
        let version = ver_opt.unwrap_or("1.0.0");

        let job_id = format!("job-{}", generate_plan_id("build", target));
        let job_dir = PathBuf::from(".archbridge").join("jobs").join(&job_id);

        let (mut snapshot, build_sys, pkgbuild, package_name) = if target.ends_with(".deb")
            || target.ends_with(".rpm")
        {
            let snap = snapshot_foreign_package(Path::new(target))?;
            let source_tar = if snap
                .tarball_bytes
                .starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0x00])
            {
                "src.tar.xz".to_string()
            } else if snap.tarball_bytes.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
                "src.tar.zst".to_string()
            } else {
                "src.tar.gz".to_string()
            };

            let inspected = inspect_package(target).ok();
            let effective_name = if let Some(ref rep) = inspected {
                if rep.package_name != "unknown" && !rep.package_name.is_empty() {
                    rep.package_name.clone()
                } else {
                    name.to_string()
                }
            } else {
                name.to_string()
            };
            let effective_ver = if let Some(ref rep) = inspected {
                if rep.version != "unknown" && !rep.version.is_empty() {
                    rep.version.clone()
                } else {
                    version.to_string()
                }
            } else {
                version.to_string()
            };

            let final_name = name_opt.unwrap_or(&effective_name);
            let sanitized_name = final_name.to_lowercase().replace('_', "-");
            let final_ver = ver_opt.unwrap_or(&effective_ver);
            let sanitized_ver = final_ver.replace('-', "_");
            // A foreign package names its own launcher, so the file list it ships
            // beats any name inferred from the archive file name.
            let entry_owned = entry_opt.map(str::to_string).unwrap_or_else(|| {
                foreign_entry(&snap.reviews, &sanitized_name)
                    .unwrap_or_else(|| sanitize_entry_name(&effective_name))
            });
            let entry = entry_owned.as_str();

            let params = PkgbuildParams {
                name: sanitized_name.clone(),
                version: sanitized_ver,
                source_tarball: source_tar,
                sha256_hash: snap.sha256_hash.clone(),
                dependencies: deps.to_vec(),
                entry_binary: Some(entry.to_string()),
                src_subdir: None,
            };
            let pkgb = generate_pkgbuild(&BuildSystem::Imported, &params)?;
            (snap, BuildSystem::Imported, pkgb, sanitized_name)
        } else if target.ends_with(".db") {
            return Err(
                "An Arch .db file is a repository index and contains no package payload to build. Download the corresponding .pkg.tar.* package instead.".to_string(),
            );
        } else if target.ends_with("PKGBUILD") {
            let pkgbuild_content = fs::read_to_string(target)
                .map_err(|e| format!("Failed to read PKGBUILD at '{}': {}", target, e))?;
            let parent_dir = Path::new(target).parent().unwrap_or_else(|| Path::new("."));
            let snap = snapshot_directory(parent_dir)?;
            (snap, BuildSystem::Make, pkgbuild_content, name.to_string())
        } else if Path::new(target).exists() && Path::new(target).is_dir() {
            let snap = snapshot_directory(Path::new(target))?;
            let bsys = detect_build_system(Path::new(target));
            let params = PkgbuildParams {
                name: name.to_string(),
                version: version.to_string(),
                source_tarball: "src.tar.gz".to_string(),
                sha256_hash: snap.sha256_hash.clone(),
                dependencies: deps.to_vec(),
                entry_binary: Some(entry.to_string()),
                src_subdir: None,
            };

            let pkgb = match generate_pkgbuild(&bsys, &params) {
                Ok(p) => p,
                Err(err_msg) => {
                    return Ok(blocked_build_plan(target, snap.reviews, err_msg));
                }
            };
            (snap, bsys, pkgb, name.to_string())
        } else if target.starts_with("https://github.com/")
            || target.starts_with("https://gitlab.com/")
        {
            let row = search(target, Some(target), &self.config);
            let upstream = row.decision_table.iter().find(|r| r.source == "upstream");
            let archive_url = upstream
                .filter(|r| r.availability == "available")
                .and_then(|r| r.candidates.first())
                .and_then(|c| c.url.clone());
            let url = match archive_url {
                Some(u) => u,
                None => {
                    // The discovery row already knows whether this was a rate limit,
                    // a missing repository or an unsupported host; pass that through.
                    return Err(format!(
                        "Cannot prepare a build for '{}': {}",
                        target,
                        upstream
                            .map(|r| r.reason.as_str())
                            .unwrap_or("no upstream source was queried")
                    ));
                }
            };
            let upstream_version = upstream
                .and_then(|r| r.candidates.first())
                .and_then(|c| c.version.clone());
            let step = Step::new(
                "curl",
                vec![
                    "-fsSL",
                    "--max-time",
                    "120",
                    "--max-filesize",
                    &MAX_SOURCE_ARCHIVE_BYTES.to_string(),
                    &url,
                ],
                "Download upstream source archive",
                150,
            );
            let download = run_step_bytes(&step, None)?;
            if download.exit_code != 0 {
                return Err(format!(
                    "Failed to download '{}' (curl exit {}): {}",
                    url,
                    download.exit_code,
                    download.stderr.trim()
                ));
            }
            if !download.bytes.starts_with(&[0x1F, 0x8B]) {
                let head =
                    String::from_utf8_lossy(&download.bytes[..download.bytes.len().min(200)])
                        .trim()
                        .to_string();
                return Err(if head.starts_with('<') {
                    format!(
                        "'{}' returned a web page instead of a source archive; the release URL is not directly downloadable.",
                        url
                    )
                } else {
                    format!(
                        "'{}' is not a gzip-compressed tar archive; ArchBridge builds upstream sources from .tar.gz only.",
                        url
                    )
                });
            }

            let snap = snapshot_tarball_bytes(&download.bytes)?;
            let (bsys, src_subdir) = detect_build_system_in_archive(&snap.reviews);
            let params = PkgbuildParams {
                name: name.to_string(),
                version: upstream_version.unwrap_or_else(|| version.to_string()),
                source_tarball: "src.tar.gz".to_string(),
                sha256_hash: snap.sha256_hash.clone(),
                dependencies: deps.to_vec(),
                entry_binary: Some(entry.to_string()),
                src_subdir,
            };
            let pkgb = match generate_pkgbuild(&bsys, &params) {
                Ok(p) => p,
                Err(err_msg) => {
                    return Ok(blocked_build_plan(target, snap.reviews, err_msg));
                }
            };
            (snap, bsys, pkgb, name.to_string())
        } else {
            return Err(format!(
                "Build target '{}' is not a valid local path, PKGBUILD, or HTTPS URL",
                target
            ));
        };

        let plan_id = generate_plan_id("build", target);

        // Mirrors src/build.rs: the unprivileged native pass runs first and the
        // privileged clean chroot is only reached if it produces no package.
        let mut steps = vec![
            Step::new(
                "makepkg",
                vec!["--force".to_string(), "--clean".to_string()],
                "Build package with native makepkg",
                900,
            ),
            Step::new(
                "sudo",
                vec![
                    "-n",
                    "mkarchroot",
                    "-C",
                    "/etc/pacman.conf",
                    job_dir.join("chroot/root").to_str().unwrap(),
                    "base",
                    "base-devel",
                    "sudo",
                ],
                "Initialize clean chroot environment (fallback)",
                900,
            ),
            Step::new(
                "sudo",
                vec![
                    "-n",
                    "makechrootpkg",
                    "-r",
                    job_dir.join("chroot").to_str().unwrap(),
                    "--",
                    "--syncdeps",
                    "--noconfirm",
                    "--cleanbuild",
                    "--clean",
                    "--force",
                ],
                "Build package inside clean chroot (fallback)",
                1200,
            ),
        ];

        let run_smoke_test = !matches!(&build_sys, BuildSystem::Imported) || entry_opt.is_some();
        if run_smoke_test {
            steps.push(Step::new(
                "systemd-nspawn",
                vec![
                    "--private-users=pick",
                    "--private-network",
                    "--user",
                    "65534",
                    "-D",
                    job_dir.join("test_root").to_str().unwrap(),
                    entry,
                ],
                "Runtime smoke test inside isolated container",
                30,
            ));
        }

        let filesystem_changes = vec![FilesystemChange {
            path: job_dir.to_string_lossy().to_string(),
            action: "create".to_string(),
            description: "Stage job workspace and chroot for isolated package build".to_string(),
        }];

        // The script makepkg will actually run belongs to what the user reviews and
        // confirms; staging it only inside the engine would ask for consent blind.
        snapshot.reviews.push(FileReview {
            path: "PKGBUILD".to_string(),
            sha256: format!("{:x}", Sha256::digest(pkgbuild.as_bytes())),
            bytes: pkgbuild.len() as u64,
            executable: false,
            content: Some(pkgbuild.clone()),
        });

        let plan = Plan {
            protocol_version: "v1.0".to_string(),
            plan_id: plan_id.clone(),
            action: "build".to_string(),
            summary: format!(
                "Build '{}' with detected build system '{:?}': unprivileged makepkg first, isolated clean chroot as fallback",
                package_name, build_sys
            ),
            steps,
            filesystem_changes,
            reviews: snapshot.reviews,
            warnings: {
                let mut notes = vec![
                    "The native build needs no administrator rights; the clean chroot fallback does, and it is only used if the native build produces no package".to_string(),
                ];
                if run_smoke_test {
                    notes.push(
                        "Built package is smoke tested in an isolated container".to_string(),
                    );
                } else {
                    notes.push(
                        "Foreign payload will be repackaged; runtime smoke test requires an explicit entry binary"
                            .to_string(),
                    );
                }
                notes
            },
            decision: None,
            downstream: Some(if run_smoke_test {
                "Upon passing runtime smoke test, package will be available for installation"
                    .to_string()
            } else {
                "After payload repackaging, the generated Arch package will be available for installation"
                    .to_string()
            }),
            blocked: None,
        };

        let staged = StagedBuild {
            job_id,
            target: target.to_string(),
            pkgbuild,
            source_tarball: snapshot.tarball_bytes,
            entry: entry.to_string(),
            smoke_args: smoke_args.to_vec(),
            run_smoke_test,
            artifact_path: None,
        };

        let plan = self.stage_plan(plan, Some(staged));
        Ok(plan)
    }

    pub fn prepare_install(&mut self, target: &str) -> Result<Plan, String> {
        if target.ends_with(".deb") || target.ends_with(".rpm") {
            return self.prepare_build(target, None, None, None, &[], &[]);
        }
        if target.ends_with(".db") {
            return Err(
                "An Arch .db file is a repository index, not an installable package. Download the matching .pkg.tar.* file.".to_string(),
            );
        }

        if target.contains(".pkg.tar.") {
            let plan_id = generate_plan_id("install", target);
            let steps = vec![Step::new(
                "sudo",
                vec!["-n", "pacman", "-U", "--noconfirm", target],
                "Install prebuilt native package",
                60,
            )];
            let plan = Plan {
                protocol_version: "v1.0".to_string(),
                plan_id: plan_id.clone(),
                action: "install".to_string(),
                summary: format!("Install native Arch package '{}'", target),
                steps,
                filesystem_changes: vec![FilesystemChange {
                    path: "/".to_string(),
                    action: "modify".to_string(),
                    description: format!("Install package '{}' to live system root", target),
                }],
                reviews: vec![],
                warnings: vec!["Modifies system packages on host".to_string()],
                decision: None,
                downstream: None,
                blocked: None,
            };
            let plan = self.stage_plan(plan, None);
            return Ok(plan);
        }

        let report = search(target, None, &self.config);
        let rec = report.recommended_source.clone();

        match rec.as_deref() {
            Some("official") => {
                let plan_id = generate_plan_id("install", target);
                let steps = vec![Step::new(
                    "sudo",
                    vec!["-n", "pacman", "-S", "--needed", "--noconfirm", target],
                    "Install official Arch package",
                    60,
                )];
                let plan = Plan {
                    protocol_version: "v1.0".to_string(),
                    plan_id: plan_id.clone(),
                    action: "install".to_string(),
                    summary: format!("Install official package '{}' via pacman", target),
                    steps,
                    filesystem_changes: vec![],
                    reviews: vec![],
                    warnings: vec![],
                    decision: Some(report),
                    downstream: None,
                    blocked: None,
                };
                let plan = self.stage_plan(plan, None);
                Ok(plan)
            }
            _ => self.prepare_build(target, None, None, None, &[], &[]),
        }
    }

    pub fn prepare_uninstall(&mut self, target: &str) -> Result<Plan, String> {
        let package = target.trim();
        if package.is_empty()
            || package == "."
            || package == ".."
            || package.len() > 128
            || !package
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"@._+-".contains(&c))
        {
            return Err(
                "Uninstall requires an installed Arch package name, not a path or file."
                    .to_string(),
            );
        }

        let plan_id = generate_plan_id("uninstall", package);
        let plan = Plan {
            protocol_version: "v1.0".to_string(),
            plan_id: plan_id.clone(),
            action: "uninstall".to_string(),
            summary: format!("Uninstall installed package '{}' with pacman", package),
            steps: vec![Step::new(
                "sudo",
                vec!["-n", "pacman", "-Rns", "--noconfirm", package],
                "Uninstall selected package and unused dependencies",
                120,
            )],
            filesystem_changes: vec![FilesystemChange {
                path: "/".to_string(),
                action: "modify".to_string(),
                description: format!("Uninstall package '{}' from the live system", package),
            }],
            reviews: vec![],
            warnings: vec![
                "This changes the live system and may remove dependencies no longer needed."
                    .to_string(),
            ],
            decision: None,
            downstream: None,
            blocked: None,
        };
        let plan = self.stage_plan(plan, None);
        Ok(plan)
    }

    pub fn prepare_test(
        &mut self,
        pkg_path: &str,
        entry_opt: Option<&str>,
        _smoke_args: &[String],
    ) -> Result<Plan, String> {
        let entry = entry_opt.unwrap_or_else(|| {
            if let Some(pos) = pkg_path.rfind('/') {
                &pkg_path[pos + 1..]
            } else {
                pkg_path
            }
        });

        let plan_id = generate_plan_id("test", pkg_path);
        let steps = vec![Step::new(
            "systemd-nspawn",
            vec![
                "--private-users=pick",
                "--private-network",
                "--user",
                "65534",
                "-D",
                "/tmp/disposable_test_root",
                entry,
            ],
            "Execute runtime smoke test",
            30,
        )];

        let plan = Plan {
            protocol_version: "v1.0".to_string(),
            plan_id: plan_id.clone(),
            action: "test".to_string(),
            summary: format!(
                "Test package artifact '{}' with entry binary '{}'",
                pkg_path, entry
            ),
            steps,
            filesystem_changes: vec![],
            reviews: vec![],
            warnings: vec!["Runs package binary in isolated container".to_string()],
            decision: None,
            downstream: None,
            blocked: None,
        };

        let plan = self.stage_plan(plan, None);
        Ok(plan)
    }

    pub fn execute_plan(
        &mut self,
        plan_id: &str,
        confirmed: bool,
    ) -> Result<ExecutionResult, String> {
        if !confirmed {
            return Err("Execution refused: missing explicit user confirmation".to_string());
        }

        let (plan, created, staged_opt) = self.pending_plans.remove(plan_id).ok_or_else(|| {
            format!(
                "Plan '{}' is no longer staged. Its review has been consumed or withdrawn; \
                 prepare the action again to review and confirm a fresh plan.",
                plan_id
            )
        })?;
        if created.elapsed() >= PLAN_TTL {
            return Err(format!(
                "Plan '{}' expired: more than {} minutes passed since it was reviewed, so the \
                 sources, hashes and dependency state it shows are no longer guaranteed. \
                 Prepare the action again.",
                plan_id,
                PLAN_TTL.as_secs() / 60
            ));
        }

        if let Some(blocked_msg) = &plan.blocked {
            return Err(format!("Cannot execute blocked plan: {}", blocked_msg));
        }

        // Publish the phase to the live progress tracker so the GUI overlay
        // can show elapsed time, the currently running step, and a percentage.
        // Builds start with the native makepkg attempt; build.rs widens the
        // count if the chroot fallback is needed.
        let total_steps = match plan.action.as_str() {
            "build" => 1,
            "install" | "uninstall" => plan.steps.len(),
            _ => 0,
        };
        let label = match plan.action.as_str() {
            "build" => "Building package",
            "install" => "Installing package",
            "uninstall" => "Uninstalling package",
            other => other,
        };
        begin_phase(label, total_steps);
        let outcome = self.execute_staged(plan_id, plan, staged_opt);
        end_phase();
        outcome
    }

    fn execute_staged(
        &mut self,
        plan_id: &str,
        plan: Plan,
        staged_opt: Option<StagedBuild>,
    ) -> Result<ExecutionResult, String> {
        // Privileged plans can run for many minutes; refresh the cached sudo
        // timestamp in the background so later `sudo -n` steps keep working.
        let _sudo_keepalive = match plan.action.as_str() {
            "build" | "install" | "uninstall" => Some(SudoKeepalive::start()),
            _ => None,
        };

        match plan.action.as_str() {
            "search" | "info" => Ok(ExecutionResult {
                ok: true,
                plan_id: plan_id.to_string(),
                message: "Search completed successfully".to_string(),
                output: serde_json::to_string_pretty(&plan.decision).ok(),
                next_plan: None,
            }),
            "inspect" => Ok(ExecutionResult {
                ok: true,
                plan_id: plan_id.to_string(),
                message: "Inspection completed successfully".to_string(),
                output: Some("Package inspected. Maintainer scripts unexecuted.".to_string()),
                next_plan: None,
            }),
            "config.set" => {
                let change = plan
                    .filesystem_changes
                    .first()
                    .ok_or("config.set plan carries no recorded change")?;
                let (k, v) = change
                    .description
                    .strip_prefix("Update configuration key '")
                    .and_then(|s| s.split_once("' = '"))
                    .ok_or("config.set plan recorded an unreadable change")?;
                let val = v.trim_end_matches('\'');
                self.config.set(k, val)?;
                self.config.save()?;
                // Every other staged plan was reviewed under the old preferences:
                // its recommendation, pinned sources and step list can all change
                // when a source is routed away. Their consent is void.
                self.pending_plans.clear();
                Ok(ExecutionResult {
                    ok: true,
                    plan_id: plan_id.to_string(),
                    message: format!("Configuration key '{k}' updated and saved"),
                    output: None,
                    next_plan: None,
                })
            }
            "build" => {
                if let Some(staged) = staged_opt {
                    let job_dir = PathBuf::from(".archbridge")
                        .join("jobs")
                        .join(&staged.job_id);
                    let art_path =
                        execute_chroot_build(&job_dir, &staged.pkgbuild, &staged.source_tarball)?;
                    // The install plan shells out to `pacman -U` in a later RPC, and the
                    // job directory is recorded relative to the CWD that built it. Store
                    // the absolute path so the install works from any working directory.
                    let art_path = match art_path.canonicalize() {
                        Ok(absolute) => absolute,
                        Err(_) => art_path,
                    };
                    let art_path =
                        publish_artifact(&art_path, &self.config.output_dir).map_err(|e| {
                            format!("{e}; the build itself succeeded at {}", art_path.display())
                        })?;

                    if staged.run_smoke_test {
                        bump_phase_total(1);
                        let smoke_res: SmokeTestReport =
                            run_runtime_smoke_test(&art_path, &staged.entry, &staged.smoke_args)?;
                        if !smoke_res.passed {
                            return Ok(ExecutionResult {
                                ok: false,
                                plan_id: plan_id.to_string(),
                                message: format!(
                                    "Runtime smoke test failed for entry '{}': {}",
                                    staged.entry, smoke_res.stderr
                                ),
                                output: Some(format!(
                                    "Exit code: {}\nStdout: {}\nStderr: {}",
                                    smoke_res.exit_code, smoke_res.stdout, smoke_res.stderr
                                )),
                                next_plan: None,
                            });
                        }
                    }

                    // Create next_plan for install
                    let install_plan_id = generate_plan_id("install", &staged.target);
                    let next_plan = Plan {
                        protocol_version: "v1.0".to_string(),
                        plan_id: install_plan_id.clone(),
                        action: "install".to_string(),
                        summary: format!("Install built package '{}'", art_path.display()),
                        steps: vec![Step::new(
                            "sudo",
                            vec![
                                "-n".to_string(),
                                "pacman".to_string(),
                                "-U".to_string(),
                                "--noconfirm".to_string(),
                                art_path.to_string_lossy().to_string(),
                            ],
                            "Install package to host",
                            120,
                        )],
                        filesystem_changes: vec![FilesystemChange {
                            path: "/".to_string(),
                            action: "modify".to_string(),
                            description: format!(
                                "Install package '{}' to host system",
                                art_path.display()
                            ),
                        }],
                        reviews: vec![],
                        warnings: vec!["Modifies system packages on host".to_string()],
                        decision: None,
                        downstream: None,
                        blocked: None,
                    };

                    self.stage_plan(next_plan.clone(), None);

                    Ok(ExecutionResult {
                        ok: true,
                        plan_id: plan_id.to_string(),
                        message: if staged.run_smoke_test {
                            format!("Build and smoke test passed for '{}'", art_path.display())
                        } else {
                            format!("Package repackaged successfully at {}", art_path.display())
                        },
                        output: Some(format!("Artifact produced at {}", art_path.display())),
                        next_plan: Some(next_plan),
                    })
                } else {
                    Err("Staged build data missing".to_string())
                }
            }
            "install" | "uninstall" => {
                let mut last_out = String::new();
                for step in &plan.steps {
                    let out = run_step(step, None)?;
                    if out.exit_code != 0 {
                        return Ok(ExecutionResult {
                            ok: false,
                            plan_id: plan_id.to_string(),
                            message: format!(
                                "Step '{}' failed with code {}",
                                step.purpose, out.exit_code
                            ),
                            output: Some(out.stderr),
                            next_plan: None,
                        });
                    }
                    last_out = out.stdout;
                }
                Ok(ExecutionResult {
                    ok: true,
                    plan_id: plan_id.to_string(),
                    message: if plan.action == "uninstall" {
                        "Package removal completed successfully".to_string()
                    } else {
                        "Installation completed successfully".to_string()
                    },
                    output: Some(last_out),
                    next_plan: None,
                })
            }
            "test" => Ok(ExecutionResult {
                ok: true,
                plan_id: plan_id.to_string(),
                message: "Test execution completed".to_string(),
                output: None,
                next_plan: None,
            }),
            _ => Err(format!("Unknown action '{}'", plan.action)),
        }
    }
}

/// The launcher a foreign package actually ships, chosen from its `usr/bin`
/// file list. Prefers a binary named after the package, then the shortest name.
pub fn foreign_entry(reviews: &[FileReview], preferred: &str) -> Option<String> {
    let mut shipped: Vec<String> = reviews
        .iter()
        .filter_map(|review| review.path.strip_prefix("usr/bin/"))
        .filter(|rest| !rest.contains('/') && !rest.is_empty())
        .map(str::to_string)
        .filter(|name| sanitize_entry_name(name) == *name)
        .collect();
    shipped.sort();
    shipped.sort_by_key(|name| (name != preferred, name.len()));
    shipped.into_iter().next()
}

/// A plan that reviewed the source but runs no steps, so the reason reaches the
/// caller through the same channel as any other plan instead of as an RPC error.
fn blocked_build_plan(target: &str, reviews: Vec<FileReview>, reason: String) -> Plan {
    Plan {
        protocol_version: "v1.0".to_string(),
        plan_id: generate_plan_id("build", target),
        action: "build".to_string(),
        summary: format!("Build plan for '{}': blocked", target),
        steps: vec![],
        filesystem_changes: vec![],
        reviews,
        warnings: vec![],
        decision: None,
        downstream: None,
        blocked: Some(reason),
    }
}

/// Copy a finished artifact into the user's configured output directory.
/// An empty setting leaves the artifact in the job directory that built it.
/// The copy is re-hashed before it is trusted, so the install plan can only
/// ever point at the bytes the build produced.
pub fn publish_artifact(artifact: &Path, output_dir: &str) -> Result<PathBuf, String> {
    let dir = output_dir.trim();
    if dir.is_empty() {
        return Ok(artifact.to_path_buf());
    }
    let name = artifact
        .file_name()
        .ok_or_else(|| "Build artifact has no file name to publish".to_string())?;
    let dest = Path::new(dir).join(name);
    if dest == artifact {
        return Ok(artifact.to_path_buf());
    }
    fs::create_dir_all(dir).map_err(|e| format!("Cannot create output directory '{dir}': {e}"))?;
    fs::copy(artifact, &dest).map_err(|e| {
        format!(
            "Cannot copy the artifact to '{dir}': {e}",
            dir = dest.display()
        )
    })?;
    let source_hash = sha256_file(artifact)?;
    let copy_hash = sha256_file(&dest)?;
    if source_hash != copy_hash {
        return Err(format!(
            "Published artifact '{}' does not match the bytes the build produced",
            dest.display()
        ));
    }
    Ok(dest.canonicalize().unwrap_or(dest))
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|e| format!("Cannot read '{}': {}", path.display(), e))?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

fn generate_plan_id(action: &str, target: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(action.as_bytes());
    hasher.update(target.as_bytes());
    hasher.update(format!("{:?}", Instant::now()).as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    format!("{}-{}", action, &hash[..12])
}

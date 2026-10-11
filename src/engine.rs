use crate::archive::{snapshot_directory, snapshot_tarball_bytes, FileReview};
use crate::build::{
    clean_job_dir, execute_chroot_build, package_files, resolve_entry, run_runtime_smoke_test,
    snapshot_foreign_package, SmokeTestReport,
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

/// How many plans stay reviewable at once. The review window alone does not
/// bound the store: inside ten minutes a client can stage as many plans as it
/// likes, and each one carries the file-review text of everything it looked at.
const MAX_PENDING_PLANS: usize = 32;

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

/// What a staged `test` plan will really run: the artifact that was reviewed,
/// the entry binary resolved out of its own file list, and the arguments the
/// user passed. Without this the execution step would have nothing to run.
pub struct StagedTest {
    pub artifact: PathBuf,
    pub entry: String,
    pub smoke_args: Vec<String>,
}

/// The work a staged plan carries past the review, kept out of the JSON a user
/// confirms so the plan stays a description rather than a closure.
pub enum StagedAction {
    Build(StagedBuild),
    Test(StagedTest),
}

pub struct Engine {
    pub config: Config,
    pub pending_plans: HashMap<String, (Plan, Instant, Option<StagedAction>)>,
    /// Why the preferences file could not be read. Kept so every method that
    /// would otherwise plan against guessed defaults says so instead.
    config_error: Option<String>,
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
        Self::from_load(Config::load(), None)
    }

    /// An engine whose preferences live at `config_path` instead of the user's
    /// real config file. Tests and sandboxed runs must be able to exercise
    /// `config.set` without rewriting someone's discovery settings.
    pub fn with_config_path(config_path: &Path) -> Self {
        Self::from_load(Config::load_from(config_path), Some(config_path))
    }

    /// Building an engine never fails: an unreadable preferences file is remembered
    /// and reported by every method that would have planned against it, instead of
    /// being silently replaced by defaults the user never chose.
    fn from_load(loaded: Result<Config, String>, config_path: Option<&Path>) -> Self {
        let (config, config_error) = match loaded {
            Ok(config) => (config, None),
            Err(error) => {
                // Keep the file this engine was pointed at: a fallback that
                // carried the default path could later be saved into the real
                // user configuration.
                let fallback = match config_path {
                    Some(path) => Config {
                        path: path.to_path_buf(),
                        ..Config::default()
                    },
                    None => Config::default(),
                };
                (fallback, Some(error))
            }
        };
        Self {
            config,
            pending_plans: HashMap::new(),
            config_error,
        }
    }

    /// The preference-store error this engine was constructed with, if any.
    pub fn config_error(&self) -> Option<&str> {
        self.config_error.as_deref()
    }

    /// Refuse to stage a review whose outcome depends on preferences that could
    /// not be read: which sources are enabled, and where the artifact goes.
    fn require_config(&self) -> Result<(), String> {
        match &self.config_error {
            Some(error) => Err(format!(
                "Preferences could not be read, so nothing can be planned against them: {error}"
            )),
            None => Ok(()),
        }
    }

    /// Read one preference, or report why the whole store is unusable.
    pub fn config_get(&self, key: &str) -> Result<serde_json::Value, String> {
        self.require_config()?;
        self.config.get(key)
    }

    /// Stage a plan so it becomes executable, dropping any plan whose review
    /// window has already closed. A daemon can serve searches for days; without
    /// this the pending store grows without bound and keeps stale consent alive.
    fn stage_plan(&mut self, plan: Plan, staged: Option<StagedAction>) -> Plan {
        self.purge_expired();
        // Ageing out is not a bound: every plan staged inside one review window
        // would be kept. The oldest review is withdrawn first, exactly as if its
        // window had closed on its own.
        while self.pending_plans.len() >= MAX_PENDING_PLANS {
            let Some(oldest) = self
                .pending_plans
                .iter()
                .max_by_key(|(_, entry)| entry.1.elapsed())
                .map(|(plan_id, _)| plan_id.clone())
            else {
                break;
            };
            self.pending_plans.remove(&oldest);
        }
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
        self.require_config()?;
        let report = search(target, repo, &self.config);
        let plan_id = generate_plan_id(
            "search",
            &review_binding(&[("target", target), ("repo", repo.unwrap_or(""))]),
        );

        let summary = format!(
            "Search target '{}': recommended source is {}",
            target,
            report
                .recommended_source
                .as_deref()
                .map_or("none".to_string(), |source| format!("'{source}'"))
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
        // Saving over an unreadable file would replace preferences the user still
        // has on disk with the defaults plus one key, so refuse until it is fixed.
        self.require_config()?;
        let mut test_config = self.config.clone();
        test_config.set(key, value)?;

        let plan_id = generate_plan_id(
            "config.set",
            &review_binding(&[("key", key), ("value", value)]),
        );
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

        // `target` and `pkgbuild` carry the change itself: execution must write the
        // key and value that were reviewed, not a re-reading of the plan's prose.
        let staged = StagedAction::Build(StagedBuild {
            job_id: "config.set".to_string(),
            target: key.to_string(),
            pkgbuild: value.to_string(),
            source_tarball: Vec::new(),
            entry: String::new(),
            smoke_args: Vec::new(),
            run_smoke_test: false,
            artifact_path: None,
        });
        let plan = self.stage_plan(plan, Some(staged));
        Ok(plan)
    }

    /// A bare name that the user configured as `repo.<name>` stands for that
    /// repository URL. A real local path always wins over a configured name.
    fn configured_source(&self, target: &str) -> Option<String> {
        if target.contains('/') || target.contains("://") || Path::new(target).exists() {
            return None;
        }
        self.config.repos.get(target).cloned()
    }

    /// An HTTPS `.tar.gz`/`.tgz` archive on a host other than github.com and
    /// gitlab.com, fetched only when the user vouched for that host by
    /// configuring a `repo.*` URL on it. Anything else keeps being refused.
    fn configured_archive_url(&self, source: &str) -> Result<Option<String>, String> {
        let Some(host) = https_host(source) else {
            return Ok(None);
        };
        if host == "github.com" || host == "gitlab.com" {
            return Ok(None);
        }
        let path = source.split(['?', '#']).next().unwrap_or(source);
        if !(path.ends_with(".tar.gz") || path.ends_with(".tgz")) {
            return Ok(None);
        }
        let vouched = self
            .config
            .repos
            .values()
            .any(|configured| https_host(configured).as_deref() == Some(host.as_str()));
        if !vouched {
            return Ok(None);
        }
        if let Some(reason) = unsafe_fetch_host(&host) {
            return Err(format!("Refusing to fetch '{source}': {reason}"));
        }
        Ok(Some(source.to_string()))
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
        self.require_config()?;
        validate_target(target)?;
        // `source` is what the build actually reads: a bare name configured as
        // `repo.<name>` stands for its URL, everything else is the target itself.
        let source = self
            .configured_source(target)
            .unwrap_or_else(|| target.to_string());
        let source = source.as_str();
        let direct_archive = self.configured_archive_url(source)?;

        let inferred_name = name_opt
            .map(str::to_string)
            .unwrap_or_else(|| infer_build_name(source));
        let name = inferred_name
            .strip_suffix(".deb")
            .or_else(|| inferred_name.strip_suffix(".rpm"))
            .unwrap_or(&inferred_name);
        let default_entry = sanitize_entry_name(name);
        let entry = entry_opt.unwrap_or(&default_entry);
        // This value is an argument vector element of the container launch and a
        // file name inside the PKGBUILD, so it can neither be empty nor read as an
        // option of `systemd-nspawn` itself.
        validate_token(entry, "Entry binary name")?;
        if let Some(name) = name_opt {
            validate_token(name, "Package name")?;
        }
        if let Some(version) = ver_opt {
            validate_token(version, "Package version")?;
        }
        validate_args(smoke_args)?;
        let version = ver_opt.unwrap_or("1.0.0");

        let job_id = format!("job-{}", generate_plan_id("build", target));
        let job_dir = PathBuf::from(".archbridge").join("jobs").join(&job_id);

        let (mut snapshot, build_sys, pkgbuild, package_name) = if target.ends_with(".deb")
            || target.ends_with(".rpm")
        {
            let snap = snapshot_foreign_package(Path::new(target))?;
            let source_tar = crate::build::source_tarball_name(&snap.tarball_bytes);

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
                // The payload is copied, not compiled: what the package says it
                // runs on is what the produced `arch` array may claim.
                architecture: inspected.as_ref().map(|report| report.architecture.clone()),
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
            // `PKGBUILD` on its own has an empty parent, which is not a directory
            // that exists: the file sits in the working directory it was named from.
            let parent_dir = Path::new(target)
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
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
                architecture: None,
            };

            let pkgb = match generate_pkgbuild(&bsys, &params) {
                Ok(p) => p,
                Err(err_msg) => {
                    return Ok(blocked_build_plan(target, snap.reviews, err_msg));
                }
            };
            (snap, bsys, pkgb, name.to_string())
        } else if direct_archive.is_some()
            || source.starts_with("https://github.com/")
            || source.starts_with("https://gitlab.com/")
        {
            let (url, upstream_version) = if let Some(url) = direct_archive.clone() {
                // A configured host serving a plain archive: there is no release
                // metadata to ask, so the review shows the archive's own hash.
                (url, None)
            } else {
                let row = search(source, Some(source), &self.config);
                let upstream = row.decision_table.iter().find(|r| r.source == "upstream");
                let archive_url = upstream
                    .filter(|r| r.availability == "available")
                    .and_then(|r| r.candidates.first())
                    .and_then(|c| c.url.clone());
                let url = match archive_url {
                    Some(u) => u,
                    None => {
                        // The discovery row already knows whether this was a rate
                        // limit, a missing repository or an unsupported host.
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
                (url, upstream_version)
            };
            let step = Step::new(
                "curl",
                vec![
                    "-fsSL",
                    "--max-time",
                    "120",
                    "--max-filesize",
                    &MAX_SOURCE_ARCHIVE_BYTES.to_string(),
                    // `--max-filesize` only bounds a response that declares its
                    // length, and a redirect can step off TLS on the way to the
                    // bytes, so both are stated here and the result is checked.
                    "--max-redirs",
                    "8",
                    "--proto-redir",
                    "=https",
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
            if download.bytes.len() as u64 > MAX_SOURCE_ARCHIVE_BYTES {
                return Err(format!(
                    "'{url}' answered with {} bytes, over the {} byte ceiling an upstream \
                     source archive is fetched within",
                    download.bytes.len(),
                    MAX_SOURCE_ARCHIVE_BYTES
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
                architecture: None,
            };
            let pkgb = match generate_pkgbuild(&bsys, &params) {
                Ok(p) => p,
                Err(err_msg) => {
                    return Ok(blocked_build_plan(target, snap.reviews, err_msg));
                }
            };
            (snap, bsys, pkgb, name.to_string())
        } else {
            // An HTTPS URL from a host that is not queried is a different answer
            // than "this is not a URL": the caller has to learn which to change.
            return Err(if source.starts_with("https://") {
                format!(
                    "Build source '{source}' is an HTTPS URL, but only github.com and gitlab.com \
                     releases and archives (.tar.gz/.tgz) from a host you configured with \
                     `archbridge config set repo.<name> <https-url>` are fetched. Name a local \
                     directory or PKGBUILD, or configure the host first."
                )
            } else {
                format!(
                    "Build target '{}' is not a valid local path, PKGBUILD, or HTTPS URL",
                    target
                )
            });
        };

        let plan_id = generate_plan_id(
            "build",
            &review_binding(&build_binding(
                target,
                &package_name,
                version,
                entry,
                deps,
                smoke_args,
                &pkgbuild,
            )),
        );

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
            let mut launch: Vec<String> = vec![
                "--private-users=pick".to_string(),
                "--private-network".to_string(),
                "--user".to_string(),
                "65534".to_string(),
                "-D".to_string(),
                job_dir.join("test_root").to_str().unwrap().to_string(),
                entry.to_string(),
            ];
            // What the container hands the entry binary is part of what is being
            // agreed to: a review that omits it describes a different run.
            launch.extend(smoke_args.iter().cloned());
            steps.push(Step::new(
                "systemd-nspawn",
                launch,
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

        let staged = StagedAction::Build(StagedBuild {
            job_id,
            target: target.to_string(),
            pkgbuild,
            source_tarball: snapshot.tarball_bytes,
            entry: entry.to_string(),
            smoke_args: smoke_args.to_vec(),
            run_smoke_test,
            artifact_path: None,
        });

        let plan = self.stage_plan(plan, Some(staged));
        Ok(plan)
    }

    pub fn prepare_install(&mut self, target: &str) -> Result<Plan, String> {
        validate_target(target)?;
        if target.ends_with(".deb") || target.ends_with(".rpm") {
            return self.prepare_build(target, None, None, None, &[], &[]);
        }
        if target.ends_with(".db") {
            return Err(
                "An Arch .db file is a repository index, not an installable package. Download the matching .pkg.tar.* file.".to_string(),
            );
        }

        // `install` also accepts something that must be built before it can be
        // installed — a source URL, a directory or a PKGBUILD. A bare name is a
        // discovery lookup instead, handled below.
        if target.starts_with("https://")
            || target.ends_with("PKGBUILD")
            || (target.contains('/') && Path::new(target).is_dir())
        {
            return self.prepare_build(target, None, None, None, &[], &[]);
        }

        if target.contains(".pkg.tar.") {
            // Consent has to be about an action that can happen, and the path
            // reaches `pacman -U` verbatim: a file that is not there installs
            // nothing, and a value starting with '-' is a pacman option.
            if !Path::new(target).is_file() {
                return Err(format!(
                    "Package file '{target}' does not exist. Pass the path of a built .pkg.tar.* package."
                ));
            }
            verify_native_package(target)?;
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

        self.require_config()?;
        let report = search(target, None, &self.config);
        match report.recommended_source.as_deref() {
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
            // A bare name is not a build target, so the old fall-through into
            // `prepare_build` answered "install htop" with "not a valid local
            // path, PKGBUILD, or HTTPS URL" — a message about a different input.
            Some(other) => Err(install_resolution_error(target, Some(other), &report)),
            None => Err(install_resolution_error(target, None, &report)),
        }
    }

    pub fn prepare_uninstall(&mut self, target: &str) -> Result<Plan, String> {
        let package = target.trim();
        let rejection = if package.is_empty() {
            Some("the name is empty")
        } else if package.starts_with('-') {
            // A leading '-' is not an odd package name: it is the shape of a
            // pacman flag, and `-Rns --config=/tmp/x` lets the caller choose the
            // configuration (and therefore the hook directory) this root run reads.
            Some("a name cannot start with '-' (that is a pacman option, not a package)")
        } else if package.starts_with('.') {
            Some("a name cannot start with '.'")
        } else if package.len() > 128 {
            Some("the name is longer than 128 characters")
        } else if package.contains('/') {
            Some("it looks like a path or file; give the installed package's name")
        } else if !package
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"@._+-".contains(&c))
        {
            Some("package names use only ASCII letters, digits and '@ . _ + -'")
        } else {
            None
        };
        if let Some(reason) = rejection {
            return Err(format!(
                "Uninstall requires an installed Arch package name: {reason}."
            ));
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

    /// Stage a runtime smoke test. The plan names the exact program the
    /// container will exec, resolved from the artifact's own file list, and
    /// executing it really starts the container: a plan that only described a
    /// test the user could never see run is worse than no test at all.
    pub fn prepare_test(
        &mut self,
        pkg_path: &str,
        entry_opt: Option<&str>,
        smoke_args: &[String],
    ) -> Result<Plan, String> {
        validate_target(pkg_path)?;
        validate_args(smoke_args)?;
        if let Some(name) = entry_opt {
            validate_token(name, "Entry binary name")?;
        }
        // This plan is the one that claims a runtime result, so it can only be
        // staged over bytes that exist right now: a test over a missing or
        // ununpackable file is a plan that can only report what it never checked.
        let artifact = Path::new(pkg_path);
        if !artifact.is_file() {
            return Err(format!(
                "Test target '{pkg_path}' is not a file. Pass the path of a built .pkg.tar.* package."
            ));
        }
        if !pkg_path.contains(".pkg.tar.") {
            return Err(format!(
                "Test target '{pkg_path}' is not an Arch package archive (.pkg.tar.*), so there is no payload to unpack into a test root."
            ));
        }
        let bytes =
            fs::read(artifact).map_err(|e| format!("Failed to read '{}': {e}", pkg_path))?;
        let reviews = package_files(&bytes)?;

        let entry = match entry_opt {
            Some(name) => name.to_string(),
            None => {
                let preferred = artifact
                    .file_name()
                    .map(|name| name.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                let preferred = preferred.split('-').next().unwrap_or("");
                foreign_entry(&reviews, preferred).ok_or_else(|| {
                    let shipped: Vec<&str> = reviews
                        .iter()
                        .filter(|review| review.path.starts_with("usr/bin/"))
                        .map(|review| review.path.as_str())
                        .take(8)
                        .collect();
                    format!(
                        "'{}' ships no runnable usr/bin entry ({:?}); pass --entry <binary>",
                        pkg_path, shipped
                    )
                })?
            }
        };
        let program = resolve_entry(&reviews, &entry)?;

        let mut container_args = vec![
            "--private-users=pick".to_string(),
            "--private-network".to_string(),
            "--user".to_string(),
            "65534".to_string(),
            "-D".to_string(),
            ISOLATED_TEST_ROOT.to_string(),
            program.clone(),
        ];
        if smoke_args.is_empty() {
            container_args.push("--version".to_string());
        } else {
            container_args.extend(smoke_args.iter().cloned());
        }
        validate_token(&entry, "Entry binary name")?;

        let plan_id = generate_plan_id(
            "test",
            &review_binding(&[("target", pkg_path), ("entry", entry.as_str())]),
        );
        let steps = vec![Step::new(
            "systemd-nspawn",
            container_args
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            "Execute runtime smoke test",
            30,
        )];

        let plan = Plan {
            protocol_version: "v1.0".to_string(),
            plan_id: plan_id.clone(),
            action: "test".to_string(),
            summary: format!(
                "Smoke test '{}' from '{}': unpack the artifact into a throwaway root, then exec \
                 {} with {:?} inside systemd-nspawn ({}) as UID 65534 with a 30 second timeout",
                entry,
                pkg_path,
                program,
                if smoke_args.is_empty() {
                    vec!["--version".to_string()]
                } else {
                    smoke_args.to_vec()
                },
                "--private-users=pick --private-network --user 65534"
            ),
            steps,
            filesystem_changes: vec![FilesystemChange {
                path: std::env::temp_dir().to_string_lossy().to_string(),
                action: "create".to_string(),
                description: format!(
                    "Unpack '{}' into a temporary root that is discarded when the test ends; the \
                     live system is not written to",
                    pkg_path
                ),
            }],
            reviews,
            warnings: vec![
                "The container has no network, no host bind mounts and no privileged user; the \
                 artifact is the only thing inside the root"
                    .to_string(),
                "A dynamically linked binary needs a loader and system libraries that a root \
                 holding only this artifact does not have; that case is reported as an \
                 inconclusive test rather than a package failure"
                    .to_string(),
            ],
            decision: None,
            downstream: Some(
                "A passing smoke test does not install anything; install is a separate plan."
                    .to_string(),
            ),
            blocked: None,
        };

        let staged = StagedAction::Test(StagedTest {
            artifact: fs::canonicalize(artifact).unwrap_or_else(|_| artifact.to_path_buf()),
            entry,
            smoke_args: smoke_args.to_vec(),
        });
        let plan = self.stage_plan(plan, Some(staged));
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
            "install" | "uninstall" | "test" => plan.steps.len(),
            _ => 0,
        };
        let label = match plan.action.as_str() {
            "build" => "Building package",
            "install" => "Installing package",
            "uninstall" => "Uninstalling package",
            "test" => "Running smoke test",
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
        staged_opt: Option<StagedAction>,
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
                message:
                    "Inspection needed no execution: the report was produced when the plan was \
                     staged"
                        .to_string(),
                output: Some(
                    "Foreign package parsed at prepare time. Maintainer scripts and scriptlets \
                     were read as text and never executed."
                        .to_string(),
                ),
                next_plan: None,
            }),
            "config.set" => {
                // The key and value are executed from the staged data, not parsed
                // back out of the sentence the user reads: a value containing
                // "' = '" was cut short there, so the file ended up holding
                // something other than what was reviewed.
                let Some(StagedAction::Build(staged)) = staged_opt else {
                    return Err("config.set plan carries no staged change".to_string());
                };
                self.config.set(&staged.target, &staged.pkgbuild)?;
                self.config.save()?;
                // Every other staged plan was reviewed under the old preferences:
                // its recommendation, pinned sources and step list can all change
                // when a source is routed away. Their consent is void.
                self.pending_plans.clear();
                Ok(ExecutionResult {
                    ok: true,
                    plan_id: plan_id.to_string(),
                    message: format!("Configuration key '{}' updated and saved", staged.target),
                    output: None,
                    next_plan: None,
                })
            }
            "build" => {
                let Some(StagedAction::Build(staged)) = staged_opt else {
                    return Err("Staged build data missing".to_string());
                };
                let job_dir = PathBuf::from(".archbridge")
                    .join("jobs")
                    .join(&staged.job_id);
                let outcome = self.run_staged_build(plan_id, &staged, &job_dir);
                // Success or failure, the working tree (sources, chroot, build
                // trees) is dead weight once the build has ended; only the
                // artifact still needed for the install plan, the PKGBUILD and
                // the log stay, unless the user asked to keep everything.
                if !self.config.keep_jobs {
                    let keep_artifacts = self.config.output_dir.trim().is_empty();
                    let _ = clean_job_dir(&job_dir, keep_artifacts);
                }
                outcome
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
            "test" => {
                let staged = match staged_opt {
                    Some(StagedAction::Test(staged)) => staged,
                    Some(StagedAction::Build(_)) => {
                        return Err(
                            "Plan carries build data but was staged as a smoke test".to_string()
                        )
                    }
                    None => return Err("Staged smoke-test data missing".to_string()),
                };
                let report =
                    run_runtime_smoke_test(&staged.artifact, &staged.entry, &staged.smoke_args)?;
                Ok(ExecutionResult {
                    ok: report.passed,
                    plan_id: plan_id.to_string(),
                    message: if report.passed {
                        format!("Runtime smoke test passed: {} exited 0", report.entry_point)
                    } else {
                        format!(
                            "Runtime smoke test did not pass: {} exited {}",
                            report.entry_point, report.exit_code
                        )
                    },
                    output: Some(format!(
                        "Program: {}\nExit code: {}\nStdout: {}\nStderr: {}",
                        report.entry_point, report.exit_code, report.stdout, report.stderr
                    )),
                    next_plan: None,
                })
            }
            _ => Err(format!("Unknown action '{}'", plan.action)),
        }
    }

    /// Build the staged job and, when it passes, stage the install plan that
    /// follows it. Cleanup of the job directory is the caller's job so that it
    /// runs on every exit path, including the `?` returns in here.
    fn run_staged_build(
        &mut self,
        plan_id: &str,
        staged: &StagedBuild,
        job_dir: &Path,
    ) -> Result<ExecutionResult, String> {
        let art_path = execute_chroot_build(job_dir, &staged.pkgbuild, &staged.source_tarball)?;
        // The install plan shells out to `pacman -U` in a later RPC, and the
        // job directory is recorded relative to the CWD that built it. Store
        // the absolute path so the install works from any working directory.
        let art_path = match art_path.canonicalize() {
            Ok(absolute) => absolute,
            Err(_) => art_path,
        };
        let art_path = publish_artifact(&art_path, &self.config.output_dir)
            .map_err(|e| format!("{e}; the build itself succeeded at {}", art_path.display()))?;

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
                description: format!("Install package '{}' to host system", art_path.display()),
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

/// `install <name>` is a discovery lookup. When discovery cannot name a source
/// that installs with a package command, the error has to say what was asked,
/// what each source answered, and what to run next — the old message complained
/// about "a valid local path, PKGBUILD, or HTTPS URL" for input that was never
/// meant to be a path.
fn install_resolution_error(
    target: &str,
    recommended: Option<&str>,
    report: &DecisionReport,
) -> String {
    let mut message = match recommended {
        Some(source) => format!(
            "Cannot install '{target}' by name: discovery recommends the '{source}' source, which has no install-by-name command. Decide the action explicitly:",
        ),
        None => format!(
            "Cannot install '{target}' by name: no enabled source reported an available package. What each source said:",
        ),
    };
    for row in &report.decision_table {
        message.push_str(&format!(
            "\n  {}: {} — {}",
            row.source, row.availability, row.reason
        ));
    }
    message.push_str(&format!("\n\nNext steps:\n  archbridge search {target}"));
    if let Some(url) = report
        .decision_table
        .iter()
        .find(|row| row.source == "upstream")
        .and_then(|row| row.candidates.first())
        .and_then(|candidate| candidate.url.clone())
    {
        message.push_str(&format!(
            "\n  archbridge build {url}   # build from the upstream source"
        ));
    }
    message.push_str("\n  archbridge install <file.pkg.tar.zst|file.deb|file.rpm>   # install a concrete artifact");
    message
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

/// The `-D` value shown in a runtime-test review. The container root is created
/// when the plan runs, so no path written at review time could name it honestly.
const ISOLATED_TEST_ROOT: &str = "<temporary isolated test root>";

/// Longest target the engine will stat, read, fetch, or print in a review.
const MAX_TARGET_BYTES: usize = 4096;

/// A build or test target becomes an argument to `pacman`, `curl` and `bsdtar`,
/// and a line in the review someone consents to. One printable argument, no more.
fn validate_target(target: &str) -> Result<(), String> {
    if target.trim().is_empty() {
        return Err("Target cannot be empty".to_string());
    }
    if target.len() > MAX_TARGET_BYTES {
        return Err(format!(
            "Target is {} bytes; no path or URL this engine reads is longer than {MAX_TARGET_BYTES}",
            target.len()
        ));
    }
    if target.contains('\0') {
        return Err(
            "Target contains a NUL byte, which ends a path at the operating-system boundary"
                .to_string(),
        );
    }
    if target.contains(char::is_control) {
        return Err(
            "Target contains control characters; one argument cannot hold a newline or escape"
                .to_string(),
        );
    }
    if target.starts_with('-') {
        return Err(format!(
            "Target '{target}' starts with '-': the tool that runs it would read it as one of \
             its own options rather than as something to act on."
        ));
    }
    Ok(())
}

/// A package name, version or entry binary the engine hands to an argument vector
/// and interpolates into the generated PKGBUILD.
fn validate_token(value: &str, what: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{what} cannot be empty"));
    }
    if value.len() > 128 {
        return Err(format!("{what} is longer than 128 characters"));
    }
    if value.contains(char::is_control) {
        return Err(format!("{what} contains control characters"));
    }
    if value.chars().any(char::is_whitespace) {
        return Err(format!(
            "{what} contains whitespace; a generated PKGBUILD could not hold it"
        ));
    }
    if value.contains('/') {
        return Err(format!(
            "{what} must be a plain file name, not a path: pass 'demo', not 'usr/bin/demo'"
        ));
    }
    if value.starts_with('-') {
        return Err(format!(
            "{what} cannot start with '-': it would be read as an option, not a name"
        ));
    }
    Ok(())
}

/// Arguments the smoke test passes to the entry binary. These legitimately begin
/// with '-' — that is how `--version` is asked for — so only what cannot be one
/// argv element is refused here.
fn validate_args(args: &[String]) -> Result<(), String> {
    if args.len() > 32 {
        return Err(format!(
            "A smoke test takes at most 32 arguments, got {}",
            args.len()
        ));
    }
    for arg in args {
        if arg.len() > 512 {
            return Err(format!(
                "Smoke-test argument is {} bytes, longer than the 512 an argument can be",
                arg.len()
            ));
        }
        if arg.contains(char::is_control) {
            return Err("Smoke-test arguments cannot contain control characters".to_string());
        }
    }
    Ok(())
}

/// The fields a review covers, each labelled and length-prefixed so no two
/// different reviews of the same target bind to the same string.
fn review_binding(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(label, value)| format!("{label}:{}:{value}", value.len()))
        .collect()
}

/// What a build consent is about: the target, the names and options that shaped
/// the PKGBUILD, and the exact script the plan will stage.
fn build_binding<'a>(
    target: &'a str,
    name: &'a str,
    version: &'a str,
    entry: &'a str,
    deps: &'a [String],
    smoke_args: &'a [String],
    pkgbuild: &'a str,
) -> Vec<(&'a str, &'a str)> {
    let mut fields: Vec<(&str, &str)> = vec![
        ("target", target),
        ("name", name),
        ("version", version),
        ("entry", entry),
    ];
    fields.extend(deps.iter().map(|dep| ("dep", dep.as_str())));
    fields.extend(smoke_args.iter().map(|arg| ("smoke", arg.as_str())));
    fields.push(("pkgbuild", pkgbuild));
    fields
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

/// Consent to `pacman -U` has to be about a package: an empty file, a
/// signature or a download that is really an error page used to be staged as
/// "Install native Arch package" and only failed after the password prompt.
/// Without `bsdtar` there is nothing to check with, so the plan is not blocked.
fn verify_native_package(target: &str) -> Result<(), String> {
    let list = Step::new(
        "bsdtar",
        vec!["-tf", target],
        "Check the package is a readable archive",
        30,
    );
    let Ok(out) = run_step(&list, None) else {
        return Ok(());
    };
    if out.exit_code != 0 {
        let reason = out.stderr.lines().next().unwrap_or("unreadable").trim();
        return Err(format!(
            "'{target}' is not a readable Arch package: {reason}"
        ));
    }
    if out.stdout.trim().is_empty() {
        return Err(format!(
            "'{target}' is not a readable Arch package: the archive is empty"
        ));
    }
    if !out.stdout.lines().any(|line| line.trim() == ".PKGINFO") {
        return Err(format!(
            "'{target}' is an archive but has no .PKGINFO, so pacman would not accept it as a package"
        ));
    }
    Ok(())
}

/// The package name a build target implies when `--name` is not given: the
/// directory's own name (resolved, so `.`, `..` and `dir/` name the directory
/// rather than nothing), or the last path segment of a file or URL without its
/// archive/VCS suffix.
fn infer_build_name(target: &str) -> String {
    let path = Path::new(target);
    if path.is_dir() {
        if let Some(name) = path
            .canonicalize()
            .ok()
            .and_then(|real| real.file_name().map(|n| n.to_string_lossy().to_string()))
        {
            return name;
        }
    }
    let trimmed = target.split(['?', '#']).next().unwrap_or(target);
    let segment = trimmed
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(trimmed);
    let segment = segment
        .strip_suffix(".tar.gz")
        .or_else(|| segment.strip_suffix(".tgz"))
        .or_else(|| segment.strip_suffix(".git"))
        .unwrap_or(segment);
    segment.to_string()
}

/// Host of an `https://` URL, lower-cased, or `None` for any other scheme, for
/// credentials in the authority, or a malformed authority.
fn https_host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains('@') || authority.contains(char::is_whitespace) {
        return None;
    }
    let host = authority
        .rsplit_once(':')
        .filter(|(_, port)| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
        .map_or(authority, |(host, _)| host);
    if host.is_empty() {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

/// Why a configured host must not be fetched from: loopback, private and
/// link-local addresses and local names would turn a build into a request
/// against the user's own machine or network.
fn unsafe_fetch_host(host: &str) -> Option<&'static str> {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if bare == "localhost" || bare.ends_with(".localhost") || bare.ends_with(".local") {
        return Some("it names this machine or its local network");
    }
    if let Ok(ip) = bare.parse::<std::net::IpAddr>() {
        let private = match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback()
                    || v4.is_private()
                    || v4.is_link_local()
                    || v4.is_unspecified()
                    || v4.is_broadcast()
            }
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    || (v6.segments()[0] & 0xfe00) == 0xfc00
                    || (v6.segments()[0] & 0xffc0) == 0xfe80
            }
        };
        if private {
            return Some("it is a loopback, private or link-local address");
        }
    }
    None
}

fn generate_plan_id(action: &str, target: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(action.as_bytes());
    hasher.update(target.as_bytes());
    hasher.update(format!("{:?}", Instant::now()).as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    format!("{}-{}", action, &hash[..12])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remote_source_is_named_without_its_archive_or_vcs_suffix() {
        for (target, expected) in [
            ("https://github.com/o/tool", "tool"),
            ("https://github.com/o/tool/", "tool"),
            ("https://github.com/o/tool.git", "tool"),
            ("https://dl.example/x/tool-1.2.tar.gz", "tool-1.2"),
            ("https://dl.example/x/tool-1.2.tgz?token=a/b", "tool-1.2"),
            ("tool_1.0_amd64.deb", "tool_1.0_amd64.deb"),
        ] {
            assert_eq!(infer_build_name(target), expected, "{target}");
        }
    }

    fn binding(
        version: &str,
        entry: &str,
        deps: &[&str],
        smoke: &[&str],
        pkgbuild: &str,
    ) -> String {
        review_binding(&build_binding(
            "https://github.com/owner/tool",
            "tool",
            version,
            entry,
            &deps.iter().map(|d| d.to_string()).collect::<Vec<_>>(),
            &smoke.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            pkgbuild,
        ))
    }

    #[test]
    fn every_option_a_review_holds_is_part_of_what_its_id_is_made_from() {
        let base = binding("1.0.0", "tool", &[], &[], "pkgver='1.0.0'");
        // Two builds of one target that differ only in what they were asked to
        // build are different consents, and must not share a review binding.
        assert_ne!(base, binding("2.0.0", "tool", &[], &[], "pkgver='2.0.0'"));
        assert_ne!(base, binding("1.0.0", "other", &[], &[], "pkgver='1.0.0'"));
        assert_ne!(
            base,
            binding("1.0.0", "tool", &["glibc"], &[], "pkgver='1.0.0'")
        );
        assert_ne!(
            base,
            binding("1.0.0", "tool", &[], &["--help"], "pkgver='1.0.0'")
        );
        assert_ne!(base, binding("1.0.0", "tool", &[], &[], "pkgver='9.9.9'"));
    }

    #[test]
    fn a_binding_cannot_be_read_two_ways() {
        // Concatenating fields without a length lets ("ab", "c") and ("a", "bc")
        // look identical, which is how one review could be re-bound as another.
        let first = review_binding(&[("a", "ab"), ("b", "c")]);
        let second = review_binding(&[("a", "a"), ("b", "bc")]);
        assert_ne!(first, second);
        assert_eq!(
            first,
            review_binding(&[("a", "ab"), ("b", "c")]),
            "the same fields must bind the same, or a review cannot be recognised"
        );
    }

    #[test]
    fn a_target_or_token_that_is_not_one_clean_argument_is_refused() {
        for target in [
            "",
            "   ",
            "-rf",
            "--config=/tmp/evil.pkg.tar.zst",
            "a\u{0}b",
            "line1\nline2",
            &format!("/tmp/{}", "deep/".repeat(2000)),
        ] {
            assert!(
                validate_target(target).is_err(),
                "target {target:?} should be refused"
            );
        }
        for token in [
            "",
            " ",
            "-n",
            "usr/bin/tool",
            "two words",
            "new\nline",
            &"x".repeat(200),
        ] {
            assert!(
                validate_token(token, "Entry binary name").is_err(),
                "token {token:?} should be refused"
            );
        }
        assert!(validate_token("tool", "Entry binary name").is_ok());
        assert!(validate_token("WeChat", "Entry binary name").is_ok());
        assert!(validate_token("1:0.3.4-1", "Package version").is_ok());
    }

    #[test]
    fn smoke_arguments_are_the_programs_own_but_still_one_each() {
        assert!(validate_args(&["--version".to_string(), "-v".to_string()]).is_ok());
        assert!(validate_args(&["two words".to_string()]).is_ok());
        assert!(validate_args(&["bad\nnewline".to_string()]).is_err());
        assert!(validate_args(&vec!["x".to_string(); 33]).is_err());
        assert!(validate_args(&vec!["y".repeat(513); 1]).is_err());
    }
}

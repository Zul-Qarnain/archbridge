use crate::archive::{snapshot_directory, snapshot_tarball_bytes, FileReview};
use crate::build::{
    execute_chroot_build, run_runtime_smoke_test, snapshot_foreign_package, SmokeTestReport,
};
use crate::config::Config;
use crate::discovery::{search, DecisionReport};
use crate::inspect::inspect_package;
use crate::process::{run_step, Step};
use crate::recipe::{detect_build_system, generate_pkgbuild, BuildSystem, PkgbuildParams};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

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

impl Engine {
    pub fn new() -> Self {
        let config = Config::load().unwrap_or_default();
        Self {
            config,
            pending_plans: HashMap::new(),
        }
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

        self.pending_plans
            .insert(plan_id, (plan.clone(), Instant::now(), None));
        Ok(plan)
    }

    pub fn prepare_inspect(&mut self, file_path: &str) -> Result<Plan, String> {
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

        self.pending_plans
            .insert(plan_id, (plan.clone(), Instant::now(), None));
        Ok(plan)
    }

    pub fn prepare_config_set(&mut self, key: &str, value: &str) -> Result<Plan, String> {
        let mut test_config = self.config.clone();
        test_config.set(key, value)?;

        let plan_id = generate_plan_id("config.set", key);
        let summary = format!("Set configuration key '{}' to '{}'", key, value);

        let config_path = Config::config_path();

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

        self.pending_plans
            .insert(plan_id, (plan.clone(), Instant::now(), None));
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
        let entry = entry_opt.unwrap_or(name);
        let version = ver_opt.unwrap_or("1.0.0");

        let job_id = format!("job-{}", generate_plan_id("build", target));
        let job_dir = PathBuf::from(".archbridge").join("jobs").join(&job_id);

        let (snapshot, build_sys, pkgbuild) = if target.ends_with(".deb")
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

            let params = PkgbuildParams {
                name: sanitized_name,
                version: sanitized_ver,
                source_tarball: source_tar,
                sha256_hash: snap.sha256_hash.clone(),
                dependencies: deps.to_vec(),
                entry_binary: Some(entry.to_string()),
            };
            let pkgb = generate_pkgbuild(&BuildSystem::Imported, &params)?;
            (snap, BuildSystem::Imported, pkgb)
        } else if target.ends_with(".db") {
            return Err(
                "An Arch .db file is a repository index and contains no package payload to build. Download the corresponding .pkg.tar.* package instead.".to_string(),
            );
        } else if target.ends_with("PKGBUILD") {
            let pkgbuild_content = fs::read_to_string(target)
                .map_err(|e| format!("Failed to read PKGBUILD at '{}': {}", target, e))?;
            let parent_dir = Path::new(target).parent().unwrap_or_else(|| Path::new("."));
            let snap = snapshot_directory(parent_dir)?;
            (snap, BuildSystem::Make, pkgbuild_content)
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
            };

            let pkgb = match generate_pkgbuild(&bsys, &params) {
                Ok(p) => p,
                Err(err_msg) => {
                    let plan_id = generate_plan_id("build", target);
                    return Ok(Plan {
                        protocol_version: "v1.0".to_string(),
                        plan_id,
                        action: "build".to_string(),
                        summary: format!("Build plan for '{}': blocked", target),
                        steps: vec![],
                        filesystem_changes: vec![],
                        reviews: snap.reviews,
                        warnings: vec![],
                        decision: None,
                        downstream: None,
                        blocked: Some(err_msg),
                    });
                }
            };
            (snap, bsys, pkgb)
        } else if target.starts_with("https://github.com/")
            || target.starts_with("https://gitlab.com/")
        {
            let row = search(target, Some(target), &self.config);
            let mut tarball_url = None;
            for r in &row.decision_table {
                if r.source == "upstream" && r.availability == "available" {
                    if let Some(c) = r.candidates.first() {
                        tarball_url = c.url.clone();
                    }
                }
            }

            let url = tarball_url.ok_or_else(|| {
                format!(
                    "Could not resolve upstream release tarball for '{}'",
                    target
                )
            })?;
            let step = Step::new(
                "curl",
                vec!["-sSL", "-m", "30", &url],
                "Download upstream release tarball",
                30,
            );
            let out = run_step(&step, None)?;
            if out.exit_code != 0 {
                return Err(format!("Failed to download tarball from '{}'", url));
            }

            let bytes = out.stdout.as_bytes();
            let snap = snapshot_tarball_bytes(bytes)?;
            let bsys = BuildSystem::Cargo;
            let params = PkgbuildParams {
                name: name.to_string(),
                version: version.to_string(),
                source_tarball: "src.tar.gz".to_string(),
                sha256_hash: snap.sha256_hash.clone(),
                dependencies: deps.to_vec(),
                entry_binary: Some(entry.to_string()),
            };
            let pkgb = generate_pkgbuild(&bsys, &params)?;
            (snap, bsys, pkgb)
        } else {
            return Err(format!(
                "Build target '{}' is not a valid local path, PKGBUILD, or HTTPS URL",
                target
            ));
        };

        let plan_id = generate_plan_id("build", target);

        let mut steps = vec![
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
                "Initialize clean chroot environment",
                180,
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
                "Build package inside clean chroot",
                600,
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

        let plan = Plan {
            protocol_version: "v1.0".to_string(),
            plan_id: plan_id.clone(),
            action: "build".to_string(),
            summary: format!(
                "Clean chroot build for '{}' using detected build system '{:?}'",
                name, build_sys
            ),
            steps,
            filesystem_changes,
            reviews: snapshot.reviews,
            warnings: if run_smoke_test {
                vec!["Build will execute inside an isolated clean chroot container".to_string()]
            } else {
                vec![
                    "Foreign payload will be repackaged; runtime smoke test requires an explicit entry binary"
                        .to_string(),
                ]
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

        self.pending_plans
            .insert(plan_id, (plan.clone(), Instant::now(), Some(staged)));
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
                vec!["pacman", "-U", "--needed", target],
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
            self.pending_plans
                .insert(plan_id, (plan.clone(), Instant::now(), None));
            return Ok(plan);
        }

        let report = search(target, None, &self.config);
        let rec = report.recommended_source.clone();

        match rec.as_deref() {
            Some("official") => {
                let plan_id = generate_plan_id("install", target);
                let steps = vec![Step::new(
                    "sudo",
                    vec!["pacman", "-S", "--needed", "--noconfirm", target],
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
                self.pending_plans
                    .insert(plan_id, (plan.clone(), Instant::now(), None));
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
            summary: format!("Remove installed package '{}' with pacman", package),
            steps: vec![Step::new(
                "sudo",
                vec!["pacman", "-Rns", "--noconfirm", package],
                "Remove selected package and unused dependencies",
                120,
            )],
            filesystem_changes: vec![FilesystemChange {
                path: "/".to_string(),
                action: "modify".to_string(),
                description: format!("Remove package '{}' from the live system", package),
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
        self.pending_plans
            .insert(plan_id, (plan.clone(), Instant::now(), None));
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

        self.pending_plans
            .insert(plan_id, (plan.clone(), Instant::now(), None));
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

        let (plan, _created, staged_opt) = self
            .pending_plans
            .remove(plan_id)
            .ok_or_else(|| format!("Plan ID '{}' not found or expired", plan_id))?;

        if let Some(blocked_msg) = &plan.blocked {
            return Err(format!("Cannot execute blocked plan: {}", blocked_msg));
        }

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
                if let Some(change) = plan.filesystem_changes.first() {
                    let desc = &change.description;
                    if let Some((k, v)) = desc
                        .strip_prefix("Update configuration key '")
                        .and_then(|s| s.split_once("' = '"))
                    {
                        let val = v.trim_end_matches('\'');
                        self.config.set(k, val)?;
                        self.config.save()?;
                    }
                }
                Ok(ExecutionResult {
                    ok: true,
                    plan_id: plan_id.to_string(),
                    message: "Configuration updated and saved".to_string(),
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

                    if staged.run_smoke_test {
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
                        summary: format!("Install built package '{:?}'", art_path),
                        steps: vec![Step::new(
                            "sudo",
                            vec![
                                "pacman".to_string(),
                                "-U".to_string(),
                                "--needed".to_string(),
                                "--noconfirm".to_string(),
                                art_path.to_str().unwrap().to_string(),
                            ],
                            "Install package to host",
                            120,
                        )],
                        filesystem_changes: vec![FilesystemChange {
                            path: "/".to_string(),
                            action: "modify".to_string(),
                            description: format!("Install package '{:?}' to host system", art_path),
                        }],
                        reviews: vec![],
                        warnings: vec!["Modifies system packages on host".to_string()],
                        decision: None,
                        downstream: None,
                        blocked: None,
                    };

                    self.pending_plans
                        .insert(install_plan_id, (next_plan.clone(), Instant::now(), None));

                    Ok(ExecutionResult {
                        ok: true,
                        plan_id: plan_id.to_string(),
                        message: if staged.run_smoke_test {
                            format!("Build and smoke test passed for '{:?}'", art_path)
                        } else {
                            format!("Package repackaged successfully at '{:?}'", art_path)
                        },
                        output: Some(format!("Artifact produced at '{:?}'", art_path)),
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

fn generate_plan_id(action: &str, target: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(action.as_bytes());
    hasher.update(target.as_bytes());
    hasher.update(format!("{:?}", Instant::now()).as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    format!("{}-{}", action, &hash[..12])
}

use crate::archive::{FileReview, SourceSnapshot};
use crate::process::{run_step, run_step_in_dir, Step};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Extract only the payload of a foreign package into a reviewable snapshot.
/// Package maintainer scripts are never executed. The snapshot is later packed
/// by an Arch PKGBUILD inside the clean chroot.
pub fn snapshot_foreign_package(path: &Path) -> Result<SourceSnapshot, String> {
    let name = path.to_string_lossy().to_lowercase();
    if name.ends_with(".db") {
        return Err(
            "An Arch .db file is a repository index, not a package payload. Provide the matching .pkg.tar.* file or a .deb/.rpm package.".to_string(),
        );
    }
    if !name.ends_with(".deb") && !name.ends_with(".rpm") {
        return Err(format!(
            "Unsupported import format '{}'; expected .deb or .rpm",
            path.display()
        ));
    }
    if !path.is_file() {
        return Err(format!(
            "Foreign package does not exist: '{}'",
            path.display()
        ));
    }

    let temp_dir = TempDir::new().map_err(|e| format!("Failed to create import workspace: {e}"))?;

    let (tarball_path, _tarball_name) = if name.ends_with(".deb") {
        let step_ar = Step::new(
            "bsdtar",
            vec![
                "-xf".to_string(),
                path.display().to_string(),
                "-C".to_string(),
                temp_dir.path().display().to_string(),
            ],
            "Extract DEB payload archive",
            60,
        );
        let out = run_step(&step_ar, None).map_err(|e| format!("bsdtar execution failed: {e}"))?;
        if out.exit_code != 0 {
            return Err(format!("Failed to unpack DEB archive: {}", out.stderr));
        }

        let mut found = None;
        if let Ok(entries) = fs::read_dir(temp_dir.path()) {
            for entry in entries.flatten() {
                let n = entry.file_name().to_string_lossy().to_string();
                if n.starts_with("data.tar") {
                    found = Some((entry.path(), n));
                    break;
                }
            }
        }
        found.ok_or_else(|| {
            format!(
                "DEB '{}' does not contain a data.tar.* payload",
                path.display()
            )
        })?
    } else {
        let payload_dir = temp_dir.path().join("payload");
        fs::create_dir_all(&payload_dir).map_err(|e| e.to_string())?;
        let step_ext = Step::new(
            "bsdtar",
            vec![
                "-xf".to_string(),
                path.display().to_string(),
                "-C".to_string(),
                payload_dir.display().to_string(),
            ],
            "Extract RPM payload",
            60,
        );
        let out = run_step(&step_ext, None).map_err(|e| format!("bsdtar execution failed: {e}"))?;
        if out.exit_code != 0 {
            return Err(format!("RPM payload extraction failed: {}", out.stderr));
        }

        let out_tar = temp_dir.path().join("src.tar.gz");
        let step_pack = Step::new(
            "bsdtar",
            vec![
                "-czf".to_string(),
                out_tar.display().to_string(),
                "-C".to_string(),
                payload_dir.display().to_string(),
                ".".to_string(),
            ],
            "Pack RPM payload into source tarball",
            60,
        );
        let out_pack =
            run_step(&step_pack, None).map_err(|e| format!("bsdtar pack failed: {e}"))?;
        if out_pack.exit_code != 0 {
            return Err(format!(
                "RPM payload compression failed: {}",
                out_pack.stderr
            ));
        }
        (out_tar, "src.tar.gz".to_string())
    };

    let tarball_bytes =
        fs::read(&tarball_path).map_err(|e| format!("Failed to read payload tarball: {e}"))?;
    let mut hasher = Sha256::new();
    hasher.update(&tarball_bytes);
    let sha256_hash = format!("{:x}", hasher.finalize());

    let mut reviews = Vec::new();
    let step_list = Step::new(
        "bsdtar",
        vec!["-tvf".to_string(), tarball_path.display().to_string()],
        "List payload archive contents",
        30,
    );
    if let Ok(out) = run_step(&step_list, None) {
        for line in out.stdout.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 6 {
                let mode_str = parts[0];
                let is_exec = mode_str.contains('x');
                let size = parts[4].parse::<u64>().unwrap_or(0);
                let path_str = parts[5..].join(" ");
                let mut h = Sha256::new();
                h.update(path_str.as_bytes());
                let p_hash = format!("{:x}", h.finalize());
                reviews.push(FileReview {
                    path: path_str,
                    sha256: p_hash,
                    bytes: size,
                    executable: is_exec,
                    content: None,
                });
            }
        }
    }

    Ok(SourceSnapshot {
        tarball_bytes,
        sha256_hash,
        reviews,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SmokeConfig {
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SmokeTestReport {
    pub passed: bool,
    pub entry_point: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub fn package_files(bytes: &[u8]) -> Result<Vec<FileReview>, String> {
    let mut reviews = Vec::new();
    let gz_decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(gz_decoder);

    if let Ok(entries) = archive.entries() {
        for entry_res in entries.flatten() {
            let path_str = entry_res
                .path()
                .map_or("unknown".to_string(), |p| p.to_string_lossy().to_string());
            let size = entry_res.header().size().unwrap_or(0);
            let mode = entry_res.header().mode().unwrap_or(0);
            let is_exec = (mode & 0o111) != 0;

            let mut hasher = Sha256::new();
            hasher.update(path_str.as_bytes());
            let sha256 = format!("{:x}", hasher.finalize());

            reviews.push(FileReview {
                path: path_str,
                sha256,
                bytes: size,
                executable: is_exec,
                content: None,
            });
        }
    }
    Ok(reviews)
}

pub fn execute_chroot_build(
    job_dir: &Path,
    pkgbuild_content: &str,
    source_tarball: &[u8],
) -> Result<PathBuf, String> {
    fs::create_dir_all(job_dir).map_err(|e| format!("Failed to create job dir: {}", e))?;
    let pkgbuild_path = job_dir.join("PKGBUILD");
    fs::write(&pkgbuild_path, pkgbuild_content)
        .map_err(|e| format!("Failed to write PKGBUILD: {}", e))?;

    let source_name = if pkgbuild_content.contains("source=('src.tar.xz')") {
        "src.tar.xz"
    } else if pkgbuild_content.contains("source=('src.tar.zst')") {
        "src.tar.zst"
    } else {
        "src.tar.gz"
    };
    let src_tar_path = job_dir.join(source_name);
    fs::write(&src_tar_path, source_tarball)
        .map_err(|e| format!("Failed to write source tarball: {}", e))?;

    // 1. First attempt: unprivileged native makepkg (fast, safe, no root/sudo needed)
    let makepkg_step = Step::new(
        "makepkg",
        vec!["--force".to_string(), "--clean".to_string()],
        "Build package with native makepkg",
        300,
    );
    let _ = run_step_in_dir(&makepkg_step, None, Some(job_dir));

    if let Ok(entries) = fs::read_dir(job_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if let Some(ext) = p.extension() {
                if ext == "zst" || p.to_string_lossy().contains(".pkg.tar.") {
                    return Ok(p);
                }
            }
        }
    }

    // 2. Fallback: clean chroot build with mkarchroot and makechrootpkg
    let chroot_dir = job_dir.join("chroot");
    fs::create_dir_all(&chroot_dir).map_err(|e| e.to_string())?;
    let job_dir_abs = fs::canonicalize(job_dir).map_err(|e| {
        format!(
            "Failed to resolve build workspace '{}': {e}",
            job_dir.display()
        )
    })?;
    let chroot_dir_abs = job_dir_abs.join("chroot");
    let chroot_root_abs = chroot_dir_abs.join("root");

    // Create clean chroot with mkarchroot
    let mkchroot_step = Step::new(
        "sudo",
        vec![
            "-n".to_string(),
            "mkarchroot".to_string(),
            "-C".to_string(),
            "/etc/pacman.conf".to_string(),
            chroot_root_abs.to_str().unwrap().to_string(),
            "base".to_string(),
            "base-devel".to_string(),
            "sudo".to_string(),
        ],
        "Initialize clean chroot root",
        180,
    );

    let mkchroot_result = run_step(&mkchroot_step, None)
        .map_err(|e| format!("Clean chroot initialization failed: {e}"))?;
    if mkchroot_result.exit_code != 0 {
        return Err(format!(
            "Clean chroot initialization failed: {}{}",
            mkchroot_result.stdout, mkchroot_result.stderr
        ));
    }

    // Run makepkg inside chroot with makechrootpkg
    let makechroot_step = Step::new(
        "sudo",
        vec![
            "-n".to_string(),
            "makechrootpkg".to_string(),
            "-r".to_string(),
            chroot_dir_abs.to_str().unwrap().to_string(),
            "--".to_string(),
            "--syncdeps".to_string(),
            "--noconfirm".to_string(),
            "--cleanbuild".to_string(),
            "--clean".to_string(),
            "--force".to_string(),
        ],
        "Build package inside clean chroot",
        600,
    );

    let res = run_step_in_dir(&makechroot_step, None, Some(job_dir));

    let mut artifact = None;
    if let Ok(entries) = fs::read_dir(job_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if let Some(ext) = p.extension() {
                if ext == "zst" || p.to_string_lossy().contains(".pkg.tar.") {
                    artifact = Some(p);
                    break;
                }
            }
        }
    }

    if let Some(art) = artifact {
        Ok(art)
    } else {
        let err_msg = res.map_or("Build failed with no output".to_string(), |o| {
            format!("Build output: {}\n{}", o.stdout, o.stderr)
        });
        Err(format!(
            "Package build failed to produce artifact. {}",
            err_msg
        ))
    }
}

pub fn run_runtime_smoke_test(
    pkg_path: &Path,
    entry_point: &str,
    smoke_args: &[String],
) -> Result<SmokeTestReport, String> {
    if !pkg_path.exists() {
        return Err(format!("Package artifact not found at '{:?}'", pkg_path));
    }

    let bytes =
        fs::read(pkg_path).map_err(|e| format!("Failed to read package artifact: {}", e))?;
    let _file_reviews = package_files(&bytes)?;

    let temp_dir = TempDir::new().map_err(|e| format!("Failed to create temp test root: {}", e))?;
    let test_root = temp_dir.path().join("test_root");
    fs::create_dir_all(&test_root).map_err(|e| e.to_string())?;

    let mut smoke = SmokeConfig { timeout_seconds: 0 };
    smoke.timeout_seconds = 30;

    let mut args = vec![
        "--private-users=pick".to_string(),
        "--private-network".to_string(),
        "--user".to_string(),
        "65534".to_string(),
        "-D".to_string(),
        test_root.to_str().unwrap().to_string(),
        entry_point.to_string(),
    ];

    if smoke_args.is_empty() {
        args.push("--version".to_string());
    } else {
        args.extend(smoke_args.iter().cloned());
    }

    let step = Step::new(
        "systemd-nspawn",
        args,
        "Execute runtime smoke test",
        smoke.timeout_seconds,
    );

    match run_step(&step, None) {
        Ok(out) => Ok(SmokeTestReport {
            passed: out.exit_code == 0,
            entry_point: entry_point.to_string(),
            exit_code: out.exit_code,
            stdout: out.stdout,
            stderr: out.stderr,
        }),
        Err(e) => Ok(SmokeTestReport {
            passed: false,
            entry_point: entry_point.to_string(),
            exit_code: -1,
            stdout: String::new(),
            stderr: format!("Runtime test execution failed: {}", e),
        }),
    }
}

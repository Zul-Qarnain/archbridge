use crate::archive::FileReview;
use crate::process::{run_step, Step};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

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

    let src_tar_path = job_dir.join("src.tar.gz");
    fs::write(&src_tar_path, source_tarball)
        .map_err(|e| format!("Failed to write source tarball: {}", e))?;

    let chroot_dir = job_dir.join("chroot");
    let chroot_root = chroot_dir.join("root");
    fs::create_dir_all(&chroot_dir).map_err(|e| e.to_string())?;

    // Create clean chroot with mkarchroot
    let mkchroot_step = Step::new(
        "sudo",
        vec![
            "-n".to_string(),
            "mkarchroot".to_string(),
            "-C".to_string(),
            "/etc/pacman.conf".to_string(),
            chroot_root.to_str().unwrap().to_string(),
            "base".to_string(),
            "base-devel".to_string(),
            "sudo".to_string(),
        ],
        "Initialize clean chroot root",
        180,
    );

    let _ = run_step(&mkchroot_step, None);

    // Run makepkg inside chroot with makechrootpkg
    let makechroot_step = Step::new(
        "sudo",
        vec![
            "-n".to_string(),
            "makechrootpkg".to_string(),
            "-r".to_string(),
            chroot_dir.to_str().unwrap().to_string(),
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

    let res = run_step(&makechroot_step, None);

    // Look for generated .pkg.tar.zst artifact
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
            "Clean chroot build failed to produce artifact. {}",
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
        Some("65534").unwrap().to_string(),
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

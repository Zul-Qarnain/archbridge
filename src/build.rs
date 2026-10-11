use crate::archive::{walk_tree, FileReview, SourceSnapshot};
use crate::process::{run_step, run_step_in_dir, Step};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// The file name a payload has to be staged under so that the extractor a
/// generated PKGBUILD chooses actually reads it. The extension is a claim about
/// compression, so it comes from the bytes and not from the container: a DEB can
/// carry an uncompressed `data.tar`, a bzip2 one, or zstd, and naming any of
/// those `src.tar.gz` points `tar -xzf` at a stream it cannot decode.
pub fn source_tarball_name(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        return "src.tar.zst".to_string();
    }
    if bytes.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0x00]) {
        return "src.tar.xz".to_string();
    }
    if bytes.starts_with(&[0x1F, 0x8B]) {
        return "src.tar.gz".to_string();
    }
    // Everything else — plain tar, bzip2, lzma — goes to the extension-less
    // name, which the generated PKGBUILD hands to `bsdtar -xf` for libarchive to
    // recognise.
    "src.tar".to_string()
}

/// The local file name the reviewed PKGBUILD expects its first source at. The
/// staged bytes have to land exactly there, or `sha256sums` verifies a file
/// makepkg never opens: the script the user confirmed is the authority on the
/// name, not a fixed list of possibilities.
fn staged_source_name(pkgbuild: &str) -> Option<String> {
    let list = pkgbuild.split_once("source=(")?.1.split_once(')')?.0;
    let element = list.split_whitespace().next()?;
    let element = element.trim_matches(|c| c == '\'' || c == '"');
    // makepkg's `URL::name` form names the file `name`; a bare entry is itself.
    let local = element.rsplit("::").next().unwrap_or(element);
    let usable = !local.is_empty()
        && local.len() <= 200
        && !local.starts_with('.')
        && local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'));
    usable.then(|| local.to_string())
}

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
    let payload_dir = temp_dir.path().join("payload");

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

    // The manifest a user reviews has to state real paths, sizes and content
    // digests. Scraping them out of a `bsdtar -tvf` listing produced file names
    // with timestamps glued in front, sizes parsed from a date column, and
    // digests of the name string, which verify nothing at all.
    if name.ends_with(".deb") {
        fs::create_dir_all(&payload_dir).map_err(|e| e.to_string())?;
        let step_unpack = Step::new(
            "bsdtar",
            vec![
                "-xf".to_string(),
                tarball_path.display().to_string(),
                "-C".to_string(),
                payload_dir.display().to_string(),
            ],
            "Unpack foreign payload for review",
            120,
        );
        let out =
            run_step(&step_unpack, None).map_err(|e| format!("bsdtar execution failed: {e}"))?;
        if out.exit_code != 0 {
            return Err(format!(
                "Failed to unpack payload of '{}': {}",
                path.display(),
                out.stderr
            ));
        }
    }
    let reviews = walk_tree(&payload_dir)?;

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

/// Unpack an Arch package into `dest` with the system `bsdtar`, which handles
/// whatever compression the artifact uses.
fn unpack_to(pkg: &Path, dest: &Path, purpose: &str) -> Result<(), String> {
    let step = Step::new(
        "bsdtar",
        vec![
            "-xf".to_string(),
            pkg.display().to_string(),
            "-C".to_string(),
            dest.display().to_string(),
        ],
        purpose,
        120,
    );
    let out = run_step(&step, None).map_err(|e| format!("bsdtar execution failed: {e}"))?;
    if out.exit_code != 0 {
        return Err(format!(
            "Failed to unpack '{}': {}",
            pkg.display(),
            out.stderr.trim()
        ));
    }
    Ok(())
}

/// The files a built package contains, each hashed over its own bytes.
pub fn package_files(bytes: &[u8]) -> Result<Vec<FileReview>, String> {
    let temp_dir = TempDir::new().map_err(|e| format!("Failed to create review workspace: {e}"))?;
    let staged = temp_dir.path().join("package.pkg");
    fs::write(&staged, bytes).map_err(|e| format!("Failed to stage package bytes: {e}"))?;
    let root = temp_dir.path().join("root");
    fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    unpack_to(&staged, &root, "Unpack built package for review")?;
    walk_tree(&root)
}

/// Map the entry binary a plan names onto the path the container will exec.
pub fn resolve_entry(reviews: &[FileReview], entry: &str) -> Result<String, String> {
    // The entry names one file, not a location. `../demo` and `/usr/bin/demo`
    // both have a file name, and matching on it would run — and then report as
    // tested — a program the plan never named.
    if entry.is_empty()
        || entry.contains('/')
        || entry == "."
        || entry == ".."
        || entry.len() > 256
        || entry.contains(char::is_control)
    {
        return Err(format!(
            "Entry binary '{entry}' is not a single file name; pass 'demo', not a path."
        ));
    }

    let mut matches: Vec<&FileReview> = reviews
        .iter()
        .filter(|review| {
            Path::new(&review.path)
                .file_name()
                .map(|name| name == entry)
                .unwrap_or(false)
        })
        .collect();
    matches.sort_by_key(|review| {
        (
            !review.path.starts_with("usr/bin/"),
            review.path.split('/').count(),
        )
    });
    let chosen = matches.first().ok_or_else(|| {
        let shipped: Vec<&str> = reviews
            .iter()
            .filter(|review| review.path.starts_with("usr/bin/"))
            .map(|review| review.path.as_str())
            .take(8)
            .collect();
        format!(
            "Entry binary '{}' is not part of the built package (it ships {:?})",
            entry, shipped
        )
    })?;

    // A launcher shipped as a link is normal, and fine while what it points at
    // came with the package. A link out to a host path resolves to nothing in a
    // container root that holds this package alone, and the run would report a
    // failure that says nothing about the package.
    if let Some(target) = chosen
        .content
        .as_deref()
        .and_then(|text| text.strip_prefix("symlink -> "))
    {
        if target.starts_with('/') {
            let inside = target.trim_start_matches('/');
            if !reviews.iter().any(|review| review.path == inside) {
                return Err(format!(
                    "Entry binary '{}' links to '{target}', which the built package does not \
                     contain, so the isolated test root has nothing to execute.",
                    chosen.path
                ));
            }
        }
    }

    Ok(format!("/{}", chosen.path))
}

/// The staged source payload lives in the same directory as the build output and
/// can itself be zstd-compressed, so only a real `.pkg.tar.*` archive counts as
/// an artifact. Picking the freshest match avoids returning one from an earlier run.
/// The path is absolute because the plan that consumes it may be reviewed later.
/// Append one phase of toolchain output to the job's build log. Logging is
/// best-effort: a failure to write must not fail an otherwise good build.
fn write_build_log(job_dir: &Path, phase: &str, log: &str) {
    let path = job_dir.join("build.log");
    let mut text = fs::read_to_string(&path).unwrap_or_default();
    text.push_str(&format!("==> {phase}\n{log}\n\n"));
    let _ = fs::write(&path, text);
}

fn find_built_package(dir: &Path) -> Option<PathBuf> {
    let mut matches: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().contains(".pkg.tar."))
                .unwrap_or(false)
        })
        .filter_map(|path| {
            let modified = path.metadata().and_then(|m| m.modified()).ok()?;
            Some((modified, path))
        })
        .collect();
    matches.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    matches
        .into_iter()
        .next()
        .map(|(_, path)| fs::canonicalize(&path).unwrap_or(path))
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

    let source_name =
        staged_source_name(pkgbuild_content).unwrap_or_else(|| "src.tar.gz".to_string());
    let src_tar_path = job_dir.join(&source_name);
    fs::write(&src_tar_path, source_tarball)
        .map_err(|e| format!("Failed to write source tarball: {}", e))?;

    // 1. First attempt: unprivileged native makepkg (fast, safe, no root/sudo needed)
    let makepkg_step = Step::new(
        "makepkg",
        vec!["--force".to_string(), "--clean".to_string()],
        "Build package with native makepkg",
        900,
    );
    let native = run_step_in_dir(&makepkg_step, None, Some(job_dir));

    // A native build needs no privilege at all, so keep its diagnostics: they
    // explain the failure when the privileged chroot fallback also gives up.
    let native_log = match native {
        Ok(out) => format!("{}{}", out.stdout, out.stderr),
        Err(err) => err,
    };
    // The caller only ever sees the last error string, and a UI can lose even
    // that, so the toolchain output stays on disk next to the PKGBUILD.
    write_build_log(job_dir, "native makepkg", &native_log);

    if let Some(artifact) = find_built_package(job_dir) {
        return Ok(artifact);
    }

    // Native build failed: the chroot fallback needs two more steps.
    crate::process::set_phase_total(3);

    // 2. Fallback: clean chroot build with mkarchroot and makechrootpkg
    let chroot_dir = job_dir.join("chroot");
    fs::create_dir_all(&chroot_dir).map_err(|e| e.to_string())?;
    let job_dir_abs = fs::canonicalize(job_dir).map_err(|e| {
        format!(
            "Failed to resolve build workspace '{}': {e}",
            job_dir.display()
        )
    })?;
    // These paths go into an argument vector, and a working directory is only
    // required to be valid OS bytes: a non-UTF-8 directory name is somebody's
    // home directory, not a reason to panic the build.
    let chroot_dir_abs = job_dir_abs.join("chroot");
    let chroot_root_abs = chroot_dir_abs.join("root");
    let chroot_dir_arg = chroot_dir_abs
        .to_str()
        .ok_or_else(|| "The resolved chroot directory is not valid UTF-8".to_string())?
        .to_string();
    let chroot_root_arg = chroot_root_abs
        .to_str()
        .ok_or_else(|| "The resolved chroot root is not valid UTF-8".to_string())?
        .to_string();

    // Create clean chroot with mkarchroot
    let mkchroot_step = Step::new(
        "sudo",
        vec![
            "-n".to_string(),
            "mkarchroot".to_string(),
            "-C".to_string(),
            "/etc/pacman.conf".to_string(),
            chroot_root_arg,
            "base".to_string(),
            "base-devel".to_string(),
            "sudo".to_string(),
        ],
        "Initialize clean chroot environment (fallback)",
        900,
    );

    let mkchroot_result = run_step(&mkchroot_step, None)
        .map_err(|e| format!("Clean chroot initialization failed: {e}"))?;
    if mkchroot_result.exit_code != 0 {
        let chroot_log = format!("{}{}", mkchroot_result.stdout, mkchroot_result.stderr);
        write_build_log(job_dir, "mkarchroot", &chroot_log);
        let needs_auth = chroot_log.contains("password is required");
        return Err(format!(
            "Clean chroot initialization failed: {}{}\
             \n\nNative (unprivileged) build attempted first and did not produce a package:\n{}",
            if needs_auth {
                "No cached administrator credential — authorize the build and retry. "
            } else {
                ""
            },
            chroot_log.trim(),
            native_log.trim()
        ));
    }

    // Run makepkg inside chroot with makechrootpkg
    let makechroot_step = Step::new(
        "sudo",
        vec![
            "-n".to_string(),
            "makechrootpkg".to_string(),
            "-r".to_string(),
            chroot_dir_arg,
            "--".to_string(),
            "--syncdeps".to_string(),
            "--noconfirm".to_string(),
            "--cleanbuild".to_string(),
            "--clean".to_string(),
            "--force".to_string(),
        ],
        "Build package inside clean chroot (fallback)",
        1200,
    );

    let res = run_step_in_dir(&makechroot_step, None, Some(job_dir));

    if let Some(artifact) = find_built_package(job_dir) {
        return Ok(artifact);
    }

    // A step that could not be run at all (timeout, missing binary) carries its
    // own reason; dropping it in favour of a generic string made every chroot
    // failure read as "no output".
    let err_msg = match res {
        Ok(o) => format!("Build output: {}\n{}", o.stdout, o.stderr),
        Err(e) => e,
    };
    write_build_log(job_dir, "makechrootpkg", &err_msg);
    Err(format!(
        "Package build failed to produce a .pkg.tar.* artifact. {}\n\nNative (unprivileged) build output:\n{}",
        err_msg,
        native_log.trim()
    ))
}

pub fn run_runtime_smoke_test(
    pkg_path: &Path,
    entry_point: &str,
    smoke_args: &[String],
) -> Result<SmokeTestReport, String> {
    if !pkg_path.exists() {
        return Err(format!(
            "Package artifact not found at {}",
            pkg_path.display()
        ));
    }

    let temp_dir = TempDir::new().map_err(|e| format!("Failed to create temp test root: {}", e))?;
    let test_root = temp_dir.path().join("test_root");
    fs::create_dir_all(&test_root).map_err(|e| e.to_string())?;
    // A root that holds nothing can never start the binary under test, so the
    // package is installed into it and the entry point is resolved to the path
    // the container will actually exec.
    unpack_to(
        pkg_path,
        &test_root,
        "Install package into isolated test root",
    )?;
    let reviews = walk_tree(&test_root)?;
    let program = resolve_entry(&reviews, entry_point)?;
    if !reviews.iter().any(|review| {
        review.path.starts_with("usr/lib/ld-linux") || review.path.starts_with("lib/ld-linux")
    }) {
        // The test root is this package alone: no dynamic loader, no system
        // libraries. Running the binary would fail for reasons that say nothing
        // about the package, so the finding is reported instead.
        return Ok(SmokeTestReport {
            passed: false,
            entry_point: program,
            exit_code: -1,
            stdout: String::new(),
            stderr: format!(
                "The isolated test root contains only '{}' and its own files, so the dynamic \
                 loader and system libraries '{}' needs are absent. Install the package, or run \
                 the smoke test inside a full Arch root, to check runtime behaviour.",
                pkg_path.display(),
                entry_point
            ),
        });
    }

    let mut smoke = SmokeConfig { timeout_seconds: 0 };
    smoke.timeout_seconds = 30;

    // The root is `TMPDIR`-derived, so its bytes are only guaranteed to be a
    // path; naming it in an argument vector needs UTF-8 or a plain refusal.
    let test_root_arg = test_root
        .to_str()
        .ok_or_else(|| "The isolated test root is not at a UTF-8 path".to_string())?;

    let mut args = vec![
        "--private-users=pick".to_string(),
        "--private-network".to_string(),
        "--user".to_string(),
        "65534".to_string(),
        "-D".to_string(),
        test_root_arg.to_string(),
        program.clone(),
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
            entry_point: program.clone(),
            exit_code: out.exit_code,
            stdout: out.stdout,
            stderr: out.stderr,
        }),
        Err(e) => Ok(SmokeTestReport {
            passed: false,
            entry_point: program.clone(),
            exit_code: -1,
            stdout: String::new(),
            stderr: format!("Runtime test execution failed: {}", e),
        }),
    }
}

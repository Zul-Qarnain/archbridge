use crate::elf::{needed_libraries_in_file, MAX_ELF_FILES};
use crate::process::{run_step, Step};
use crate::rpm::{self, Scriptlet};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScriptInfo {
    pub name: String,
    pub content: String,
    pub executed: bool,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InspectionReport {
    pub target_file: String,
    pub format: String,
    pub package_name: String,
    pub version: String,
    pub architecture: String,
    pub description: String,
    pub maintainer: String,
    pub dependencies: Vec<String>,
    /// Capability names the package declares, so a reviewer can see what
    /// installing it would claim to satisfy.
    pub provides: Vec<String>,
    pub maintainer_scripts: Vec<ScriptInfo>,
    pub desktop_files: Vec<String>,
    pub systemd_units: Vec<String>,
    pub needed_libraries: Vec<String>,
    pub compatibility_note: String,
    /// Identity of the exact bytes reviewed, so a later build can be checked
    /// against this inspection. The payload itself is never carried in the
    /// report: serializing it turned a 200 MB package into a ~1 GB JSON string.
    pub size_bytes: u64,
    pub sha256: String,
}

pub fn inspect_package(file_path: &str) -> Result<InspectionReport, String> {
    let path = Path::new(file_path);
    if !path.exists() {
        return Err(format!("File not found: '{}'", file_path));
    }

    let bytes = fs::read(path).map_err(|e| format!("Failed to read file '{:?}': {}", path, e))?;
    let size_bytes = bytes.len() as u64;
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    drop(bytes);

    let file_str = file_path.to_lowercase();
    if file_str.ends_with(".deb") {
        inspect_deb(file_path, size_bytes, sha256)
    } else if file_str.ends_with(".rpm") {
        inspect_rpm(file_path, size_bytes, sha256)
    } else {
        Err(format!(
            "Unsupported foreign package format for file '{}'",
            file_path
        ))
    }
}

fn inspect_deb(
    file_path: &str,
    size_bytes: u64,
    sha256: String,
) -> Result<InspectionReport, String> {
    let temp_dir = TempDir::new().map_err(|e| format!("Failed to create temp directory: {}", e))?;
    let control_dir = temp_dir.path().join("control");
    fs::create_dir_all(&control_dir).map_err(|e| e.to_string())?;

    // Unpack deb archive members (control.tar.* and debian-binary) using bsdtar
    let step_ar = Step::new(
        "bsdtar",
        vec![
            "-xf".to_string(),
            file_path.to_string(),
            "-C".to_string(),
            temp_dir.path().to_str().unwrap().to_string(),
        ],
        "Unpack DEB archive members",
        30,
    );
    let _ = run_step(&step_ar, None);

    // Locate control.tar.* archive (supports .gz, .xz, .zst)
    let mut control_tar = None;
    let mut data_tar = None;
    if let Ok(entries) = fs::read_dir(temp_dir.path()) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("control.tar") {
                control_tar = Some(entry.path());
            } else if name.starts_with("data.tar") {
                data_tar = Some(entry.path());
            }
        }
    }

    if let Some(c_tar) = control_tar {
        let step_ctrl = Step::new(
            "bsdtar",
            vec![
                "-xf".to_string(),
                c_tar.to_str().unwrap().to_string(),
                "-C".to_string(),
                control_dir.to_str().unwrap().to_string(),
            ],
            "Extract DEB control files",
            30,
        );
        let _ = run_step(&step_ctrl, None);
    }

    // Read control file
    let mut pkg_name = "unknown".to_string();
    let mut version = "unknown".to_string();
    let mut architecture = "unknown".to_string();
    let mut description = String::new();
    let mut maintainer = String::new();
    let mut dependencies = Vec::new();
    let mut provides = Vec::new();

    let control_file = control_dir.join("control");
    if control_file.exists() {
        if let Ok(content) = fs::read_to_string(&control_file) {
            for line in content.lines() {
                if let Some((k, v)) = line.split_once(':') {
                    let key = k.trim();
                    let val = v.trim();
                    match key {
                        "Package" => pkg_name = val.to_string(),
                        "Version" => version = val.to_string(),
                        "Architecture" => architecture = val.to_string(),
                        "Description" => description = val.to_string(),
                        "Maintainer" => maintainer = val.to_string(),
                        "Depends" | "Pre-Depends" => {
                            for dep in val.split(',') {
                                let trimmed = dep.trim();
                                if !trimmed.is_empty() {
                                    dependencies.push(trimmed.to_string());
                                }
                            }
                        }
                        "Provides" => {
                            for capability in val.split(',') {
                                let trimmed = capability.trim();
                                if !trimmed.is_empty() {
                                    provides.push(trimmed.to_string());
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // Inspect maintainer scripts (preinst, postinst, prerm, postrm) - never executed
    let script_names = ["preinst", "postinst", "prerm", "postrm"];
    let mut maintainer_scripts = Vec::new();
    for script_name in &script_names {
        let s_path = control_dir.join(script_name);
        if s_path.exists() {
            let content = fs::read_to_string(&s_path)
                .unwrap_or_else(|_| "<binary or unreadable>".to_string());
            let is_utf8 = !content.contains('\0');
            maintainer_scripts.push(ScriptInfo {
                name: script_name.to_string(),
                content,
                executed: false,
                status: if is_utf8 {
                    "readable_shell".to_string()
                } else {
                    "unsupported".to_string()
                },
            });
        }
    }

    // Scan payload for desktop files, systemd units, and binaries
    let mut desktop_files = Vec::new();
    let mut systemd_units = Vec::new();
    let mut payload_members = Vec::new();

    if let Some(d_tar) = &data_tar {
        let step_list = Step::new(
            "bsdtar",
            vec!["-tf".to_string(), d_tar.display().to_string()],
            "List DEB payload files",
            30,
        );
        if let Ok(out) = run_step(&step_list, None) {
            for line in out.stdout.lines() {
                let line = line.trim();
                let trimmed = line.trim_start_matches('.').trim_start_matches('/');
                if trimmed.is_empty() {
                    continue;
                }
                // The name as the archive stores it, because that is what an
                // extraction pattern has to match.
                payload_members.push(line.to_string());
                if trimmed.ends_with(".desktop") {
                    let name = Path::new(trimmed)
                        .file_name()
                        .map_or(trimmed.to_string(), |s| s.to_string_lossy().to_string());
                    desktop_files.push(name);
                } else if trimmed.ends_with(".service")
                    || trimmed.ends_with(".socket")
                    || trimmed.ends_with(".target")
                    || trimmed.ends_with(".timer")
                {
                    let name = Path::new(trimmed)
                        .file_name()
                        .map_or(trimmed.to_string(), |s| s.to_string_lossy().to_string());
                    systemd_units.push(name);
                }
            }
        }
    }

    let needed_libraries = data_tar
        .as_ref()
        .map(|tar| payload_libraries(tar, &payload_members))
        .unwrap_or_default();

    let note = format!(
        "Inspected DEB package '{}' ({} {}). {} maintainer scripts parsed but unexecuted. \
         {} dependencies detected. {} shared library needs read from the payload's ELF objects \
         ({} files listed).",
        pkg_name,
        version,
        architecture,
        maintainer_scripts.len(),
        dependencies.len(),
        needed_libraries.len(),
        payload_members.len()
    );

    Ok(InspectionReport {
        target_file: file_path.to_string(),
        format: "deb".to_string(),
        package_name: pkg_name,
        version,
        architecture,
        description,
        maintainer,
        dependencies,
        provides,
        maintainer_scripts,
        desktop_files,
        systemd_units,
        needed_libraries,
        compatibility_note: note,
        size_bytes,
        sha256,
    })
}

fn inspect_rpm(
    file_path: &str,
    size_bytes: u64,
    sha256: String,
) -> Result<InspectionReport, String> {
    let bytes =
        fs::read(file_path).map_err(|e| format!("Failed to read file '{}': {}", file_path, e))?;
    // rpm keeps its whole reviewable half — metadata, dependencies and every
    // scriptlet — in the header, so none of it requires touching the payload.
    let metadata = rpm::decode(&bytes)?;
    drop(bytes);

    let mut desktop_files = Vec::new();
    let mut systemd_units = Vec::new();
    let mut payload_members = Vec::new();

    // Scan RPM payload files with bsdtar (built-in libarchive RPM support)
    let step_list = Step::new(
        "bsdtar",
        vec!["-tf".to_string(), file_path.to_string()],
        "List RPM payload files",
        30,
    );
    if let Ok(out) = run_step(&step_list, None) {
        for line in out.stdout.lines() {
            let line = line.trim();
            let trimmed = line.trim_start_matches('.').trim_start_matches('/');
            if trimmed.is_empty() {
                continue;
            }
            payload_members.push(line.to_string());
            if trimmed.ends_with(".desktop") {
                let name = Path::new(trimmed)
                    .file_name()
                    .map_or(trimmed.to_string(), |s| s.to_string_lossy().to_string());
                desktop_files.push(name);
            } else if trimmed.ends_with(".service")
                || trimmed.ends_with(".socket")
                || trimmed.ends_with(".target")
                || trimmed.ends_with(".timer")
            {
                let name = Path::new(trimmed)
                    .file_name()
                    .map_or(trimmed.to_string(), |s| s.to_string_lossy().to_string());
                systemd_units.push(name);
            }
        }
    }

    let needed_libraries = payload_libraries(Path::new(file_path), &payload_members);

    // A package that ships nothing under its own name says so; an empty header
    // field is reported as empty rather than guessed from the file name.
    let version = version_evr(&metadata);
    let summary = metadata.summary.trim();
    let body = metadata.description.trim();
    let description = if body.is_empty() {
        metadata.summary.clone()
    } else if summary.is_empty() || body.starts_with(summary) {
        metadata.description.clone()
    } else {
        format!("{}\n{}", metadata.summary, metadata.description)
    };
    let maintainer_scripts = metadata
        .scriptlets
        .iter()
        .map(script_info)
        .collect::<Vec<_>>();
    let custom_interpreters: Vec<String> = metadata
        .scriptlets
        .iter()
        .filter_map(|script| script.interpreter.clone())
        .collect();

    let mut note = format!(
        "Inspected RPM package '{}' ({} {}) from its header. Declared license: {}; upstream: {}. \
         {} scriptlets parsed but unexecuted. \
         {} dependencies and {} capabilities detected; the payload ({} {} compressed) ships {} files. \
         {} shared library needs read from the payload's ELF objects ({} files listed).",
        metadata.name,
        version,
        metadata.architecture,
        if metadata.license.is_empty() {
            "none"
        } else {
            &metadata.license
        },
        if metadata.url.is_empty() {
            "none"
        } else {
            &metadata.url
        },
        maintainer_scripts.len(),
        metadata.requires.len(),
        metadata.provides.len(),
        metadata.payload_compressor,
        metadata.payload_format,
        metadata.file_count,
        needed_libraries.len(),
        payload_members.len(),
    );
    if !custom_interpreters.is_empty() {
        note.push_str(&format!(
            " Scriptlets run under a non-shell interpreter: {}.",
            custom_interpreters.join(", ")
        ));
    }

    Ok(InspectionReport {
        target_file: file_path.to_string(),
        format: "rpm".to_string(),
        package_name: metadata.name.clone(),
        version,
        architecture: metadata.architecture,
        description,
        maintainer: if metadata.packager.is_empty() {
            "unknown".to_string()
        } else {
            metadata.packager
        },
        dependencies: metadata.requires,
        provides: metadata.provides,
        maintainer_scripts,
        desktop_files,
        systemd_units,
        needed_libraries,
        compatibility_note: note,
        size_bytes,
        sha256,
    })
}

/// rpm's own way of naming a build: `[epoch:]version-release`.
fn version_evr(metadata: &rpm::RpmMetadata) -> String {
    let epoch = metadata
        .epoch
        .as_deref()
        .map(|epoch| format!("{epoch}:"))
        .unwrap_or_default();
    let release = if metadata.release.is_empty() {
        String::new()
    } else {
        format!("-{}", metadata.release)
    };
    format!("{}{}{}", epoch, metadata.version, release)
}

/// A scriptlet is shown as archived text with the execution flag set to false,
/// exactly like a DEB maintainer script, plus the interpreter rpm would exec.
fn script_info(scriptlet: &Scriptlet) -> ScriptInfo {
    let status = if scriptlet.body.contains('\0') {
        "unsupported".to_string()
    } else if let Some(interpreter) = &scriptlet.interpreter {
        format!("interpreter:{interpreter}")
    } else {
        "readable_shell".to_string()
    };
    ScriptInfo {
        name: scriptlet.name.clone(),
        content: scriptlet.body.clone(),
        executed: false,
        status,
    }
}

/// Extensions that mark a payload member as data rather than a binary. Only
/// used to decide what is worth extracting for an ELF header check, so a wrong
/// guess costs a skipped file and never a false claim.
const NON_ELF_EXTENSIONS: &[&str] = &[
    "txt",
    "md",
    "rst",
    "html",
    "xml",
    "json",
    "yaml",
    "yml",
    "toml",
    "ini",
    "cfg",
    "conf",
    "desktop",
    "metainfo",
    "appdata",
    "appstream",
    "png",
    "jpg",
    "jpeg",
    "gif",
    "svg",
    "ico",
    "woff",
    "woff2",
    "ttf",
    "otf",
    "po",
    "pot",
    "mo",
    "gz",
    "bz2",
    "xz",
    "zst",
    "lzma",
    "zip",
    "tar",
    "deb",
    "rpm",
    "py",
    "pyc",
    "pyo",
    "js",
    "ts",
    "lua",
    "rb",
    "pl",
    "pm",
    "sh",
    "bash",
    "zsh",
    "fish",
    "c",
    "cc",
    "cpp",
    "h",
    "hpp",
    "hh",
    "include",
    "license",
    "list",
    "links",
    "log",
    "readme",
    "copyright",
    "examples",
    "samples",
    "doc",
    "man",
    "info",
    "la",
    "prl",
    "pc",
    "cmake",
    "mk",
    "am",
    "ac",
    "in",
    "desktop",
    "service",
    "socket",
    "target",
    "timer",
    "mount",
    "automount",
    "swap",
    "path",
    "slice",
    "scope",
    "rules",
    "pkla",
    "policy",
    "link",
];

/// True when a payload member is plausibly an ELF object worth extracting.
/// The payload listing carries names only, so names have to do the filtering;
/// the ELF reader itself confirms or rejects every candidate it is handed.
fn looks_like_elf(member: &str) -> bool {
    if member.starts_with('-') || member.starts_with('#') || member.starts_with('~') {
        // These are pattern arguments to bsdtar, not file names.
        return false;
    }
    if member.contains(['*', '?', '[', ']', '{', '}', '\\']) {
        return false;
    }
    let path = member.trim_start_matches('.').trim_start_matches('/');
    let name = match path.rsplit_once('/') {
        Some((_, name)) => name,
        None => path,
    };
    if name.is_empty() || name.ends_with('.') || path.ends_with('/') {
        return false;
    }
    // A `.dbg`/`.o`/`.a` object or a shared library by name.
    if name.contains(".so") {
        return true;
    }
    let extension = match name.rsplit_once('.') {
        Some((_, extension)) => extension.to_lowercase(),
        None => String::new(),
    };
    if NON_ELF_EXTENSIONS.contains(&extension.as_str()) {
        return false;
    }
    if path.starts_with("usr/lib/debug/") || path.contains("/locale-archive") {
        return false;
    }
    // Version-suffixed files (`python3.11`, `foo.1.0`) and extensionless names
    // under a bin- or lib-shaped directory are where ELF objects live.
    let version_like = !extension.is_empty()
        && extension
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.');
    let directory = &path[..path.len() - name.len()];
    let in_executable_dir = ["bin/", "sbin/", "lib/", "lib64/", "libexec/", "lib32/"]
        .iter()
        .any(|prefix| directory.starts_with(*prefix) || directory.contains(&format!("/{prefix}")));
    name.is_ascii() && (extension.is_empty() || version_like || in_executable_dir)
}

/// The shared libraries the payload's own ELF objects declare as needed.
///
/// Only the members that the listing suggests could be binaries are extracted,
/// in one bounded `bsdtar` run, and only regular files under the temporary root
/// are opened — symlinks and anything else are never followed. Nothing from the
/// package is executed, and an unusable archive simply answers with no
/// libraries.
fn payload_libraries(archive: &Path, members: &[String]) -> Vec<String> {
    let mut patterns: Vec<String> = Vec::new();
    for member in members {
        if looks_like_elf(member) {
            patterns.push(member.clone());
            if patterns.len() >= MAX_ELF_FILES {
                break;
            }
        }
    }
    if patterns.is_empty() {
        return Vec::new();
    }

    let temp = match TempDir::new() {
        Ok(temp) => temp,
        Err(_) => return Vec::new(),
    };
    let root = temp.path().join("elf-scan");
    if fs::create_dir_all(&root).is_err() {
        return Vec::new();
    }

    let mut args = vec![
        "-xf".to_string(),
        archive.display().to_string(),
        "-C".to_string(),
        root.display().to_string(),
        "--".to_string(),
    ];
    args.append(&mut patterns);
    let step = Step::new(
        "bsdtar",
        args,
        "Extract ELF payload members for a library scan",
        90,
    );
    if run_step(&step, None).is_err() {
        return Vec::new();
    }

    let mut libraries: Vec<String> = Vec::new();
    for file in extracted_regular_files(&root) {
        for library in needed_libraries_in_file(&file) {
            if !libraries.contains(&library) {
                libraries.push(library);
            }
        }
    }
    libraries.sort();
    libraries
}

/// Regular files under `root`, symlinks and other entry kinds skipped rather
/// than followed. Capped so a payload that unpacked more than expected cannot
/// turn the scan into an unbounded walk.
fn extracted_regular_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.is_dir() {
                stack.push(path);
            } else if metadata.is_file() {
                if files.len() >= MAX_ELF_FILES {
                    return files;
                }
                files.push(path);
            }
        }
    }
    files
}

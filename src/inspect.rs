use crate::process::{run_step, Step};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
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
    pub maintainer_scripts: Vec<ScriptInfo>,
    pub desktop_files: Vec<String>,
    pub systemd_units: Vec<String>,
    pub needed_libraries: Vec<String>,
    pub compatibility_note: String,
    pub package_bytes: Option<Vec<u8>>,
}

pub fn inspect_package(file_path: &str) -> Result<InspectionReport, String> {
    let path = Path::new(file_path);
    if !path.exists() {
        return Err(format!("File not found: '{}'", file_path));
    }

    let bytes = fs::read(path).map_err(|e| format!("Failed to read file '{:?}': {}", path, e))?;
    let package_bytes: Option<Vec<u8>> = Some(bytes.to_vec());

    let file_str = file_path.to_lowercase();
    if file_str.ends_with(".deb") {
        inspect_deb(file_path, package_bytes)
    } else if file_str.ends_with(".rpm") {
        inspect_rpm(file_path, package_bytes)
    } else {
        Err(format!(
            "Unsupported foreign package format for file '{}'",
            file_path
        ))
    }
}

fn inspect_deb(
    file_path: &str,
    package_bytes: Option<Vec<u8>>,
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
    let needed_libraries = Vec::new();

    if let Some(d_tar) = data_tar {
        let step_list = Step::new(
            "bsdtar",
            vec!["-tf".to_string(), d_tar.to_str().unwrap().to_string()],
            "List DEB payload files",
            30,
        );
        if let Ok(out) = run_step(&step_list, None) {
            for line in out.stdout.lines() {
                let trimmed = line.trim().trim_start_matches('.').trim_start_matches('/');
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

    let note = format!(
        "Inspected DEB package '{}' ({} {}). {} maintainer scripts parsed but unexecuted. {} dependencies detected.",
        pkg_name, version, architecture, maintainer_scripts.len(), dependencies.len()
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
        maintainer_scripts,
        desktop_files,
        systemd_units,
        needed_libraries,
        compatibility_note: note,
        package_bytes,
    })
}

fn inspect_rpm(
    file_path: &str,
    package_bytes: Option<Vec<u8>>,
) -> Result<InspectionReport, String> {
    let mut desktop_files = Vec::new();
    let mut systemd_units = Vec::new();
    let needed_libraries = Vec::new();
    let maintainer_scripts = Vec::new();

    // Scan RPM payload files with bsdtar (built-in libarchive RPM support)
    let step_list = Step::new(
        "bsdtar",
        vec!["-tf".to_string(), file_path.to_string()],
        "List RPM payload files",
        30,
    );
    if let Ok(out) = run_step(&step_list, None) {
        for line in out.stdout.lines() {
            let trimmed = line.trim().trim_start_matches('.').trim_start_matches('/');
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

    let file_name = Path::new(file_path)
        .file_name()
        .map_or("package".to_string(), |s| s.to_string_lossy().to_string());
    let (pkg_name, version, architecture) = parse_rpm_filename(&file_name);

    let note = format!(
        "Inspected RPM package '{}' ({} {}). Payload files scanned without execution.",
        pkg_name, version, architecture
    );

    Ok(InspectionReport {
        target_file: file_path.to_string(),
        format: "rpm".to_string(),
        package_name: pkg_name,
        version,
        architecture,
        description: format!("Imported RPM package {}", file_name),
        maintainer: "unknown".to_string(),
        dependencies: Vec::new(),
        maintainer_scripts,
        desktop_files,
        systemd_units,
        needed_libraries,
        compatibility_note: note,
        package_bytes,
    })
}

fn parse_rpm_filename(file_name: &str) -> (String, String, String) {
    let clean = file_name.strip_suffix(".rpm").unwrap_or(file_name);
    let parts: Vec<&str> = clean.split('-').collect();
    if parts.len() >= 3 {
        let name = parts[..parts.len() - 2].join("-");
        let ver = parts[parts.len() - 2].to_string();
        let rel_arch = parts[parts.len() - 1];
        let arch = if let Some((_, a)) = rel_arch.rsplit_once('.') {
            a.to_string()
        } else {
            "x86_64".to_string()
        };
        (name, ver, arch)
    } else if parts.len() == 2 {
        (
            parts[0].to_string(),
            parts[1].to_string(),
            "x86_64".to_string(),
        )
    } else {
        (clean.to_string(), "1.0.0".to_string(), "x86_64".to_string())
    }
}

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
    let extract_dir = temp_dir.path().join("extract");
    fs::create_dir_all(&control_dir).map_err(|e| e.to_string())?;
    fs::create_dir_all(&extract_dir).map_err(|e| e.to_string())?;

    // Extract control directory via dpkg-deb -e
    let step_ctrl = Step::new(
        "dpkg-deb",
        vec!["-e", file_path, control_dir.to_str().unwrap()],
        "Extract DEB control files",
        30,
    );
    let _ = run_step(&step_ctrl, None);

    // Extract payload via dpkg-deb -x
    let step_ext = Step::new(
        "dpkg-deb",
        vec!["-x", file_path, extract_dir.to_str().unwrap()],
        "Extract DEB payload files",
        30,
    );
    let _ = run_step(&step_ext, None);

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

    // Inspect maintainer scripts (preinst, postinst, prerm, postrm)
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

    // Find desktop files, systemd units, and ELF NEEDED libraries
    let (desktop_files, systemd_units, needed_libraries) = analyze_extracted_files(&extract_dir);

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
    let temp_dir = TempDir::new().map_err(|e| format!("Failed to create temp directory: {}", e))?;
    let extract_dir = temp_dir.path().join("extract");
    fs::create_dir_all(&extract_dir).map_err(|e| e.to_string())?;

    // Read metadata via rpm -qp --info
    let step_info = Step::new(
        "rpm",
        vec!["-qp", "--info", file_path],
        "Query RPM package metadata",
        15,
    );
    let mut pkg_name = "unknown".to_string();
    let mut version = "unknown".to_string();
    let mut architecture = "unknown".to_string();
    let mut description = String::new();
    let mut maintainer = String::new();

    if let Ok(out) = run_step(&step_info, None) {
        if out.exit_code == 0 {
            for line in out.stdout.lines() {
                if let Some((k, v)) = line.split_once(':') {
                    let key = k.trim();
                    let val = v.trim();
                    match key {
                        "Name" => pkg_name = val.to_string(),
                        "Version" => version = val.to_string(),
                        "Architecture" => architecture = val.to_string(),
                        "Summary" | "Description" => description = val.to_string(),
                        "Packager" | "Vendor" => maintainer = val.to_string(),
                        _ => {}
                    }
                }
            }
        }
    }

    // Read dependencies via rpm -qp --requires
    let step_req = Step::new(
        "rpm",
        vec!["-qp", "--requires", file_path],
        "Query RPM requirements",
        15,
    );
    let mut dependencies = Vec::new();
    if let Ok(out) = run_step(&step_req, None) {
        if out.exit_code == 0 {
            for line in out.stdout.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    dependencies.push(trimmed.to_string());
                }
            }
        }
    }

    // Read scriptlets via rpm -qp --scripts
    let step_scr = Step::new(
        "rpm",
        vec!["-qp", "--scripts", file_path],
        "Query RPM scriptlets",
        15,
    );
    let mut maintainer_scripts = Vec::new();
    if let Ok(out) = run_step(&step_scr, None) {
        if out.exit_code == 0 && !out.stdout.trim().is_empty() {
            maintainer_scripts.push(ScriptInfo {
                name: "rpm_scriptlets".to_string(),
                content: out.stdout,
                executed: false,
                status: "readable_shell".to_string(),
            });
        }
    }

    // Extract payload via bsdtar into extract_dir
    let step_tar = Step::new(
        "bsdtar",
        vec!["-xf", file_path, "-C", extract_dir.to_str().unwrap()],
        "Extract RPM payload with bsdtar",
        30,
    );
    let _ = run_step(&step_tar, None);

    let (desktop_files, systemd_units, needed_libraries) = analyze_extracted_files(&extract_dir);

    let note = format!(
        "Inspected RPM package '{}' ({} {}). {} maintainer scriptlets parsed but unexecuted. {} dependencies detected.",
        pkg_name, version, architecture, maintainer_scripts.len(), dependencies.len()
    );

    Ok(InspectionReport {
        target_file: file_path.to_string(),
        format: "rpm".to_string(),
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

fn analyze_extracted_files(root: &Path) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut desktop_files = Vec::new();
    let mut systemd_units = Vec::new();
    let mut needed_libs = Vec::new();

    let _walker = match fs::read_dir(root) {
        Ok(w) => w,
        Err(_) => return (desktop_files, systemd_units, needed_libs),
    };

    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.is_file() {
                    let file_name = path.file_name().unwrap_or_default().to_string_lossy();

                    if file_name.ends_with(".desktop") {
                        desktop_files.push(file_name.to_string());
                    } else if file_name.ends_with(".service")
                        || file_name.ends_with(".socket")
                        || file_name.ends_with(".timer")
                        || file_name.ends_with(".target")
                    {
                        systemd_units.push(file_name.to_string());
                    }

                    // Check for ELF executable/library and read DT_NEEDED via readelf
                    if is_elf(&path) {
                        let step_elf = Step::new(
                            "readelf",
                            vec!["-d", path.to_str().unwrap()],
                            "Inspect ELF DT_NEEDED entries",
                            10,
                        );
                        if let Ok(out) = run_step(&step_elf, None) {
                            if out.exit_code == 0 {
                                for line in out.stdout.lines() {
                                    if line.contains("(NEEDED)") {
                                        if let Some(pos) = line.find("Shared library: [") {
                                            let rest = &line[pos + "Shared library: [".len()..];
                                            if let Some(end) = rest.find(']') {
                                                let lib = &rest[..end];
                                                if !needed_libs.contains(&lib.to_string()) {
                                                    needed_libs.push(lib.to_string());
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    (desktop_files, systemd_units, needed_libs)
}

fn is_elf(path: &Path) -> bool {
    if let Ok(mut f) = fs::File::open(path) {
        let mut magic = [0u8; 4];
        if std::io::Read::read_exact(&mut f, &mut magic).is_ok() {
            return magic == [0x7f, b'E', b'L', b'F'];
        }
    }
    false
}

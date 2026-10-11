use crate::process::{run_step, ProcessOutput, Step};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

pub const DEFAULT_NETWORK_ENDPOINT: &str = "https://archlinux.org";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CheckResult {
    pub name: String,
    pub status: String, // "pass", "unavailable", or "fail"
    pub message: String,
}

/// Comprehensive report of system readiness for building packages on Arch Linux.
///
/// # Readiness Semantics (`ready`)
/// A system is considered `ready` (`ready: true`) when all essential local packaging
/// prerequisites are satisfied:
/// - `arch`: The host is an Arch Linux system — os-release names it, or `pacman`
///   and `makepkg` are both in PATH. This check leads the report so a non-Arch
///   host gets one reason instead of five messages about missing binaries.
/// - `pacman`: The Arch Linux package manager is installed.
/// - `base-devel`: Essential build tools are available.
/// - `devtools`: `mkarchroot` and `makechrootpkg` are present for isolated clean chroot builds.
/// - `compiler`: GCC compiler toolchain is operational.
/// - `keyring`: Pacman GPG keyring is populated for signature validation.
///
/// Non-blocking checks:
/// - `network`:
///   - `pass`: HTTPS connectivity to the endpoint verified with strict TLS validation.
///   - `unavailable`: Network is offline or DNS is blocked. This does NOT mark the
///     system unready (`ready: true`), because local package building, clean chroot
///     preparation with local caches, and foreign package (.deb/.rpm) inspection can proceed offline.
///   - `fail`: The endpoint responded but TLS certificate verification or handshake failed,
///     or an invalid HTTP protocol response occurred. This indicates a potential security
///     or configuration failure and marks `ready: false`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DoctorReport {
    pub ready: bool,
    pub checks: Vec<CheckResult>,
}

/// Evaluates the result of an HTTPS network probe into `pass`, `unavailable`, or `fail`.
///
/// Distinguishes:
/// - `pass`: HTTPS endpoint reachable and TLS handshake verified.
/// - `unavailable`: DNS resolution failed, network connection refused, host offline, or timed out.
/// - `fail`: Endpoint responded but request is invalid, or TLS verification failed.
pub fn evaluate_network_probe_result(
    step_result: Result<&ProcessOutput, &str>,
    endpoint: &str,
) -> CheckResult {
    match step_result {
        Ok(out) => {
            let stderr = out.stderr.trim();
            let stdout = out.stdout.trim();

            if out.exit_code == 0 {
                // Inspect headers in stdout (e.g. "HTTP/2 200" or "HTTP/1.1 200 OK")
                let status_line = stdout.lines().find(|l| l.starts_with("HTTP/"));
                let is_http_error = if let Some(line) = status_line {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if parts.len() >= 2 {
                        parts[1].parse::<u16>().is_ok_and(|code| code >= 400)
                    } else {
                        false
                    }
                } else {
                    false
                };

                if is_http_error {
                    CheckResult {
                        name: "network".to_string(),
                        status: "fail".to_string(),
                        message: format!(
                            "HTTP endpoint {} responded with error: {}",
                            endpoint,
                            status_line.unwrap_or("unknown HTTP error")
                        ),
                    }
                } else {
                    CheckResult {
                        name: "network".to_string(),
                        status: "pass".to_string(),
                        message: format!("HTTPS network connectivity verified ({})", endpoint),
                    }
                }
            } else {
                let stderr_lower = stderr.to_lowercase();

                // TLS / Certificate failures (e.g., CURLE_SSL_CONNECT_ERROR 35,
                // CURLE_PEER_FAILED_VERIFICATION 51/60, bad CA file 77)
                let is_tls_error =
                    matches!(out.exit_code, 35 | 51 | 58 | 59 | 60 | 77 | 82 | 83 | 90)
                        || stderr_lower.contains("ssl")
                        || stderr_lower.contains("certificate")
                        || stderr_lower.contains("cert")
                        || stderr_lower.contains("handshake")
                        || stderr_lower.contains("tls");

                // DNS / connection offline / timeouts (e.g., CURLE_COULDNT_RESOLVE_HOST 6,
                // CURLE_COULDNT_CONNECT 7, CURLE_OPERATION_TIMEDOUT 28)
                let is_dns_or_offline = matches!(out.exit_code, 5 | 6 | 7 | 28 | 52)
                    || stderr_lower.contains("could not resolve")
                    || stderr_lower.contains("failed to connect")
                    || stderr_lower.contains("connection refused")
                    || stderr_lower.contains("network is unreachable")
                    || stderr_lower.contains("no route to host")
                    || stderr_lower.contains("timed out")
                    || stderr_lower.contains("operation timeout");

                if is_tls_error {
                    CheckResult {
                        name: "network".to_string(),
                        status: "fail".to_string(),
                        message: format!(
                            "TLS verification or handshake failed for {}: {}",
                            endpoint,
                            if stderr.is_empty() {
                                "certificate or TLS handshake error"
                            } else {
                                stderr
                            }
                        ),
                    }
                } else if is_dns_or_offline {
                    CheckResult {
                        name: "network".to_string(),
                        status: "unavailable".to_string(),
                        message: format!(
                            "Network offline or endpoint unreachable ({}): {}",
                            endpoint,
                            if stderr.is_empty() {
                                "DNS or connection offline"
                            } else {
                                stderr
                            }
                        ),
                    }
                } else if out.exit_code == 22 {
                    CheckResult {
                        name: "network".to_string(),
                        status: "fail".to_string(),
                        message: format!(
                            "HTTP request to {} returned error: {}",
                            endpoint,
                            if stderr.is_empty() {
                                "HTTP status code error"
                            } else {
                                stderr
                            }
                        ),
                    }
                } else {
                    CheckResult {
                        name: "network".to_string(),
                        status: "fail".to_string(),
                        message: format!(
                            "HTTPS probe to {} failed (exit code {}): {}",
                            endpoint, out.exit_code, stderr
                        ),
                    }
                }
            }
        }
        Err(err_msg) => {
            let lower = err_msg.to_lowercase();
            if err_msg.contains("timed out") {
                CheckResult {
                    name: "network".to_string(),
                    status: "unavailable".to_string(),
                    message: format!(
                        "Network offline or connection timed out probing {}: {}",
                        endpoint, err_msg
                    ),
                }
            } else if lower.contains("failed to spawn")
                || lower.contains("failed to execute")
                || lower.contains("no such file or directory")
                || lower.contains("not found")
                || lower.contains("permission denied")
            {
                // The probe itself could not be started — curl is absent, or the
                // sandbox refused it. Nothing about the network was observed, so
                // claiming a TLS or protocol failure here would point the reader
                // at a certificate problem that does not exist.
                CheckResult {
                    name: "network".to_string(),
                    status: "unavailable".to_string(),
                    message: format!(
                        "HTTPS probe of {} could not be run: {} — no connectivity conclusion is drawn, and offline inspection and local builds still work",
                        endpoint, err_msg
                    ),
                }
            } else {
                CheckResult {
                    name: "network".to_string(),
                    status: "fail".to_string(),
                    message: format!("Failed to execute HTTPS network probe: {}", err_msg),
                }
            }
        }
    }
}

/// Probes HTTPS network connectivity with strict TLS validation and timeouts.
pub fn probe_network(endpoint: &str) -> CheckResult {
    let step_net = Step::new(
        "curl",
        vec![
            "-sSL",
            "-I",
            "--fail",
            "--connect-timeout",
            "5",
            "-m",
            "8",
            endpoint,
        ],
        format!("Check HTTPS connectivity to {}", endpoint),
        10,
    );
    let res = run_step(&step_net, None);
    evaluate_network_probe_result(res.as_ref().map_err(|s| s.as_str()), endpoint)
}

/// Checks whose failure alone makes the system unready to build or install.
/// The host is first because every other entry is an Arch packaging tool that
/// cannot exist anywhere else.
pub const ESSENTIAL_CHECKS: [&str; 6] = [
    "arch",
    "pacman",
    "base-devel",
    "devtools",
    "compiler",
    "keyring",
];

/// Computes overall system readiness based on essential and non-blocking checks.
pub fn compute_readiness(checks: &[CheckResult]) -> bool {
    for check in checks {
        match check.name.as_str() {
            // The host itself is the first prerequisite: every other essential
            // check is an Arch packaging tool that cannot exist elsewhere.
            name if ESSENTIAL_CHECKS.contains(&name) => {
                if check.status != "pass" {
                    return false;
                }
            }
            "network" if check.status == "fail" => {
                // "pass" or "unavailable" are permitted for readiness (offline building is supported).
                // "fail" (e.g. TLS verification failure) marks system unready as a security safeguard.
                return false;
            }
            _ => {}
        }
    }
    true
}

/// True when `program` resolves through the user's PATH. Scanned directly: a
/// host without `which` still has a PATH, and a missing helper must not be
/// reported as a missing package manager.
fn in_path(program: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|directory| {
        let candidate = directory.join(program);
        let Ok(metadata) = fs::metadata(&candidate) else {
            return false;
        };
        metadata.is_file() && is_executable(&metadata)
    })
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    true
}

/// Strip the quotes `/etc/os-release` optionally wraps its values.
fn unquote(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_string()
}

/// ArchBridge drives `pacman`, `makepkg` and `devtools`, and packages the result
/// for an Arch system. On anything else those tools do not exist, so the reason
/// has to be said here — once, in plain words — instead of leaving a caller with
/// five separate messages about individual missing binaries.
pub fn check_arch_host() -> CheckResult {
    let os_release = fs::read_to_string("/etc/os-release").unwrap_or_default();
    let mut ids: Vec<String> = Vec::new();
    for line in os_release.lines() {
        if let Some(value) = line.strip_prefix("ID=") {
            ids.push(unquote(value));
        } else if let Some(value) = line.strip_prefix("ID_LIKE=") {
            ids.extend(unquote(value).split_whitespace().map(str::to_string));
        }
    }
    let named_arch = ids.iter().any(|id| id == "arch");
    let pacman = in_path("pacman");
    let makepkg = in_path("makepkg");

    if named_arch || (pacman && makepkg) {
        return CheckResult {
            name: "arch".to_string(),
            status: "pass".to_string(),
            message: format!(
                "Arch Linux host ({}); pacman and makepkg are in PATH",
                if ids.is_empty() {
                    "identified by its packaging tools".to_string()
                } else {
                    format!("os-release ID={}", ids.join(" "))
                }
            ),
        };
    }

    let mut missing: Vec<String> = Vec::new();
    if !named_arch {
        missing.push(if ids.is_empty() {
            "/etc/os-release does not name an Arch (or Arch-like) distribution".to_string()
        } else {
            format!("/etc/os-release reports ID={}", ids.join(" "))
        });
    }
    if !pacman {
        missing.push("pacman is not in PATH".to_string());
    }
    if !makepkg {
        missing.push("makepkg is not in PATH".to_string());
    }
    CheckResult {
        name: "arch".to_string(),
        status: "fail".to_string(),
        message: format!(
            "not an Arch Linux system: {} — ArchBridge builds and installs Arch packages, so \
             run it on Arch Linux or in an Arch container",
            missing.join(", ")
        ),
    }
}

/// Unprivileged user namespaces carry the smoke-test container. The old check
/// passed whenever `/proc/self/ns/user` existed — which is always on Linux, even
/// when an administrator has switched the feature off — so it could never fail.
/// The knobs that actually gate it are read instead.
pub fn check_user_namespaces() -> CheckResult {
    let mut blocked: Vec<String> = Vec::new();

    // Older kernels and some distributions expose an explicit on/off switch.
    if let Ok(value) = fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone") {
        let value = value.trim().to_string();
        if value != "1" {
            blocked.push(format!("kernel.unprivileged_userns_clone is {value}"));
        }
    }
    // The limit that is set to 0 to disable the feature on current kernels.
    if let Ok(value) = fs::read_to_string("/proc/sys/user/max_user_namespaces") {
        let value = value.trim().to_string();
        if value.parse::<u64>().unwrap_or(1) == 0 {
            blocked.push("user.max_user_namespaces is 0".to_string());
        }
    }

    if !blocked.is_empty() {
        CheckResult {
            name: "namespaces".to_string(),
            status: "fail".to_string(),
            message: format!(
                "Unprivileged user namespaces are restricted: {}",
                blocked.join(", ")
            ),
        }
    } else if Path::new("/proc/self/ns/user").exists() {
        CheckResult {
            name: "namespaces".to_string(),
            status: "pass".to_string(),
            message: "Kernel user namespaces support detected".to_string(),
        }
    } else {
        CheckResult {
            name: "namespaces".to_string(),
            status: "fail".to_string(),
            message: "No user namespace interface found under /proc".to_string(),
        }
    }
}

/// Workspace free space. Clean chroot builds unpack a full base filesystem, so
/// a nearly-full disk has to be reported before a build dies partway through.
pub fn check_disk_space(workspace: &Path) -> CheckResult {
    const MIN_FREE_KIB: u64 = 2 * 1024 * 1024;
    let step = Step::new(
        "df",
        vec!["-Pk".to_string(), workspace.to_string_lossy().to_string()],
        "Measure free workspace disk space",
        10,
    );
    let available_kib = match run_step(&step, None) {
        Ok(out) if out.exit_code == 0 => out
            .stdout
            .lines()
            .last()
            .and_then(|line| line.split_whitespace().nth(3))
            .and_then(|value| value.parse::<u64>().ok()),
        _ => None,
    };

    match available_kib {
        Some(kib) if kib >= MIN_FREE_KIB => CheckResult {
            name: "disk_space".to_string(),
            status: "pass".to_string(),
            message: format!(
                "{:.1} GiB free in {}",
                kib as f64 / 1024.0 / 1024.0,
                workspace.display()
            ),
        },
        Some(kib) => CheckResult {
            name: "disk_space".to_string(),
            status: "fail".to_string(),
            message: format!(
                "Only {:.1} GiB free in {}; clean chroot builds need at least 2 GiB",
                kib as f64 / 1024.0 / 1024.0,
                workspace.display()
            ),
        },
        None => CheckResult {
            name: "disk_space".to_string(),
            status: "unavailable".to_string(),
            message: "Could not measure free disk space (df unavailable)".to_string(),
        },
    }
}

pub fn run_doctor() -> DoctorReport {
    run_doctor_with_probe(probe_network, DEFAULT_NETWORK_ENDPOINT)
}

pub fn run_doctor_with_probe<F>(probe_fn: F, endpoint: &str) -> DoctorReport
where
    F: FnOnce(&str) -> CheckResult,
{
    let mut checks = Vec::new();

    // 0. The host: everything below is an Arch packaging tool.
    checks.push(check_arch_host());

    // 1. pacman
    let step_pacman = Step::new("which", vec!["pacman"], "Check pacman presence", 5);
    if let Ok(out) = run_step(&step_pacman, None) {
        if out.exit_code == 0 {
            checks.push(CheckResult {
                name: "pacman".to_string(),
                status: "pass".to_string(),
                message: "pacman package manager is installed".to_string(),
            });
        } else {
            checks.push(CheckResult {
                name: "pacman".to_string(),
                status: "fail".to_string(),
                message: "pacman executable not found in PATH".to_string(),
            });
        }
    } else {
        checks.push(CheckResult {
            name: "pacman".to_string(),
            status: "fail".to_string(),
            message: "Failed to check pacman executable".to_string(),
        });
    }

    // 2. base-devel meta-package
    let step_base_devel = Step::new(
        "pacman",
        vec!["-Qq", "base-devel"],
        "Check base-devel meta-package",
        10,
    );
    match run_step(&step_base_devel, None) {
        Ok(out) if out.exit_code == 0 => {
            checks.push(CheckResult {
                name: "base-devel".to_string(),
                status: "pass".to_string(),
                message: "base-devel group/meta-package is installed".to_string(),
            });
        }
        Ok(out) => {
            checks.push(CheckResult {
                name: "base-devel".to_string(),
                status: "fail".to_string(),
                message: format!("base-devel group check failed: {}", out.stderr.trim()),
            });
        }
        Err(e) => {
            checks.push(CheckResult {
                name: "base-devel".to_string(),
                status: "fail".to_string(),
                message: format!(
                    "base-devel cannot be queried because pacman does not run here: {}",
                    e
                ),
            });
        }
    }

    // 3. devtools (mkarchroot & makechrootpkg)
    let step_mkchroot = Step::new(
        "which",
        vec!["mkarchroot"],
        "Check devtools (mkarchroot)",
        5,
    );
    let step_makechrootpkg = Step::new(
        "which",
        vec!["makechrootpkg"],
        "Check devtools (makechrootpkg)",
        5,
    );
    let mk_ok = run_step(&step_mkchroot, None).is_ok_and(|o| o.exit_code == 0);
    let pkg_ok = run_step(&step_makechrootpkg, None).is_ok_and(|o| o.exit_code == 0);
    if mk_ok && pkg_ok {
        checks.push(CheckResult {
            name: "devtools".to_string(),
            status: "pass".to_string(),
            message: "devtools (mkarchroot and makechrootpkg) are installed".to_string(),
        });
    } else {
        checks.push(CheckResult {
            name: "devtools".to_string(),
            status: "fail".to_string(),
            message: "devtools missing: install with `sudo pacman -S --needed devtools`"
                .to_string(),
        });
    }

    // 4. Kernel unprivileged namespaces
    checks.push(check_user_namespaces());

    // 5. Free disk space for the build workspace
    checks.push(check_disk_space(Path::new(".")));

    // 6. Compiler toolchain (gcc / cargo)
    let step_gcc = Step::new("gcc", vec!["--version"], "Check compiler toolchain", 5);
    if let Ok(out) = run_step(&step_gcc, None) {
        if out.exit_code == 0 {
            checks.push(CheckResult {
                name: "compiler".to_string(),
                status: "pass".to_string(),
                message: "GCC compiler toolchain is available".to_string(),
            });
        } else {
            checks.push(CheckResult {
                name: "compiler".to_string(),
                status: "fail".to_string(),
                message: "GCC compiler missing or returning error".to_string(),
            });
        }
    } else {
        checks.push(CheckResult {
            name: "compiler".to_string(),
            status: "fail".to_string(),
            message: "GCC compiler toolchain not found".to_string(),
        });
    }

    // 7. Network HTTPS connectivity
    checks.push(probe_fn(endpoint));

    // 8. Pacman keyring
    let step_keyring = Step::new("pacman-key", vec!["-l"], "Check pacman keyring", 10);
    if let Ok(out) = run_step(&step_keyring, None) {
        if out.exit_code == 0 {
            checks.push(CheckResult {
                name: "keyring".to_string(),
                status: "pass".to_string(),
                message: "Pacman keyring is populated".to_string(),
            });
        } else {
            checks.push(CheckResult {
                name: "keyring".to_string(),
                status: "fail".to_string(),
                message: "Pacman keyring check returned failure".to_string(),
            });
        }
    } else {
        checks.push(CheckResult {
            name: "keyring".to_string(),
            status: "fail".to_string(),
            message: "pacman-key is unavailable here, so the keyring cannot be verified"
                .to_string(),
        });
    }

    let ready = compute_readiness(&checks);

    DoctorReport { ready, checks }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::ProcessOutput;

    #[test]
    fn test_network_probe_pass() {
        let output = ProcessOutput {
            exit_code: 0,
            stdout: "HTTP/2 200\r\ncontent-type: text/html\r\n".to_string(),
            stderr: String::new(),
        };
        let result = evaluate_network_probe_result(Ok(&output), "https://archlinux.org");
        assert_eq!(result.name, "network");
        assert_eq!(result.status, "pass");
        assert!(result.message.contains("verified"));
    }

    #[test]
    fn test_network_probe_unavailable_dns() {
        let output = ProcessOutput {
            exit_code: 6,
            stdout: String::new(),
            stderr: "curl: (6) Could not resolve host: archlinux.org".to_string(),
        };
        let result = evaluate_network_probe_result(Ok(&output), "https://archlinux.org");
        assert_eq!(result.name, "network");
        assert_eq!(result.status, "unavailable");
        assert!(result.message.contains("offline") || result.message.contains("unreachable"));
    }

    #[test]
    fn test_network_probe_unavailable_connection_refused() {
        let output = ProcessOutput {
            exit_code: 7,
            stdout: String::new(),
            stderr: "curl: (7) Failed to connect to 127.0.0.1:54321: Connection refused"
                .to_string(),
        };
        let result = evaluate_network_probe_result(Ok(&output), "https://archlinux.org");
        assert_eq!(result.name, "network");
        assert_eq!(result.status, "unavailable");
    }

    #[test]
    fn test_network_probe_unavailable_timeout() {
        let output = ProcessOutput {
            exit_code: 28,
            stdout: String::new(),
            stderr: "curl: (28) Operation timed out after 5000 milliseconds with 0 bytes received"
                .to_string(),
        };
        let result = evaluate_network_probe_result(Ok(&output), "https://archlinux.org");
        assert_eq!(result.name, "network");
        assert_eq!(result.status, "unavailable");
    }

    #[test]
    fn test_network_probe_unavailable_process_timeout() {
        let result = evaluate_network_probe_result(
            Err("Process 'curl' timed out after 10s"),
            "https://archlinux.org",
        );
        assert_eq!(result.name, "network");
        assert_eq!(result.status, "unavailable");
    }

    #[test]
    fn test_network_probe_fail_tls_cert() {
        let output = ProcessOutput {
            exit_code: 60,
            stdout: String::new(),
            stderr: "curl: (60) SSL certificate problem: self-signed certificate".to_string(),
        };
        let result = evaluate_network_probe_result(Ok(&output), "https://archlinux.org");
        assert_eq!(result.name, "network");
        assert_eq!(result.status, "fail");
        assert!(result.message.contains("TLS") || result.message.contains("certificate"));
    }

    #[test]
    fn test_network_probe_fail_tls_handshake() {
        let output = ProcessOutput {
            exit_code: 35,
            stdout: String::new(),
            stderr: "curl: (35) error:0A000410:SSL routines::sslv3 alert handshake failure"
                .to_string(),
        };
        let result = evaluate_network_probe_result(Ok(&output), "https://archlinux.org");
        assert_eq!(result.name, "network");
        assert_eq!(result.status, "fail");
    }

    #[test]
    fn test_network_probe_fail_http_error() {
        let output = ProcessOutput {
            exit_code: 22,
            stdout: String::new(),
            stderr: "curl: (22) The requested URL returned error: 400".to_string(),
        };
        let result = evaluate_network_probe_result(Ok(&output), "https://archlinux.org");
        assert_eq!(result.name, "network");
        assert_eq!(result.status, "fail");
    }

    #[test]
    fn test_ready_status_when_only_network_is_unavailable() {
        let checks = vec![
            CheckResult {
                name: "pacman".to_string(),
                status: "pass".to_string(),
                message: "ok".to_string(),
            },
            CheckResult {
                name: "base-devel".to_string(),
                status: "pass".to_string(),
                message: "ok".to_string(),
            },
            CheckResult {
                name: "devtools".to_string(),
                status: "pass".to_string(),
                message: "ok".to_string(),
            },
            CheckResult {
                name: "namespaces".to_string(),
                status: "pass".to_string(),
                message: "ok".to_string(),
            },
            CheckResult {
                name: "disk_space".to_string(),
                status: "pass".to_string(),
                message: "ok".to_string(),
            },
            CheckResult {
                name: "compiler".to_string(),
                status: "pass".to_string(),
                message: "ok".to_string(),
            },
            CheckResult {
                name: "network".to_string(),
                status: "unavailable".to_string(),
                message: "offline".to_string(),
            },
            CheckResult {
                name: "keyring".to_string(),
                status: "pass".to_string(),
                message: "ok".to_string(),
            },
        ];

        assert!(compute_readiness(&checks));
    }

    #[test]
    fn test_unready_status_when_network_has_tls_failure() {
        let checks = vec![
            CheckResult {
                name: "pacman".to_string(),
                status: "pass".to_string(),
                message: "ok".to_string(),
            },
            CheckResult {
                name: "base-devel".to_string(),
                status: "pass".to_string(),
                message: "ok".to_string(),
            },
            CheckResult {
                name: "devtools".to_string(),
                status: "pass".to_string(),
                message: "ok".to_string(),
            },
            CheckResult {
                name: "compiler".to_string(),
                status: "pass".to_string(),
                message: "ok".to_string(),
            },
            CheckResult {
                name: "network".to_string(),
                status: "fail".to_string(),
                message: "TLS verification failed".to_string(),
            },
            CheckResult {
                name: "keyring".to_string(),
                status: "pass".to_string(),
                message: "ok".to_string(),
            },
        ];

        assert!(!compute_readiness(&checks));
    }

    #[test]
    fn test_unready_status_when_essential_check_fails() {
        let checks = vec![
            CheckResult {
                name: "pacman".to_string(),
                status: "fail".to_string(),
                message: "missing".to_string(),
            },
            CheckResult {
                name: "network".to_string(),
                status: "pass".to_string(),
                message: "ok".to_string(),
            },
        ];

        assert!(!compute_readiness(&checks));
    }

    #[test]
    fn test_run_doctor_with_mocked_offline_probe() {
        let report = run_doctor_with_probe(
            |_endpoint| CheckResult {
                name: "network".to_string(),
                status: "unavailable".to_string(),
                message: "Simulated offline network".to_string(),
            },
            DEFAULT_NETWORK_ENDPOINT,
        );

        let net_check = report.checks.iter().find(|c| c.name == "network").unwrap();
        assert_eq!(net_check.status, "unavailable");
        // The mocked probe only controls network status; local prerequisites
        // still vary between Arch hosts and non-Arch CI runners.
        assert_eq!(report.ready, compute_readiness(&report.checks));
    }

    fn passing_essentials() -> Vec<CheckResult> {
        ESSENTIAL_CHECKS
            .iter()
            .map(|name| CheckResult {
                name: name.to_string(),
                status: "pass".to_string(),
                message: "ok".to_string(),
            })
            .collect()
    }

    #[test]
    fn a_host_that_is_not_arch_is_the_reason_readiness_fails() {
        // Every packaging tool reports a green result, and the host check is
        // still the one that has to be able to say why the system cannot build.
        let mut checks = passing_essentials();
        let host = checks
            .iter_mut()
            .find(|check| check.name == "arch")
            .expect("the host is an essential check");
        host.status = "fail".to_string();
        host.message =
            "not an Arch Linux system: pacman is not in PATH, makepkg is not in PATH".to_string();
        assert!(!compute_readiness(&checks));
        assert!(checks
            .iter()
            .any(|check| check.name == "arch"
                && check.message.starts_with("not an Arch Linux system")));
    }

    #[test]
    fn the_offline_and_off_the_path_probe_are_both_non_blocking() {
        let mut checks = passing_essentials();
        checks.retain(|check| check.name != "arch");
        checks.push(CheckResult {
            name: "network".to_string(),
            status: "unavailable".to_string(),
            message: "Network offline".to_string(),
        });
        assert!(
            compute_readiness(&checks),
            "offline must not mark the system unready"
        );
    }

    #[test]
    fn a_probe_that_cannot_be_started_is_not_reported_as_a_tls_failure() {
        let result = evaluate_network_probe_result(
            Err("Failed to spawn process 'curl': No such file or directory (os error 2)"),
            "https://archlinux.org",
        );
        assert_eq!(result.name, "network");
        assert_eq!(
            result.status, "unavailable",
            "a probe that never ran observed nothing about the network"
        );
        assert!(
            result.message.contains("could not be run"),
            "{}",
            result.message
        );
        assert!(!result.message.contains("TLS"), "{}", result.message);
    }

    #[test]
    fn a_probe_refused_by_the_sandbox_also_reads_as_unavailable() {
        let result = evaluate_network_probe_result(
            Err("Failed to spawn process 'curl': Permission denied (os error 13)"),
            "https://archlinux.org",
        );
        assert_eq!(result.status, "unavailable");
    }

    #[test]
    fn the_host_check_describes_the_host_it_found() {
        let host = check_arch_host();
        assert_eq!(host.name, "arch");
        match host.status.as_str() {
            "pass" => assert!(
                host.message.contains("pacman and makepkg"),
                "{}",
                host.message
            ),
            "fail" => {
                assert!(
                    host.message.starts_with("not an Arch Linux system"),
                    "{}",
                    host.message
                );
                assert!(
                    host.message.contains("pacman")
                        || host.message.contains("makepkg")
                        || host.message.contains("os-release"),
                    "the refusal has to name what is missing: {}",
                    host.message
                );
            }
            other => panic!("the host check must pass or fail, got {other:?}"),
        }
    }

    #[test]
    fn the_doctor_reports_the_host_before_the_tools() {
        let report = run_doctor_with_probe(
            |_| CheckResult {
                name: "network".to_string(),
                status: "unavailable".to_string(),
                message: "offline".to_string(),
            },
            DEFAULT_NETWORK_ENDPOINT,
        );
        assert_eq!(
            report.checks.first().map(|check| check.name.as_str()),
            Some("arch"),
            "the reason a non-Arch host fails has to be the first line"
        );
        assert_eq!(report.ready, compute_readiness(&report.checks));
    }
}

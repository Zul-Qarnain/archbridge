use archbridge::doctor::{
    check_disk_space, check_user_namespaces, compute_readiness, evaluate_network_probe_result,
    run_doctor_with_probe, CheckResult, DEFAULT_NETWORK_ENDPOINT, ESSENTIAL_CHECKS,
};
use archbridge::process::ProcessOutput;
use archbridge::rpc::{JsonRpcRequest, RpcServer};
use serde_json::json;
use std::fs;
use std::path::Path;

#[test]
fn test_network_probe_pass_integration() {
    let output = ProcessOutput {
        exit_code: 0,
        stdout: "HTTP/2 200\r\ncontent-type: text/html; charset=utf-8\r\n".to_string(),
        stderr: String::new(),
    };
    let result = evaluate_network_probe_result(Ok(&output), "https://archlinux.org");
    assert_eq!(result.name, "network");
    assert_eq!(result.status, "pass");
    assert!(result.message.contains("verified"));
}

#[test]
fn test_network_probe_unavailable_dns_integration() {
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
fn test_ready_status_when_only_network_is_unavailable_integration() {
    let checks = vec![
        CheckResult {
            name: "pacman".to_string(),
            status: "pass".to_string(),
            message: "pacman package manager is installed".to_string(),
        },
        CheckResult {
            name: "base-devel".to_string(),
            status: "pass".to_string(),
            message: "base-devel group/meta-package is installed".to_string(),
        },
        CheckResult {
            name: "devtools".to_string(),
            status: "pass".to_string(),
            message: "devtools (mkarchroot and makechrootpkg) are installed".to_string(),
        },
        CheckResult {
            name: "namespaces".to_string(),
            status: "pass".to_string(),
            message: "Kernel user namespaces support detected".to_string(),
        },
        CheckResult {
            name: "disk_space".to_string(),
            status: "pass".to_string(),
            message: "Sufficient workspace disk space available".to_string(),
        },
        CheckResult {
            name: "compiler".to_string(),
            status: "pass".to_string(),
            message: "GCC compiler toolchain is available".to_string(),
        },
        CheckResult {
            name: "network".to_string(),
            status: "unavailable".to_string(),
            message: "Network offline or endpoint unreachable (https://archlinux.org): DNS or connection offline".to_string(),
        },
        CheckResult {
            name: "keyring".to_string(),
            status: "pass".to_string(),
            message: "Pacman keyring is populated".to_string(),
        },
    ];

    assert!(compute_readiness(&checks));
}

#[test]
fn test_doctor_offline_report_ready() {
    let report = run_doctor_with_probe(
        |_endpoint| CheckResult {
            name: "network".to_string(),
            status: "unavailable".to_string(),
            message: "Network offline or endpoint unreachable (https://archlinux.org)".to_string(),
        },
        DEFAULT_NETWORK_ENDPOINT,
    );

    let net_check = report
        .checks
        .iter()
        .find(|c| c.name == "network")
        .expect("network check must exist");
    assert_eq!(net_check.status, "unavailable");
    // The mocked probe only controls network status; local prerequisites
    // still vary between Arch hosts and non-Arch CI runners.
    assert_eq!(report.ready, compute_readiness(&report.checks));
}

#[test]
fn test_rpc_doctor_returns_valid_structure() {
    let mut server = RpcServer::new();
    let req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(10)),
        method: "v1.doctor".to_string(),
        params: json!({}),
    };
    let resp = server.handle_request(req);
    assert!(resp.error.is_none());
    let res = resp.result.expect("result must be present");
    assert!(res.get("checks").unwrap().is_array());
    assert!(res.get("ready").unwrap().is_boolean());

    let checks = res.get("checks").unwrap().as_array().unwrap();
    let net = checks
        .iter()
        .find(|c| c.get("name").and_then(|n| n.as_str()) == Some("network"))
        .expect("network check must be present in doctor checks");

    let status = net.get("status").and_then(|s| s.as_str()).unwrap();
    assert!(
        status == "pass" || status == "unavailable" || status == "fail",
        "Status must be pass, unavailable, or fail, got: {}",
        status
    );
}

/// The disk check used to return a hardcoded "pass" without measuring anything.
#[test]
fn test_disk_space_check_measures_instead_of_asserting() {
    let missing = check_disk_space(Path::new("/archbridge-does-not-exist"));
    assert_eq!(missing.name, "disk_space");
    assert_ne!(
        missing.status, "pass",
        "an unresolvable path must not report a healthy measurement"
    );

    let real = check_disk_space(Path::new("/"));
    assert!(
        matches!(real.status.as_str(), "pass" | "fail" | "unavailable"),
        "unexpected status: {}",
        real.status
    );
    if real.status != "unavailable" {
        assert!(
            real.message.contains("GiB"),
            "a measured result must quote the free space: {}",
            real.message
        );
    }
}

/// The namespaces check used to test for `/proc/self/ns/user`, which exists on
/// every Linux host even after an administrator disables the feature.
#[test]
fn test_namespaces_check_fails_when_sysctls_disable_user_namespaces() {
    let result = check_user_namespaces();
    assert_eq!(result.name, "namespaces");
    assert!(
        matches!(result.status.as_str(), "pass" | "fail"),
        "unexpected status: {}",
        result.status
    );

    let max = fs::read_to_string("/proc/sys/user/max_user_namespaces")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok());
    let clone_off = fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone")
        .ok()
        .is_some_and(|v| v.trim() != "1");
    if max == Some(0) || clone_off {
        assert_eq!(
            result.status, "fail",
            "kernel restrictions on unprivileged user namespaces must be reported"
        );
    }
}

/// A caller that gets `ready: false` needs one line that explains everything.
/// The report has to lead with the host, because every other essential check is
/// an Arch packaging tool that cannot exist on a machine that is not Arch.
#[test]
fn doctor_names_the_host_before_the_tools_it_cannot_find() {
    let mut server = RpcServer::new();
    let response = server.handle_request(JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(21)),
        method: "v1.doctor".to_string(),
        params: json!({}),
    });
    let res = response.result.expect("v1.doctor answers with a report");
    let checks: Vec<CheckResult> =
        serde_json::from_value(res.get("checks").cloned().unwrap()).expect("check list");

    assert_eq!(
        checks.first().map(|check| check.name.as_str()),
        Some("arch"),
        "the host check leads the report"
    );
    let host = &checks[0];
    assert!(
        matches!(host.status.as_str(), "pass" | "fail"),
        "the host either is Arch or is not: {}",
        host.status
    );
    if host.status == "fail" {
        assert!(
            host.message.starts_with("not an Arch Linux system"),
            "the refusal must say what was found instead: {}",
            host.message
        );
        assert!(
            host.message.contains("pacman")
                || host.message.contains("makepkg")
                || host.message.contains("os-release"),
            "and name the missing evidence: {}",
            host.message
        );
    }

    let ready = res.get("ready").unwrap().as_bool().unwrap();
    assert_eq!(
        ready,
        compute_readiness(&checks),
        "the answer a caller reads has to be the answer the checks compute"
    );
    if !ready {
        assert!(
            checks.iter().any(|check| {
                ESSENTIAL_CHECKS.contains(&check.name.as_str()) && check.status != "pass"
            }) || checks
                .iter()
                .any(|check| check.name == "network" && check.status == "fail"),
            "a refusal has to point at a specific blocking check"
        );
    }
}

/// "unavailable" is the vocabulary's offline status. Reading a probe that never
/// ran as a TLS failure would send the reader chasing certificates.
#[test]
fn an_unrunnable_network_probe_stays_out_of_the_blocking_vocabulary() {
    let output = ProcessOutput {
        exit_code: 6,
        stdout: String::new(),
        stderr: "curl: (6) Could not resolve host: archlinux.org".to_string(),
    };
    let offline = evaluate_network_probe_result(Ok(&output), DEFAULT_NETWORK_ENDPOINT);
    assert_eq!(offline.status, "unavailable");

    let never_ran = evaluate_network_probe_result(
        Err("Failed to spawn process 'curl': No such file or directory (os error 2)"),
        DEFAULT_NETWORK_ENDPOINT,
    );
    assert_eq!(never_ran.status, "unavailable", "{}", never_ran.message);

    let mut checks = vec![offline, never_ran];
    for name in ESSENTIAL_CHECKS {
        checks.push(CheckResult {
            name: name.to_string(),
            status: "pass".to_string(),
            message: "ok".to_string(),
        });
    }
    assert!(
        compute_readiness(&checks),
        "an offline host, or a host with no curl, is still able to inspect and build locally"
    );
}

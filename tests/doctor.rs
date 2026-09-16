use archbridge::doctor::{
    compute_readiness, evaluate_network_probe_result, run_doctor_with_probe, CheckResult,
    DEFAULT_NETWORK_ENDPOINT,
};
use archbridge::process::ProcessOutput;
use archbridge::rpc::{JsonRpcRequest, RpcServer};
use serde_json::json;

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

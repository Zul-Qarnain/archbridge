use archbridge::rpc::{JsonRpcRequest, RpcServer};
use serde_json::json;
use std::process::Command;

fn cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_archbridge"))
        .args(args)
        .output()
        .expect("the archbridge binary must be runnable from tests")
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn unknown_flags_are_a_usage_error_instead_of_a_silent_real_run() {
    // `--dryrun` used to fall on the floor, so the install ran for real.
    let out = cli(&["install", "--dryrun", "coreutils"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr_of(&out));
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains("Unknown flag '--dryrun'"),
        "expected the flag to be named: {}",
        stderr
    );
}

#[test]
fn missing_required_arguments_exit_with_usage_code() {
    for args in [
        &["build"][..],
        &["test"][..],
        &["search"][..],
        &["install"][..],
        &["inspect"][..],
        &["config", "set"][..],
    ] {
        let out = cli(args);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?} must be reported as a usage error"
        );
    }
}

#[test]
fn a_blocked_plan_exits_3_and_never_executes() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("README"), "no build system here").expect("write README");

    let out = cli(&["build", dir.path().to_str().unwrap(), "--yes"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "blocked plan: {}",
        stderr_of(&out)
    );
}

#[test]
fn a_successful_query_exits_0() {
    let out = cli(&["config", "get", "aur"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
}

#[test]
fn test_rpc_capabilities() {
    let mut server = RpcServer::new();
    let req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: "v1.capabilities".to_string(),
        params: json!({}),
    };
    let resp = server.handle_request(req);
    assert!(resp.error.is_none());
    let res = resp.result.unwrap();
    assert_eq!(res.get("foreign_script_execution").unwrap(), false);
}

#[test]
fn test_rpc_config_get_set() {
    // A sandboxed config file: the previous version of this test wrote
    // `aur: false` into the developer's real ~/.config/archbridge/config.json.
    let dir = tempfile::tempdir().expect("tempdir");
    let config_path = dir.path().join("config.json");
    let mut server = RpcServer::with_config_path(&config_path);
    let req_set = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(2)),
        method: "v1.prepare".to_string(),
        params: json!({
            "action": "config.set",
            "key": "aur",
            "value": "false"
        }),
    };
    let resp = server.handle_request(req_set);
    assert!(resp.error.is_none());
    let plan = resp.result.unwrap();
    let plan_id = plan.get("plan_id").unwrap().as_str().unwrap();
    assert_eq!(
        plan.get("filesystem_changes")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("path"))
            .and_then(|p| p.as_str()),
        Some(config_path.to_str().unwrap()),
        "the plan must name the file execution writes"
    );

    let req_exec = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(3)),
        method: "v1.execute".to_string(),
        params: json!({
            "plan_id": plan_id,
            "confirmed": true
        }),
    };
    let resp_exec = server.handle_request(req_exec);
    assert!(resp_exec.error.is_none());

    let req_get = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(4)),
        method: "v1.config.get".to_string(),
        params: json!({ "key": "aur" }),
    };
    let resp_get = server.handle_request(req_get);
    assert_eq!(resp_get.result.unwrap(), false);

    // The change has to survive a reload, otherwise the save never happened.
    let reloaded = RpcServer::with_config_path(&config_path);
    assert!(!reloaded.engine.config.aur);
    drop(dir);
}

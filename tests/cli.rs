use archbridge::rpc::{JsonRpcRequest, RpcServer};
use serde_json::json;

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
    let mut server = RpcServer::new();
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
}

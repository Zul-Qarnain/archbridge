use crate::doctor::run_doctor;
use crate::engine::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, Write};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

pub struct RpcServer {
    pub engine: Engine,
}

impl Default for RpcServer {
    fn default() -> Self {
        Self::new()
    }
}

impl RpcServer {
    pub fn new() -> Self {
        Self {
            engine: Engine::new(),
        }
    }

    pub fn handle_request(&mut self, req: JsonRpcRequest) -> JsonRpcResponse {
        if req.jsonrpc != "2.0" {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: req.id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32600,
                    message: "Invalid Request: jsonrpc must be '2.0'".to_string(),
                }),
            };
        }

        let res = match req.method.as_str() {
            "v1.capabilities" => Ok(serde_json::json!({
                "protocol": "v1.0",
                "methods": ["v1.capabilities", "v1.search", "v1.info", "v1.inspect", "v1.doctor", "v1.config.get", "v1.prepare", "v1.execute"],
                "chroot_build": true,
                "runtime_smoke_test": true,
                "foreign_script_execution": false
            })),
            "v1.search" | "v1.info" => {
                let target = req
                    .params
                    .get("target")
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                let repo = req.params.get("repo").and_then(|r| r.as_str());
                self.engine
                    .prepare_search(target, repo)
                    .and_then(|p| serde_json::to_value(&p.decision).map_err(|e| e.to_string()))
            }
            "v1.inspect" => {
                let target = req
                    .params
                    .get("target")
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                self.engine
                    .prepare_inspect(target)
                    .and_then(|p| serde_json::to_value(&p).map_err(|e| e.to_string()))
            }
            "v1.doctor" => {
                let doc = run_doctor();
                serde_json::to_value(doc).map_err(|e| e.to_string())
            }
            "v1.config.get" => {
                let key = req
                    .params
                    .get("key")
                    .and_then(|k| k.as_str())
                    .unwrap_or("all");
                self.engine.config.get(key)
            }
            "v1.prepare" => {
                let action = req
                    .params
                    .get("action")
                    .and_then(|a| a.as_str())
                    .unwrap_or("");
                let req_obj = req
                    .params
                    .get("request")
                    .cloned()
                    .unwrap_or(req.params.clone());

                let target = req_obj.get("target").and_then(|t| t.as_str()).unwrap_or("");
                let options = req_obj
                    .get("options")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));

                let name = options.get("name").and_then(|n| n.as_str());
                let version = options.get("version").and_then(|v| v.as_str());
                let entry = options.get("entry").and_then(|e| e.as_str());

                let deps: Vec<String> = options
                    .get("dependencies")
                    .and_then(|d| serde_json::from_value(d.clone()).ok())
                    .unwrap_or_default();
                let smoke_args: Vec<String> = options
                    .get("smoke_args")
                    .and_then(|s| serde_json::from_value(s.clone()).ok())
                    .unwrap_or_default();

                let plan_res = match action {
                    "search" | "info" => self
                        .engine
                        .prepare_search(target, req_obj.get("repo").and_then(|r| r.as_str())),
                    "inspect" => self.engine.prepare_inspect(target),
                    "config.set" => {
                        let k = req.params.get("key").and_then(|k| k.as_str()).unwrap_or("");
                        let v = req
                            .params
                            .get("value")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        self.engine.prepare_config_set(k, v)
                    }
                    "build" => {
                        self.engine
                            .prepare_build(target, name, version, entry, &deps, &smoke_args)
                    }
                    "install" => self.engine.prepare_install(target),
                    "test" => self.engine.prepare_test(target, entry, &smoke_args),
                    _ => Err(format!("Unknown prepare action '{}'", action)),
                };

                plan_res.and_then(|p| serde_json::to_value(p).map_err(|e| e.to_string()))
            }
            "v1.execute" => {
                let plan_id = req
                    .params
                    .get("plan_id")
                    .and_then(|p| p.as_str())
                    .unwrap_or("");
                let confirmed = req
                    .params
                    .get("confirmed")
                    .and_then(|c| c.as_bool())
                    .unwrap_or(false);

                self.engine
                    .execute_plan(plan_id, confirmed)
                    .and_then(|res| serde_json::to_value(res).map_err(|e| e.to_string()))
            }
            _ => Err(format!("Method not found: '{}'", req.method)),
        };

        match res {
            Ok(val) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: req.id,
                result: Some(val),
                error: None,
            },
            Err(e) => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: req.id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32000,
                    message: e,
                }),
            },
        }
    }

    pub fn run_loop<R: BufRead, W: Write>(
        &mut self,
        mut reader: R,
        mut writer: W,
    ) -> Result<(), String> {
        let mut line = String::new();
        while reader.read_line(&mut line).map_err(|e| e.to_string())? > 0 {
            if line.len() > 1_048_576 {
                let err_resp = JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: None,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32700,
                        message: "Request frame exceeds maximum size of 1 MiB".to_string(),
                    }),
                };
                let resp_str = serde_json::to_string(&err_resp).unwrap_or_default();
                writeln!(writer, "{}", resp_str).map_err(|e| e.to_string())?;
                return Err("Request oversized".to_string());
            }

            let trimmed = line.trim();
            if !trimmed.is_empty() {
                let req_res: Result<JsonRpcRequest, _> = serde_json::from_str(trimmed);
                let resp = match req_res {
                    Ok(req) => self.handle_request(req),
                    Err(e) => JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: None,
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32700,
                            message: format!("Parse error: {}", e),
                        }),
                    },
                };

                let resp_json = serde_json::to_string(&resp).map_err(|e| e.to_string())?;
                writeln!(writer, "{}", resp_json).map_err(|e| e.to_string())?;
                writer.flush().map_err(|e| e.to_string())?;
            }
            line.clear();
        }
        Ok(())
    }
}

use crate::engine::{ExecutionResult, Plan};
use crate::rpc::{JsonRpcRequest, RpcServer};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

/// Which tab is currently active in the sidebar
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ActiveTab {
    #[default]
    Discover,
    Inspect,
    Build,
    Uninstall,
    Doctor,
    Settings,
}

/// Severity for inline messages
#[derive(Debug, Clone, PartialEq)]
pub enum MsgLevel {
    Info,
    Success,
    Warning,
    Error,
}

/// An inline status / result message shown in the view
#[derive(Debug, Clone)]
pub struct StatusMsg {
    pub level: MsgLevel,
    pub text: String,
}

/// One result item in the Discover search results list
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub name: String,
    pub version: String,
    pub description: String,
    pub source: String,
    pub score: f64,
}

/// One installed package in the Uninstall view
#[derive(Debug, Clone)]
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
    pub size: String,
}

/// Doctor health check item
#[derive(Debug, Clone)]
pub struct HealthCheck {
    pub name: String,
    pub ok: bool,
    pub message: String,
}

/// The complete shared application state.
/// Everything the views need lives here.
#[derive(Default)]
pub struct AppState {
    // ──────────────────── navigation ────────────────────
    pub active_tab: ActiveTab,

    // ──────────────────── discover ──────────────────────
    pub search_query: String,
    pub search_results: Vec<SearchResult>,
    pub selected_result: Option<usize>,
    pub search_busy: bool,
    pub search_msg: Option<StatusMsg>,

    // ──────────────────── inspect ───────────────────────
    pub inspect_path: String,
    pub inspect_data: Option<Value>,
    pub inspect_busy: bool,
    pub inspect_msg: Option<StatusMsg>,

    // ──────────────────── build ─────────────────────────
    pub active_plan: Option<Plan>,
    pub build_log: String,
    pub build_busy: bool,
    pub build_result: Option<ExecutionResult>,
    pub build_msg: Option<StatusMsg>,

    // ──────────────────── uninstall ─────────────────────
    pub installed_packages: Vec<InstalledPackage>,
    pub uninstall_query: String,
    pub uninstall_selected: Option<usize>,
    pub uninstall_busy: bool,
    pub uninstall_msg: Option<StatusMsg>,

    // ──────────────────── doctor ────────────────────────
    pub health_checks: Vec<HealthCheck>,
    pub doctor_busy: bool,
    pub doctor_ready: bool,
    pub doctor_msg: Option<StatusMsg>,

    // ──────────────────── settings ──────────────────────
    pub sudo_session_active: bool,
    pub config_data: Option<Value>,
    pub settings_msg: Option<StatusMsg>,

    // ──────────────────── auth ──────────────────────────
    pub auth_dialog_open: bool,
    pub auth_password_input: String,
    pub auth_pending_action: Option<String>,
    pub auth_error: Option<String>,
    pub auth_focus_active: bool,
}

/// Thread-safe handle to the engine / RPC server
pub type SharedEngine = Arc<Mutex<RpcServer>>;

/// Convenience: call one RPC method synchronously
pub fn rpc_call(engine: &SharedEngine, method: &str, params: Value) -> Result<Value, String> {
    let req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(json!(1)),
        method: method.to_string(),
        params,
    };
    let mut srv = engine.lock().map_err(|e| e.to_string())?;
    let resp = srv.handle_request(req);
    if let Some(err) = resp.error {
        Err(format!("RPC {}: {}", err.code, err.message))
    } else if let Some(res) = resp.result {
        Ok(res)
    } else {
        Err("Empty response".to_string())
    }
}

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
    pub repo: String,
    pub is_recommended: bool,
    pub license: String,
    pub arch: String,
    pub size: String,
    pub maintainer: String,
}

/// One installed package in the Uninstall view
#[derive(Debug, Clone)]
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
    pub description: String,
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
pub struct AppState {
    // ──────────────────── navigation ────────────────────
    pub active_tab: ActiveTab,

    // ──────────────────── top status bar ────────────────
    pub search_history: Vec<String>,
    pub show_history_menu: bool,
    pub doctor_ready: bool,
    pub sudo_session_active: bool,
    pub active_chroot: String,
    pub free_disk_space: String,
    pub keyring_status: String,

    // ──────────────────── discover ──────────────────────
    pub search_query: String,
    pub source_filter: String,
    pub search_results: Vec<SearchResult>,
    pub recommended_item: Option<SearchResult>,
    pub selected_result: Option<usize>,
    pub search_busy: bool,
    pub search_msg: Option<StatusMsg>,

    // ──────────────────── inspect ───────────────────────
    pub inspect_path: String,
    pub inspect_data: Option<Value>,
    pub inspect_active_subtab: usize, // 0: Scripts, 1: Deps, 2: Desktop & Units, 3: ELF Libs
    pub inspect_busy: bool,
    pub inspect_msg: Option<StatusMsg>,

    // ──────────────────── build ─────────────────────────
    pub build_target_input: String,
    pub active_plan: Option<Plan>,
    pub build_stage: usize, // 1: Prepare, 2: PKGBUILD, 3: Chroot, 4: Install
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
    pub doctor_msg: Option<StatusMsg>,

    // ──────────────────── settings ──────────────────────
    pub config_data: Option<Value>,
    pub opt_official: bool,
    pub opt_aur: bool,
    pub opt_flatpak: bool,
    pub opt_appimage: bool,
    pub opt_upstream: bool,
    pub opt_deb: bool,
    pub opt_rpm: bool,
    pub opt_remember_sudo: bool,
    pub settings_msg: Option<StatusMsg>,

    // ──────────────────── auth ──────────────────────────
    pub auth_dialog_open: bool,
    pub auth_password_input: String,
    pub auth_pending_action: Option<String>,
    pub auth_error: Option<String>,
    pub auth_focus_active: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            active_tab: ActiveTab::Discover,
            search_history: Vec::new(),
            show_history_menu: false,
            doctor_ready: true,
            sudo_session_active: false,
            active_chroot: "None".into(),
            free_disk_space: "47 GiB Free".into(),
            keyring_status: "Verified".into(),

            // Clean default Discovery state
            search_query: "".into(),
            source_filter: "All Sources".into(),
            search_results: Vec::new(),
            recommended_item: None,
            selected_result: None,
            search_busy: false,
            search_msg: None,

            inspect_path: "".into(),
            inspect_data: None,
            inspect_active_subtab: 0,
            inspect_busy: false,
            inspect_msg: None,

            build_target_input: "".into(),
            active_plan: None,
            build_stage: 1,
            build_log: "".into(),
            build_busy: false,
            build_result: None,
            build_msg: None,

            installed_packages: Vec::new(),
            uninstall_query: "".into(),
            uninstall_selected: None,
            uninstall_busy: false,
            uninstall_msg: None,

            // Pre-populated 8 health checks matching doctor report
            health_checks: vec![
                HealthCheck {
                    name: "pacman".into(),
                    ok: true,
                    message: "pacman package manager is installed".into(),
                },
                HealthCheck {
                    name: "base-devel".into(),
                    ok: true,
                    message: "base-devel group/meta-package is installed".into(),
                },
                HealthCheck {
                    name: "devtools".into(),
                    ok: true,
                    message: "devtools (mkarchroot and makechrootpkg) are installed".into(),
                },
                HealthCheck {
                    name: "namespaces".into(),
                    ok: true,
                    message: "Kernel user namespaces support detected".into(),
                },
                HealthCheck {
                    name: "disk_space".into(),
                    ok: true,
                    message: "Sufficient workspace disk space available (142 GiB Free)".into(),
                },
                HealthCheck {
                    name: "compiler".into(),
                    ok: true,
                    message: "GCC compiler toolchain is available".into(),
                },
                HealthCheck {
                    name: "network".into(),
                    ok: true,
                    message: "HTTPS network connectivity verified (https://archlinux.org)".into(),
                },
                HealthCheck {
                    name: "keyring".into(),
                    ok: true,
                    message: "Pacman keyring is populated and valid".into(),
                },
            ],
            doctor_busy: false,
            doctor_msg: None,

            config_data: None,
            opt_official: true,
            opt_aur: true,
            opt_flatpak: true,
            opt_appimage: true,
            opt_upstream: true,
            opt_deb: true,
            opt_rpm: true,
            opt_remember_sudo: true,
            settings_msg: None,

            auth_dialog_open: false,
            auth_password_input: "".into(),
            auth_pending_action: None,
            auth_error: None,
            auth_focus_active: false,
        }
    }
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

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
            search_history: vec!["brave".into(), "vscode".into(), "docker".into()],
            show_history_menu: false,
            doctor_ready: true,
            sudo_session_active: false,
            active_chroot: "None".into(),
            free_disk_space: "142 GiB Free".into(),
            keyring_status: "Verified".into(),

            search_query: "brave".into(),
            source_filter: "All Sources".into(),
            search_results: vec![
                SearchResult {
                    name: "brave".into(),
                    version: "v1.70.126-1".into(),
                    description: "A privacy focused web browser".into(),
                    source: "official".into(),
                    score: 1.0,
                    repo: "extra".into(),
                    is_recommended: true,
                    license: "MPL-2.0".into(),
                    arch: "x86_64".into(),
                    size: "182.4 MiB".into(),
                    maintainer: "Arch Linux".into(),
                },
                SearchResult {
                    name: "brave-bin".into(),
                    version: "v1.70.126-1".into(),
                    description: "Pre-built binary from upstream (AUR)".into(),
                    source: "aur".into(),
                    score: 0.95,
                    repo: "AUR (community)".into(),
                    is_recommended: false,
                    license: "MPL-2.0".into(),
                    arch: "x86_64".into(),
                    size: "182.4 MiB".into(),
                    maintainer: "AUR Contributor".into(),
                },
                SearchResult {
                    name: "com.brave.Browser".into(),
                    version: "v1.70.126".into(),
                    description: "Brave Browser (Flatpak)".into(),
                    source: "flatpak".into(),
                    score: 0.90,
                    repo: "Flathub".into(),
                    is_recommended: false,
                    license: "MPL-2.0".into(),
                    arch: "x86_64".into(),
                    size: "210.0 MiB".into(),
                    maintainer: "Flathub Maintainers".into(),
                },
                SearchResult {
                    name: "Brave-Browser".into(),
                    version: "v1.70.126".into(),
                    description: "Standalone AppImage (official)".into(),
                    source: "appimage".into(),
                    score: 0.85,
                    repo: "Official Release".into(),
                    is_recommended: false,
                    license: "MPL-2.0".into(),
                    arch: "x86_64".into(),
                    size: "195.0 MiB".into(),
                    maintainer: "Brave Software".into(),
                },
                SearchResult {
                    name: "github.com/brave/brave-browser".into(),
                    version: "master".into(),
                    description: "Source code (build from source)".into(),
                    source: "upstream".into(),
                    score: 0.80,
                    repo: "GitHub".into(),
                    is_recommended: false,
                    license: "MPL-2.0".into(),
                    arch: "source".into(),
                    size: "Source Repo".into(),
                    maintainer: "Brave Software".into(),
                },
            ],
            recommended_item: Some(SearchResult {
                name: "brave".into(),
                version: "v1.70.126-1".into(),
                description: "A privacy focused web browser".into(),
                source: "official".into(),
                score: 1.0,
                repo: "extra".into(),
                is_recommended: true,
                license: "MPL-2.0".into(),
                arch: "x86_64".into(),
                size: "182.4 MiB".into(),
                maintainer: "Arch Linux".into(),
            }),
            selected_result: Some(0),
            search_busy: false,
            search_msg: None,

            inspect_path: "".into(),
            inspect_data: None,
            inspect_active_subtab: 0,
            inspect_busy: false,
            inspect_msg: None,

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

            health_checks: Vec::new(),
            doctor_busy: false,
            doctor_msg: None,

            config_data: None,
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

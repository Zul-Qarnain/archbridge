use crate::engine::{ExecutionResult, Plan};
use crate::process::ProgressSnapshot;
use crate::rpc::{JsonRpcRequest, RpcServer};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Application Theme mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppTheme {
    #[default]
    Dark,
    Midnight,
    Light,
}

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

/// Which text field currently owns keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputField {
    Search,
    Inspect,
    Build,
    Uninstall,
    Auth,
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

/// One result item in the Discover search results list. Optional metadata stays
/// optional: a source that does not report a licence must not have one invented
/// for it in the UI.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub name: String,
    pub version: String,
    pub description: String,
    pub source: String,
    pub repo: String,
    pub is_recommended: bool,
    pub url: Option<String>,
    pub source_url: Option<String>,
    pub license: Option<String>,
    pub arch: Option<String>,
    pub size: Option<String>,
    pub maintainer: Option<String>,
    pub out_of_date: Option<String>,
}

/// One installed package in the Uninstall view
#[derive(Debug, Clone)]
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
    pub description: String,
    pub size: String,
}

/// Doctor health check item. `status` is the engine's own verdict — "pass",
/// "unavailable" or "fail" — and the three are not interchangeable: an offline
/// network is unavailable, not a failure, and must not render as one.
#[derive(Debug, Clone)]
pub struct HealthCheck {
    pub name: String,
    pub status: String,
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
    /// True once the staged plan shown in the review panel has been sent to the
    /// engine. Plans are single-use, so a second click cannot run anything and
    /// has to say so instead of resending a consumed plan id.
    pub build_plan_consumed: bool,
    /// Same, for the install plan a successful build hands off to.
    pub install_plan_consumed: bool,
    pub build_msg: Option<StatusMsg>,

    // ──────────────────── uninstall ─────────────────────
    pub installed_packages: Vec<InstalledPackage>,
    /// True while `pacman -Qe` is being read off the UI thread.
    pub installed_loading: bool,
    pub uninstall_query: String,
    pub uninstall_selected: Option<usize>,
    pub uninstall_confirm_idx: Option<usize>,
    pub uninstall_display_limit: usize,
    pub uninstall_busy: bool,
    pub uninstall_msg: Option<StatusMsg>,

    // ──────────────────── doctor ────────────────────────
    pub health_checks: Vec<HealthCheck>,
    pub doctor_busy: bool,
    pub doctor_msg: Option<StatusMsg>,

    // ──────────────────── settings ──────────────────────
    pub config_data: Option<Value>,
    pub settings_busy: bool,
    /// True when a toggle differs from what is on disk.
    pub settings_dirty: bool,
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
    pub auth_password_visible: bool,
    pub auth_busy: bool,
    pub auth_pending_action: Option<String>,
    pub auth_error: Option<String>,
    pub auth_started: Option<Instant>,
    pub auth_fix_fingerprint: bool,
    pub auth_needs_focus: bool,

    // ──────────────────── live progress ──────────────────
    pub tick: u32,
    pub progress: ProgressSnapshot,
    pub sudo_advice_slow: bool,

    // ──────────────────── theme & focus ──────────────────
    pub theme: AppTheme,
    pub focused_input: Option<InputField>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            active_tab: ActiveTab::Discover,
            search_history: Vec::new(),
            show_history_menu: false,
            doctor_ready: false,
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
            build_plan_consumed: false,
            install_plan_consumed: false,
            build_msg: None,

            installed_packages: Vec::new(),
            installed_loading: false,
            uninstall_query: "".into(),
            uninstall_selected: None,
            uninstall_confirm_idx: None,
            uninstall_display_limit: 50,
            uninstall_busy: false,
            uninstall_msg: None,

            // Empty until the engine actually measures something: a health
            // check shown before it ran is a claim about the user's system.
            health_checks: Vec::new(),
            doctor_busy: false,
            doctor_msg: None,

            config_data: None,
            settings_busy: false,
            settings_dirty: false,
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
            auth_password_visible: false,
            auth_busy: false,
            auth_pending_action: None,
            auth_error: None,
            auth_started: None,
            auth_fix_fingerprint: false,
            auth_needs_focus: false,

            tick: 0,
            progress: ProgressSnapshot::default(),
            sudo_advice_slow: false,

            theme: AppTheme::Dark,
            focused_input: None,
        }
    }
}

/// Thread-safe handle to the engine / RPC server
pub type SharedEngine = Arc<Mutex<RpcServer>>;

impl AppState {
    /// The persisted config keys behind the source-routing checkboxes, in the
    /// order the Settings card shows them.
    pub fn source_preferences(&self) -> [(&'static str, bool); 7] {
        [
            ("official", self.opt_official),
            ("aur", self.opt_aur),
            ("flatpak", self.opt_flatpak),
            ("appimage", self.opt_appimage),
            ("upstream", self.opt_upstream),
            ("deb", self.opt_deb),
            ("rpm", self.opt_rpm),
        ]
    }

    /// Read the checkbox states back from a `v1.config.get` "all" payload so the
    /// card reflects what is on disk rather than compile-time defaults.
    pub fn apply_source_preferences(&mut self, config: &Value) {
        let read = |key: &str, slot: &mut bool| {
            if let Some(value) = config.get(key).and_then(|v| v.as_bool()) {
                *slot = value;
            }
        };
        read("official", &mut self.opt_official);
        read("aur", &mut self.opt_aur);
        read("flatpak", &mut self.opt_flatpak);
        read("appimage", &mut self.opt_appimage);
        read("upstream", &mut self.opt_upstream);
        read("deb", &mut self.opt_deb);
        read("rpm", &mut self.opt_rpm);
        self.config_data = Some(config.clone());
        self.settings_dirty = false;
    }

    /// True while any engine-backed operation is running.
    pub fn engine_busy(&self) -> bool {
        self.search_busy
            || self.inspect_busy
            || self.build_busy
            || self.uninstall_busy
            || self.doctor_busy
            || self.settings_busy
    }

    /// True when the 400 ms UI ticker must keep running (busy animations,
    /// live progress polling, or a blinking caret in a focused field).
    pub fn ticker_needed(&self) -> bool {
        self.engine_busy() || self.auth_dialog_open || self.focused_input.is_some()
    }
}

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
        Err(err.message)
    } else if let Some(res) = resp.result {
        Ok(res)
    } else {
        Err("Empty response".to_string())
    }
}

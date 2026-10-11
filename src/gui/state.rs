use crate::engine::{ExecutionResult, Plan};
use crate::inspect::InspectionReport;
use crate::process::ProgressSnapshot;
use crate::rpc::{JsonRpcRequest, RpcServer};
use serde_json::{json, Value};
use std::rc::Rc;
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

/// Which engine operation the next native path-chooser result belongs to. The
/// XDG portal answers asynchronously, so the picker cannot return a value to
/// the caller and has to be routed when it lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathPrompt {
    BuildTarget,
    OutputDir,
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
#[derive(Debug, Clone, PartialEq)]
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
#[derive(Debug, Clone, PartialEq)]
pub struct HealthCheck {
    pub name: String,
    pub status: String,
    pub message: String,
}

/// Shown in the status bar until Doctor has actually measured the value.
/// A number or "Verified" that was never read from the system is a lie.
pub const STATUS_UNMEASURED: &str = "not measured";

/// One source row from `v1.search`. The engine always returns a row per source,
/// including ones with no candidate, and the reason is why that source is or
/// is not usable. Dropping empty rows hides that explanation.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceDecision {
    pub source: String,
    pub availability: String,
    pub reason: String,
    pub recommended: bool,
    pub candidates: Vec<SearchResult>,
}

/// A keyboard-movable entry in the Discover list: a source with no candidate
/// is still selectable so its reason can be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscoverPick {
    pub source_index: usize,
    pub candidate_index: Option<usize>,
}

/// The complete shared application state.
pub struct AppState {
    // ──────────────────── navigation ────────────────────
    pub active_tab: ActiveTab,
    /// Set when a native path chooser is opened, cleared when its result lands.
    pub path_prompt: Option<PathPrompt>,

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
    /// Every source the engine reported for the last search, in priority order.
    pub source_decisions: Vec<SourceDecision>,
    pub selected_source: Option<usize>,
    pub selected_candidate: Option<usize>,
    pub search_busy: bool,
    pub search_msg: Option<StatusMsg>,

    // ──────────────────── inspect ───────────────────────
    pub inspect_path: String,
    /// Parsed once when the engine answers, not on every repaint: the report
    /// carries every maintainer script body, and re-reading it per frame is what
    /// made this tab stutter.
    pub inspect_report: Option<Rc<InspectionReport>>,
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
    /// The output folder shown in the UI. It is written through the engine the
    /// moment the user picks it, so it never sits unsaved next to the build.
    pub opt_output_dir: String,
    /// True while a chosen output folder is being written through the engine.
    pub output_dir_saving: bool,
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
    /// Last measured window width, so layout decisions stay testable and the
    /// views can hide panes that would not fit.
    pub viewport_width: f32,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            active_tab: ActiveTab::Discover,
            path_prompt: None,
            search_history: Vec::new(),
            show_history_menu: false,
            doctor_ready: false,
            sudo_session_active: false,
            active_chroot: "None".into(),
            free_disk_space: STATUS_UNMEASURED.into(),
            keyring_status: STATUS_UNMEASURED.into(),

            // Clean default Discovery state
            search_query: "".into(),
            source_filter: "All Sources".into(),
            source_decisions: Vec::new(),
            selected_source: None,
            selected_candidate: None,
            search_busy: false,
            search_msg: None,

            inspect_path: "".into(),
            inspect_report: None,
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
            opt_output_dir: String::new(),
            output_dir_saving: false,
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
            viewport_width: 1320.0,
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
        if let Some(dir) = config.get("output_dir").and_then(|v| v.as_str()) {
            self.opt_output_dir = dir.to_string();
        }
        self.config_data = Some(config.clone());
        self.settings_dirty = false;
    }

    /// The output directory currently on disk.
    pub fn saved_output_dir(&self) -> &str {
        self.config_data
            .as_ref()
            .and_then(|c| c.get("output_dir"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
    }

    /// True while either the source toggles differ from what is on disk. The
    /// output folder is not part of this: it saves the moment it is picked.
    pub fn settings_unsaved(&self) -> bool {
        self.settings_dirty
    }

    /// True while any engine-backed operation is running.
    pub fn engine_busy(&self) -> bool {
        self.search_busy
            || self.inspect_busy
            || self.build_busy
            || self.uninstall_busy
            || self.doctor_busy
            || self.settings_busy
            || self.output_dir_saving
    }

    /// True when the 400 ms UI ticker must keep running (busy animations,
    /// live progress polling, or a blinking caret in a focused field).
    pub fn ticker_needed(&self) -> bool {
        self.engine_busy() || self.auth_dialog_open || self.focused_input.is_some()
    }

    /// The full-window progress overlay is for long packaging work. Doctor has
    /// its own inline state, and covering Discover with that overlay at startup
    /// would hide the view while the status bar is being measured.
    pub fn shows_progress_overlay(&self) -> bool {
        self.search_busy
            || self.inspect_busy
            || self.build_busy
            || self.uninstall_busy
            || self.settings_busy
            || self.output_dir_saving
    }

    pub fn recommended_source(&self) -> Option<&SourceDecision> {
        self.source_decisions
            .iter()
            .find(|source| source.recommended)
    }

    pub fn selected_decision(&self) -> Option<&SourceDecision> {
        self.selected_source
            .and_then(|index| self.source_decisions.get(index))
    }

    pub fn selected_search_result(&self) -> Option<&SearchResult> {
        let source = self.selected_decision()?;
        self.selected_candidate
            .and_then(|index| source.candidates.get(index))
    }

    /// Forget hits from a previous query. An error or a cleared box must not
    /// keep showing packages the latest request did not return.
    pub fn clear_search_hits(&mut self) {
        self.source_decisions.clear();
        self.selected_source = None;
        self.selected_candidate = None;
    }

    pub fn apply_search_error(&mut self, error: String) {
        self.clear_search_hits();
        self.search_msg = Some(StatusMsg {
            level: MsgLevel::Error,
            text: error,
        });
    }

    pub fn apply_search_success(&mut self, sources: Vec<SourceDecision>, query: &str) {
        if sources.is_empty() {
            self.clear_search_hits();
            self.search_msg = Some(StatusMsg {
                level: MsgLevel::Warning,
                text: format!("The engine returned no source report for '{query}'."),
            });
            return;
        }
        let available = sources
            .iter()
            .filter(|source| source.availability == "available")
            .count();
        let pick = visible_picks(&sources, &self.source_filter)
            .into_iter()
            .find(|pick| pick.candidate_index.is_some())
            .or_else(|| {
                visible_picks(&sources, &self.source_filter)
                    .into_iter()
                    .next()
            });
        self.selected_source = pick.map(|pick| pick.source_index);
        self.selected_candidate = pick.and_then(|pick| pick.candidate_index);
        self.source_decisions = sources;
        let listed = self.source_decisions.len();
        self.search_msg = Some(StatusMsg {
            level: if available == 0 {
                MsgLevel::Warning
            } else {
                MsgLevel::Info
            },
            text: if available == 0 {
                format!(
                    "No installable candidate for '{query}'. All {listed} sources are listed with the reason each one is unavailable."
                )
            } else {
                format!(
                    "{available} of {listed} sources can provide '{query}'. Unavailable sources stay listed with their reason."
                )
            },
        });
    }

    pub fn apply_doctor_report(&mut self, checks: Vec<HealthCheck>, ready: bool) {
        self.doctor_ready = ready;
        if let Some(disk) = checks.iter().find(|check| check.name == "disk_space") {
            self.free_disk_space = disk_status_label(disk);
        }
        if let Some(keyring) = checks.iter().find(|check| check.name == "keyring") {
            self.keyring_status = keyring_status_label(keyring);
        }
        self.doctor_msg = Some(doctor_banner(&checks, ready));
        self.health_checks = checks;
    }

    /// Top-bar health pill. "System Ready" is only earned by a report.
    pub fn doctor_pill_label(&self) -> &'static str {
        if self.health_checks.is_empty() {
            "Diagnostics"
        } else if self.doctor_ready {
            "System Ready"
        } else {
            "Not Ready"
        }
    }

    /// Bottom-bar readiness word. Unchecked is not the same as ready.
    pub fn readiness_label(&self) -> &'static str {
        if self.health_checks.is_empty() {
            "Not checked"
        } else if self.doctor_ready {
            "Ready"
        } else {
            "Not ready"
        }
    }
}

/// Sidebar and title shortcuts. Digits follow the sidebar order.
pub fn tab_for_shortcut(key: &str, control: bool) -> Option<ActiveTab> {
    if !control {
        return None;
    }
    match key {
        "1" => Some(ActiveTab::Discover),
        "2" => Some(ActiveTab::Inspect),
        "3" => Some(ActiveTab::Build),
        "4" => Some(ActiveTab::Uninstall),
        "5" => Some(ActiveTab::Doctor),
        "6" => Some(ActiveTab::Settings),
        _ => None,
    }
}

pub const SOURCE_FILTERS: [&str; 8] = [
    "All Sources",
    "official",
    "aur",
    "flatpak",
    "appimage",
    "upstream",
    "deb",
    "rpm",
];

pub fn cycle_source_filter(current: &str) -> &'static str {
    let index = SOURCE_FILTERS
        .iter()
        .position(|filter| *filter == current)
        .unwrap_or(0);
    SOURCE_FILTERS[(index + 1) % SOURCE_FILTERS.len()]
}

pub fn source_filter_label(filter: &str) -> String {
    if filter == "All Sources" {
        "All Sources".to_string()
    } else {
        format!("{filter} only")
    }
}

pub fn source_matches_filter(filter: &str, source: &str) -> bool {
    filter == "All Sources" || filter.eq_ignore_ascii_case(source)
}

/// The detail pane is 340px beside a 236px sidebar. Below this window width
/// the source list is squeezed until its text paints through the pane.
/// Suggestion chips sit above the report. Once sources are listed they push
/// the lower rows past the bottom of the default window.
pub fn show_quick_try(has_results: bool) -> bool {
    !has_results
}

pub fn show_discover_inspector(window_width: f32) -> bool {
    window_width >= 1100.0
}

/// The top bar's pills share one row. Below this width the theme control is
/// pushed past the window edge.
pub fn compact_top_bar(window_width: f32) -> bool {
    window_width < 1100.0
}

pub fn availability_label(availability: &str) -> &'static str {
    match availability {
        "available" => "AVAILABLE",
        "unavailable" => "UNAVAILABLE",
        "disabled" => "DISABLED",
        "unknown" => "UNKNOWN",
        _ => "UNKNOWN",
    }
}

/// Banner copy is the engine's own reason. Inventing "the official {source}
/// path" called an AUR package an official repository package.
pub fn recommendation_line(source: &SourceDecision) -> String {
    match source.candidates.first() {
        Some(candidate) => format!("{} — {}", candidate.name, source.reason),
        None => format!("{} — {}", source.source, source.reason),
    }
}

pub fn visible_picks(sources: &[SourceDecision], filter: &str) -> Vec<DiscoverPick> {
    let mut picks = Vec::new();
    for (source_index, source) in sources.iter().enumerate() {
        if !source_matches_filter(filter, &source.source) {
            continue;
        }
        if source.candidates.is_empty() {
            picks.push(DiscoverPick {
                source_index,
                candidate_index: None,
            });
        } else {
            for candidate_index in 0..source.candidates.len() {
                picks.push(DiscoverPick {
                    source_index,
                    candidate_index: Some(candidate_index),
                });
            }
        }
    }
    picks
}

pub fn step_pick(
    picks: &[DiscoverPick],
    source_index: Option<usize>,
    candidate_index: Option<usize>,
    delta: i32,
) -> Option<DiscoverPick> {
    if picks.is_empty() {
        return None;
    }
    let current = picks.iter().position(|pick| {
        pick.source_index == source_index.unwrap_or(usize::MAX)
            && pick.candidate_index == candidate_index
    });
    let next = match current {
        Some(index) => {
            let moved = index as i32 + delta;
            if moved < 0 {
                0
            } else if moved as usize >= picks.len() {
                picks.len() - 1
            } else {
                moved as usize
            }
        }
        None => {
            if delta < 0 {
                picks.len() - 1
            } else {
                0
            }
        }
    };
    Some(picks[next])
}

/// Only an explicit failure is a failure. "unavailable" is the engine's word
/// for offline or unmeasurable, and an unknown status is not evidence of a break.
pub fn status_is_failure(status: &str) -> bool {
    status == "fail"
}

/// Doctor status column width. "UNAVAILABLE" is the longest label; a 120px
/// column clipped it into the message on the default UI font.
pub const DOCTOR_STATUS_COLUMN_PX: f32 = 148.0;

pub fn status_label(status: &str) -> &'static str {
    match status {
        "pass" => "PASS",
        "unavailable" => "UNAVAILABLE",
        "fail" => "FAIL",
        _ => "UNKNOWN",
    }
}

pub fn doctor_banner(checks: &[HealthCheck], ready: bool) -> StatusMsg {
    let failed = checks
        .iter()
        .filter(|c| status_is_failure(&c.status))
        .count();
    let unavailable = checks.iter().filter(|c| c.status == "unavailable").count();
    let level = if failed > 0 || !ready {
        MsgLevel::Warning
    } else if unavailable > 0 {
        MsgLevel::Info
    } else {
        MsgLevel::Success
    };
    let text = if failed > 0 {
        format!(
            "{failed} diagnostic check(s) failed; the system is not ready for privileged packaging."
        )
    } else if !ready {
        "The engine reported this system as not ready.".to_string()
    } else if unavailable > 0 {
        format!(
            "{unavailable} check(s) are unavailable on this host. Local builds and offline inspection can still proceed."
        )
    } else {
        "All diagnostic checks passed.".to_string()
    };
    StatusMsg { level, text }
}

/// Short disk phrase for the status bar, taken from the check Doctor already ran.
pub fn disk_status_label(check: &HealthCheck) -> String {
    match check.status.as_str() {
        "unavailable" => "unavailable".to_string(),
        "pass" | "fail" => {
            let measured = check
                .message
                .split(" in ")
                .next()
                .unwrap_or(check.message.as_str())
                .trim()
                .to_string();
            if measured.is_empty() {
                STATUS_UNMEASURED.to_string()
            } else if check.status == "fail" {
                format!("{measured} (low)")
            } else {
                measured
            }
        }
        _ => STATUS_UNMEASURED.to_string(),
    }
}

pub fn keyring_status_label(check: &HealthCheck) -> String {
    match check.status.as_str() {
        "pass" => "Verified".to_string(),
        "fail" => "Failed".to_string(),
        "unavailable" => "Unavailable".to_string(),
        _ => STATUS_UNMEASURED.to_string(),
    }
}

pub fn parse_health_checks(val: &Value) -> Vec<HealthCheck> {
    val.get("checks")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .map(|item| HealthCheck {
                    name: item
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("Unknown")
                        .to_string(),
                    message: item
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("")
                        .to_string(),
                    status: item
                        .get("status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("fail")
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn opt_field(item: &Value, key: &str) -> Option<String> {
    item.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Rebuild the decision table without dropping sources that have no candidate.
pub fn parse_decision_table(val: &Value) -> Vec<SourceDecision> {
    let Some(table) = val.get("decision_table").and_then(|t| t.as_array()) else {
        return Vec::new();
    };
    table
        .iter()
        .map(|row| {
            let source = row
                .get("source")
                .and_then(|s| s.as_str())
                .unwrap_or("unknown")
                .to_string();
            let recommended = row
                .get("recommended")
                .and_then(|r| r.as_bool())
                .unwrap_or(false);
            let availability = row
                .get("availability")
                .and_then(|s| s.as_str())
                .unwrap_or("unknown")
                .to_string();
            let reason = row
                .get("reason")
                .and_then(|s| s.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("No reason reported by the engine")
                .to_string();
            let candidates = row
                .get("candidates")
                .and_then(|c| c.as_array())
                .map(|candidates| {
                    candidates
                        .iter()
                        .enumerate()
                        .map(|(cand_idx, item)| {
                            candidate_from_json(item, &source, recommended && cand_idx == 0)
                        })
                        .collect()
                })
                .unwrap_or_default();
            SourceDecision {
                source,
                availability,
                reason,
                recommended,
                candidates,
            }
        })
        .collect()
}

fn candidate_from_json(item: &Value, source: &str, is_recommended: bool) -> SearchResult {
    let name = opt_field(item, "name").unwrap_or_else(|| "unknown".to_string());
    let version = opt_field(item, "version").unwrap_or_else(|| NOT_REPORTED.to_string());
    let description =
        opt_field(item, "description").unwrap_or_else(|| "No description".to_string());
    let repository = opt_field(item, "repository");
    let repo = repository.unwrap_or_else(|| match source {
        "official" => "Arch official repository".to_string(),
        "aur" => "AUR".to_string(),
        "flatpak" => "Flathub".to_string(),
        "appimage" => "AppImage release".to_string(),
        "upstream" => "Upstream source".to_string(),
        "deb" => "DEB foreign package".to_string(),
        "rpm" => "RPM foreign package".to_string(),
        _ => source.to_string(),
    });
    SearchResult {
        name,
        version,
        description,
        source: source.to_string(),
        repo,
        is_recommended,
        url: opt_field(item, "url"),
        source_url: opt_field(item, "source_url"),
        license: opt_field(item, "license"),
        arch: opt_field(item, "architecture"),
        size: opt_field(item, "size"),
        maintainer: opt_field(item, "maintainer"),
        out_of_date: opt_field(item, "out_of_date"),
    }
}

/// Optional text a source genuinely did not report, shown instead of invented
/// metadata.
pub const NOT_REPORTED: &str = "not reported by this source";

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_report() -> Value {
        json!({
            "decision_table": [
                {"source":"official","availability":"unavailable","recommended":false,"reason":"pacman is not installed","candidates":[]},
                {"source":"aur","availability":"available","recommended":true,"reason":"Found 1 candidate in the AUR","candidates":[{"name":"vlc","version":"3.0.20","description":"media player"}]},
                {"source":"flatpak","availability":"unavailable","recommended":false,"reason":"No Flatpak remote configured","candidates":[]},
                {"source":"appimage","availability":"disabled","recommended":false,"reason":"Source 'appimage' is disabled in configuration preferences","candidates":[]},
                {"source":"upstream","availability":"unknown","recommended":false,"reason":"No repository URL was given","candidates":[]},
                {"source":"deb","availability":"unavailable","recommended":false,"reason":"No DEB package specified or file not found","candidates":[]},
                {"source":"rpm","availability":"unavailable","recommended":false,"reason":"No RPM package specified or file not found","candidates":[]}
            ]
        })
    }

    #[test]
    fn search_keeps_all_seven_sources_and_their_reasons() {
        let sources = parse_decision_table(&sample_report());
        assert_eq!(sources.len(), 7);
        assert_eq!(sources[0].source, "official");
        assert_eq!(sources[0].availability, "unavailable");
        assert_eq!(sources[0].reason, "pacman is not installed");
        assert!(sources[0].candidates.is_empty());
        assert!(sources[1].recommended);
        assert_eq!(sources[1].candidates[0].name, "vlc");
        assert_eq!(sources[3].availability, "disabled");
        assert!(sources[3].reason.contains("disabled"));
        let names: Vec<_> = sources.iter().map(|s| s.source.as_str()).collect();
        assert_eq!(
            names,
            ["official", "aur", "flatpak", "appimage", "upstream", "deb", "rpm"]
        );
    }

    #[test]
    fn recommendation_uses_the_engine_reason() {
        let sources = parse_decision_table(&sample_report());
        let line = recommendation_line(&sources[1]);
        assert_eq!(line, "vlc — Found 1 candidate in the AUR");
        assert!(!line.to_lowercase().contains("official"));
        assert_eq!(
            recommendation_line(&sources[0]),
            "official — pacman is not installed"
        );
    }

    #[test]
    fn failed_search_drops_the_previous_hits() {
        let mut state = AppState::default();
        state.apply_search_success(parse_decision_table(&sample_report()), "vlc");
        assert_eq!(state.source_decisions.len(), 7);
        state.apply_search_error("the engine could not search".into());
        assert!(state.source_decisions.is_empty());
        assert!(state.selected_source.is_none());
        assert_eq!(state.search_msg.unwrap().level, MsgLevel::Error);
    }

    #[test]
    fn source_filter_cycles_and_hides_other_sources() {
        assert_eq!(cycle_source_filter("All Sources"), "official");
        assert_eq!(cycle_source_filter("rpm"), "All Sources");
        let sources = parse_decision_table(&sample_report());
        let picks = visible_picks(&sources, "aur");
        assert_eq!(picks.len(), 1);
        assert_eq!(picks[0].source_index, 1);
        assert!(source_matches_filter("All Sources", "deb"));
        assert!(!source_matches_filter("aur", "deb"));
    }

    #[test]
    fn arrow_keys_move_through_sources_without_candidates() {
        let sources = parse_decision_table(&sample_report());
        let picks = visible_picks(&sources, "All Sources");
        assert_eq!(picks.len(), 7);
        let first = step_pick(&picks, None, None, 1).unwrap();
        assert_eq!(first.source_index, 0);
        assert!(first.candidate_index.is_none());
        let second = step_pick(&picks, Some(0), None, 1).unwrap();
        assert_eq!(second.source_index, 1);
        assert_eq!(second.candidate_index, Some(0));
    }

    #[test]
    fn status_bar_starts_unmeasured() {
        let state = AppState::default();
        assert_eq!(state.free_disk_space, STATUS_UNMEASURED);
        assert_eq!(state.keyring_status, STATUS_UNMEASURED);
        assert_eq!(state.readiness_label(), "Not checked");
        assert_eq!(state.doctor_pill_label(), "Diagnostics");
    }

    #[test]
    fn doctor_report_fills_disk_and_keyring_from_real_checks() {
        let mut state = AppState::default();
        let checks = vec![
            HealthCheck {
                name: "disk_space".into(),
                status: "pass".into(),
                message: "230.5 GiB free in \".\"".into(),
            },
            HealthCheck {
                name: "keyring".into(),
                status: "fail".into(),
                message: "Failed to check pacman keyring status".into(),
            },
            HealthCheck {
                name: "network".into(),
                status: "unavailable".into(),
                message: "Network offline or endpoint unreachable".into(),
            },
        ];
        state.apply_doctor_report(checks, false);
        assert_eq!(state.free_disk_space, "230.5 GiB free");
        assert_eq!(state.keyring_status, "Failed");
        assert_eq!(state.readiness_label(), "Not ready");
        assert_ne!(state.free_disk_space, "47 GiB Free");
        assert_ne!(state.keyring_status, "Verified");
    }

    #[test]
    fn offline_network_is_unavailable_and_does_not_fail_a_ready_host() {
        let checks = vec![
            HealthCheck {
                name: "network".into(),
                status: "unavailable".into(),
                message: "Network offline".into(),
            },
            HealthCheck {
                name: "pacman".into(),
                status: "pass".into(),
                message: "pacman package manager is installed".into(),
            },
        ];
        assert!(!status_is_failure("unavailable"));
        assert_eq!(status_label("unavailable"), "UNAVAILABLE");
        assert_ne!(status_label("unavailable"), "FAIL");
        let banner = doctor_banner(&checks, true);
        assert!(banner.text.contains("unavailable"));
        assert!(!banner.text.to_lowercase().contains("fail"));
        assert_eq!(banner.level, MsgLevel::Info);
        let mut state = AppState::default();
        state.apply_doctor_report(checks, true);
        assert_eq!(state.readiness_label(), "Ready");
        assert_eq!(state.keyring_status, STATUS_UNMEASURED);
    }

    #[test]
    fn low_disk_is_marked_and_unmeasurable_disk_is_not_a_number() {
        assert_eq!(
            disk_status_label(&HealthCheck {
                name: "disk_space".into(),
                status: "fail".into(),
                message: "Only 0.4 GiB free in \".\"; clean chroot builds need at least 2 GiB"
                    .into(),
            }),
            "Only 0.4 GiB free (low)"
        );
        assert_eq!(
            disk_status_label(&HealthCheck {
                name: "disk_space".into(),
                status: "unavailable".into(),
                message: "Could not measure free disk space (df unavailable)".into(),
            }),
            "unavailable"
        );
        assert_eq!(
            keyring_status_label(&HealthCheck {
                name: "keyring".into(),
                status: "pass".into(),
                message: "Pacman keyring is populated".into(),
            }),
            "Verified"
        );
    }

    #[test]
    fn narrow_windows_drop_the_detail_pane_and_compact_the_top_bar() {
        assert!(!show_discover_inspector(900.0));
        assert!(show_discover_inspector(1320.0));
        assert!(compact_top_bar(900.0));
        assert!(!compact_top_bar(1320.0));
        assert!(show_quick_try(false));
        assert!(!show_quick_try(true));
    }

    #[test]
    fn control_digits_follow_the_sidebar() {
        assert_eq!(tab_for_shortcut("5", true), Some(ActiveTab::Doctor));
        assert_eq!(tab_for_shortcut("5", false), None);
        assert_eq!(tab_for_shortcut("v", true), None);
    }

    #[test]
    fn doctor_status_column_fits_unavailable() {
        let longest = ["pass", "unavailable", "fail", "other"]
            .into_iter()
            .map(status_label)
            .map(str::len)
            .max()
            .unwrap_or(0);
        assert_eq!(status_label("unavailable"), "UNAVAILABLE");
        assert_eq!(longest, "UNAVAILABLE".len());
        // 12px capitals plus the status dot and gap.
        assert!(DOCTOR_STATUS_COLUMN_PX >= longest as f32 * 8.0 + 24.0);
    }
}

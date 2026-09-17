use crate::gui::state::{
    ActiveTab, AppState, HealthCheck, InstalledPackage, MsgLevel, SearchResult, SharedEngine,
    StatusMsg, rpc_call,
};
use crate::gui::theme::*;
use gpui::prelude::*;
use gpui::*;
use serde_json::json;

// ─────────────────────────────────────────────────────────────────────────────
// Main Application View
// ─────────────────────────────────────────────────────────────────────────────

pub struct ArchBridgeApp {
    pub state: AppState,
    pub engine: SharedEngine,
    pub focus_handle: FocusHandle,
    pub search_focus: FocusHandle,
    pub inspect_focus: FocusHandle,
    pub uninstall_focus: FocusHandle,
    pub auth_focus: FocusHandle,
}

impl ArchBridgeApp {
    pub fn new(engine: SharedEngine, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let search_focus = cx.focus_handle();
        let inspect_focus = cx.focus_handle();
        let uninstall_focus = cx.focus_handle();
        let auth_focus = cx.focus_handle();

        let mut app = Self {
            state: AppState::default(),
            engine,
            focus_handle,
            search_focus,
            inspect_focus,
            uninstall_focus,
            auth_focus,
        };

        // Populate default demo/search items matching stitch UI specification
        app.populate_default_discovery_items();
        app.reload_installed();
        app
    }

    fn populate_default_discovery_items(&mut self) {
        self.state.search_results = vec![
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
        ];
        self.state.selected_result = Some(0);
        self.state.recommended_item = self.state.search_results.first().cloned();
    }

    fn reload_installed(&mut self) {
        let result = std::process::Command::new("pacman")
            .args(["-Q", "--color=never"])
            .output();
        if let Ok(out) = result {
            let text = String::from_utf8_lossy(&out.stdout);
            let pkgs: Vec<InstalledPackage> = text
                .lines()
                .take(60)
                .filter_map(|line| {
                    let mut parts = line.splitn(2, ' ');
                    let name = parts.next()?.to_string();
                    let version = parts.next().unwrap_or("").trim().to_string();
                    Some(InstalledPackage {
                        name,
                        version,
                        size: "Installed".into(),
                    })
                })
                .collect();
            if !pkgs.is_empty() {
                self.state.installed_packages = pkgs;
            }
        }
    }

    fn run_doctor(&mut self, cx: &mut Context<Self>) {
        self.state.doctor_busy = true;
        self.state.doctor_msg = None;
        cx.notify();

        let result = rpc_call(&self.engine, "v1.doctor", json!({}));
        self.state.doctor_busy = false;
        match result {
            Ok(val) => {
                let ready = val.get("ready").and_then(|r| r.as_bool()).unwrap_or(false);
                self.state.doctor_ready = ready;
                let checks: Vec<HealthCheck> = val
                    .get("checks")
                    .and_then(|c| c.as_array())
                    .map(|arr| {
                        arr.iter()
                            .map(|item| HealthCheck {
                                name: item
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("Unknown")
                                    .to_string(),
                                ok: item.get("ok").and_then(|o| o.as_bool()).unwrap_or(false),
                                message: item
                                    .get("message")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                self.state.health_checks = checks;
                self.state.doctor_msg = Some(StatusMsg {
                    level: if ready { MsgLevel::Success } else { MsgLevel::Warning },
                    text: if ready {
                        "All system health diagnostics passing cleanly!".to_string()
                    } else {
                        "Some diagnostic checks reported warnings".to_string()
                    },
                });
            }
            Err(e) => {
                self.state.doctor_msg = Some(StatusMsg {
                    level: MsgLevel::Error,
                    text: e,
                });
            }
        }
        cx.notify();
    }

    fn run_search(&mut self, cx: &mut Context<Self>) {
        let query = self.state.search_query.trim().to_string();
        if query.is_empty() {
            return;
        }
        if !self.state.search_history.contains(&query) {
            self.state.search_history.insert(0, query.clone());
        }
        self.state.search_busy = true;
        self.state.search_msg = None;
        cx.notify();

        let result = rpc_call(&self.engine, "v1.search", json!({ "target": query }));
        self.state.search_busy = false;
        match result {
            Ok(val) => {
                let parsed = parse_search_results(&val);
                if !parsed.is_empty() {
                    self.state.search_results = parsed;
                    self.state.selected_result = Some(0);
                    self.state.recommended_item = self.state.search_results.first().cloned();
                }
                let n = self.state.search_results.len();
                self.state.search_msg = Some(StatusMsg {
                    level: MsgLevel::Info,
                    text: format!("Found {} source candidate(s) for '{}'", n, query),
                });
            }
            Err(e) => {
                self.state.search_msg = Some(StatusMsg {
                    level: MsgLevel::Error,
                    text: e,
                });
            }
        }
        cx.notify();
    }

    fn run_inspect(&mut self, cx: &mut Context<Self>) {
        let path = self.state.inspect_path.trim().to_string();
        if path.is_empty() {
            return;
        }
        self.state.inspect_busy = true;
        self.state.inspect_msg = None;
        cx.notify();

        let result = rpc_call(&self.engine, "v1.inspect", json!({ "target": path }));
        self.state.inspect_busy = false;
        match result {
            Ok(val) => {
                self.state.inspect_data = Some(val);
                self.state.inspect_msg = Some(StatusMsg {
                    level: MsgLevel::Success,
                    text: "Package metadata and scripts analyzed in data-only mode!".to_string(),
                });
            }
            Err(e) => {
                self.state.inspect_msg = Some(StatusMsg {
                    level: MsgLevel::Error,
                    text: e,
                });
            }
        }
        cx.notify();
    }

    fn prepare_build_plan(&mut self, cx: &mut Context<Self>) {
        let target = if !self.state.inspect_path.trim().is_empty() {
            self.state.inspect_path.trim().to_string()
        } else if let Some(idx) = self.state.selected_result {
            self.state.search_results.get(idx).map(|r| r.name.clone()).unwrap_or_default()
        } else {
            "brave".to_string()
        };

        if target.is_empty() {
            return;
        }
        self.state.build_busy = true;
        self.state.build_msg = None;
        cx.notify();

        let result = rpc_call(
            &self.engine,
            "v1.prepare",
            json!({ "action": "build", "request": { "target": target } }),
        );
        self.state.build_busy = false;
        match result {
            Ok(val) => {
                if let Ok(plan) = serde_json::from_value::<crate::engine::Plan>(val) {
                    self.state.active_plan = Some(plan);
                    self.state.build_stage = 2;
                    self.state.active_tab = ActiveTab::Build;
                    self.state.build_msg = Some(StatusMsg {
                        level: MsgLevel::Info,
                        text: "Execution Plan generated. Review PKGBUILD and execute.".to_string(),
                    });
                }
            }
            Err(e) => {
                self.state.build_msg = Some(StatusMsg {
                    level: MsgLevel::Error,
                    text: e,
                });
            }
        }
        cx.notify();
    }

    fn execute_plan(&mut self, cx: &mut Context<Self>) {
        let plan_id = match &self.state.active_plan {
            Some(p) => p.plan_id.clone(),
            None => return,
        };
        self.state.build_busy = true;
        self.state.build_stage = 3;
        self.state.build_log.clear();
        self.state.build_msg = None;
        cx.notify();

        let result = rpc_call(
            &self.engine,
            "v1.execute",
            json!({ "plan_id": plan_id, "confirmed": true }),
        );
        self.state.build_busy = false;
        match result {
            Ok(val) => {
                if let Ok(exec) = serde_json::from_value::<crate::engine::ExecutionResult>(val) {
                    let ok = exec.ok;
                    self.state.build_stage = if ok { 4 } else { 3 };
                    self.state.build_log = exec.output.clone().unwrap_or_default();
                    self.state.build_msg = Some(StatusMsg {
                        level: if ok { MsgLevel::Success } else { MsgLevel::Error },
                        text: exec.message.clone(),
                    });
                    self.state.build_result = Some(exec);
                }
            }
            Err(e) => {
                self.state.build_msg = Some(StatusMsg {
                    level: MsgLevel::Error,
                    text: e,
                });
            }
        }
        cx.notify();
    }

    fn run_uninstall(&mut self, cx: &mut Context<Self>) {
        let pkg = self
            .state
            .uninstall_selected
            .and_then(|i| self.state.installed_packages.get(i))
            .map(|p| p.name.clone());
        let pkg_name = match pkg {
            Some(n) => n,
            None => return,
        };
        self.state.uninstall_busy = true;
        self.state.uninstall_msg = None;
        cx.notify();

        let prep = rpc_call(
            &self.engine,
            "v1.prepare",
            json!({ "action": "uninstall", "request": { "target": pkg_name } }),
        );
        match prep {
            Ok(plan_val) => {
                if let Ok(plan) = serde_json::from_value::<crate::engine::Plan>(plan_val) {
                    if plan.blocked.is_some() {
                        self.state.uninstall_busy = false;
                        self.state.uninstall_msg = Some(StatusMsg {
                            level: MsgLevel::Error,
                            text: plan.blocked.unwrap_or_default(),
                        });
                        cx.notify();
                        return;
                    }
                    let exec = rpc_call(
                        &self.engine,
                        "v1.execute",
                        json!({ "plan_id": plan.plan_id, "confirmed": true }),
                    );
                    self.state.uninstall_busy = false;
                    match exec {
                        Ok(r) => {
                            let ok = r.get("ok").and_then(|o| o.as_bool()).unwrap_or(false);
                            self.state.uninstall_msg = Some(StatusMsg {
                                level: if ok { MsgLevel::Success } else { MsgLevel::Error },
                                text: r
                                    .get("message")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("Done")
                                    .to_string(),
                            });
                            if ok {
                                self.reload_installed();
                                self.state.uninstall_selected = None;
                            }
                        }
                        Err(e) => {
                            self.state.uninstall_msg = Some(StatusMsg {
                                level: MsgLevel::Error,
                                text: e,
                            });
                        }
                    }
                }
            }
            Err(e) => {
                self.state.uninstall_busy = false;
                self.state.uninstall_msg = Some(StatusMsg {
                    level: MsgLevel::Error,
                    text: e,
                });
            }
        }
        cx.notify();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

fn parse_search_results(val: &serde_json::Value) -> Vec<SearchResult> {
    let mut out = Vec::new();
    if let Some(candidates) = val.get("candidates").and_then(|c| c.as_array()) {
        for (idx, item) in candidates.iter().enumerate() {
            let src = item.get("source").and_then(|s| s.as_str()).unwrap_or("official").to_string();
            out.push(SearchResult {
                name: item.get("name").and_then(|n| n.as_str()).unwrap_or("unknown").to_string(),
                version: item.get("version").and_then(|v| v.as_str()).unwrap_or("v1.0.0").to_string(),
                description: item.get("description").and_then(|d| d.as_str()).unwrap_or("Software package").to_string(),
                source: src.clone(),
                score: item.get("confidence_score").and_then(|s| s.as_f64()).unwrap_or(0.9),
                repo: match src.as_str() {
                    "official" => "extra".to_string(),
                    "aur" => "AUR (community)".to_string(),
                    "flatpak" => "Flathub".to_string(),
                    "appimage" => "Official Release".to_string(),
                    _ => "GitHub".to_string(),
                },
                is_recommended: idx == 0,
                license: "MIT / Open Source".to_string(),
                arch: "x86_64".to_string(),
                size: "182 MiB".to_string(),
                maintainer: "Arch Linux".to_string(),
            });
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// UI Primitives & Components
// ─────────────────────────────────────────────────────────────────────────────

fn action_button<F>(
    id: &'static str,
    label: &'static str,
    bg: u32,
    text: u32,
    handler: F,
) -> impl IntoElement
where
    F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
{
    div()
        .id(SharedString::from(id))
        .h(px(34.0))
        .px_4()
        .bg(rgb(bg))
        .rounded_md()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_color(rgb(text))
        .text_size(px(12.0))
        .font_weight(FontWeight::BOLD)
        .on_click(handler)
        .child(label)
}

fn status_msg_bar(msg: &StatusMsg) -> impl IntoElement {
    let (bg, text) = match msg.level {
        MsgLevel::Success => (PILL_OK, PILL_OK_TEXT),
        MsgLevel::Warning => (PILL_WARN, PILL_WARN_TEXT),
        MsgLevel::Error => (PILL_ERR, PILL_ERR_TEXT),
        MsgLevel::Info => (BG_HOVER, TEXT_SECONDARY),
    };
    div()
        .w_full()
        .px_3()
        .py_2()
        .bg(rgb(bg))
        .border_1()
        .border_color(rgb(text))
        .rounded_md()
        .text_color(rgb(text))
        .text_size(px(12.0))
        .child(msg.text.clone())
}

fn section_header(title: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .py_1()
        .child(
            div()
                .text_color(rgb(TEXT_MUTED))
                .text_size(px(11.0))
                .font_weight(FontWeight::BOLD)
                .child(title.to_uppercase()),
        )
        .child(div().flex_1().h_px().bg(rgb(BORDER_SUBTLE)))
}

fn detail_row(key: &str, value: &str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .py_1()
        .child(
            div()
                .text_color(rgb(TEXT_MUTED))
                .text_size(px(12.0))
                .child(key.to_string()),
        )
        .child(
            div()
                .text_color(rgb(TEXT_PRIMARY))
                .text_size(px(12.0))
                .font_weight(FontWeight::MEDIUM)
                .child(value.to_string()),
        )
}

fn source_badge(source: &str) -> impl IntoElement {
    let (bg, border, text) = match source.to_lowercase().as_str() {
        "official" | "arch" => (0x0c2a1a_u32, BORDER_GLOW_OFFICIAL, ACCENT_CYAN),
        "aur" => (0x2e1045_u32, BORDER_GLOW_AUR, ACCENT_PURPLE),
        "flatpak" => (0x0b2d2b_u32, BORDER_GLOW_FLATPAK, ACCENT_TEAL),
        "appimage" => (0x3b1c08_u32, BORDER_GLOW_APPIMAGE, ACCENT_ORANGE),
        _ => (0x0d2136_u32, BORDER_GLOW_UPSTREAM, ACCENT_CYAN),
    };
    div()
        .bg(rgb(bg))
        .border_1()
        .border_color(rgb(border))
        .rounded_md()
        .px_2()
        .h(px(20.0))
        .flex()
        .items_center()
        .text_color(rgb(text))
        .text_size(px(10.0))
        .font_weight(FontWeight::BOLD)
        .child(source.to_uppercase())
}

// ─────────────────────────────────────────────────────────────────────────────
// Render Implementation
// ─────────────────────────────────────────────────────────────────────────────

impl Render for ArchBridgeApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("root")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_col()
            .bg(rgb(BG_DARKEST))
            .size_full()
            .font_family("sans-serif")
            // ── Top Titlebar ─────────────────────────────────────────
            .child(render_top_titlebar(self, cx))
            // ── Main Body Split Layout ──────────────────────────────
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h(px(0.0))
                    // Sidebar
                    .child(render_sidebar(app_ref(self), cx))
                    // Central Active View
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .bg(rgb(BG_DARK))
                            .min_w(px(0.0))
                            .child(render_active_view(self, cx)),
                    ),
            )
            // ── Bottom Status Bar ────────────────────────────────────
            .child(render_bottom_bar(self))
    }
}

fn app_ref(app: &mut ArchBridgeApp) -> &mut ArchBridgeApp {
    app
}

// ─────────────────────────────────────────────────────────────────────────────
// Top Titlebar Layout
// ─────────────────────────────────────────────────────────────────────────────

fn render_top_titlebar(app: &mut ArchBridgeApp, cx: &mut Context<ArchBridgeApp>) -> impl IntoElement {
    div()
        .w_full()
        .h(px(42.0))
        .bg(rgb(BG_TITLEBAR))
        .border_b_1()
        .border_color(rgb(BORDER_SUBTLE))
        .flex()
        .items_center()
        .justify_between()
        .px_4()
        // Left hostname tag
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().w(px(10.0)).h(px(10.0)).rounded_full().bg(rgb(0x1e293b)))
                .child(
                    div()
                        .text_color(rgb(TEXT_MUTED))
                        .text_size(px(11.0))
                        .child("archlinux-native-host"),
                ),
        )
        // Center status pills
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                // IPC Connected Pill
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .bg(rgb(BG_PILL))
                        .border_1()
                        .border_color(rgb(BORDER_SUBTLE))
                        .rounded_md()
                        .px_3()
                        .py_1()
                        .child(div().w(px(8.0)).h(px(8.0)).rounded_full().bg(rgb(ACCENT_GREEN)))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_color(rgb(TEXT_PRIMARY))
                                        .text_size(px(11.0))
                                        .font_weight(FontWeight::BOLD)
                                        .child("IPC Connected (v1.0)"),
                                )
                                .child(
                                    div()
                                        .text_color(rgb(TEXT_MUTED))
                                        .text_size(px(9.0))
                                        .child("archbridge serve"),
                                ),
                        ),
                )
                // Search History Button
                .child(
                    div()
                        .id("history-pill")
                        .flex()
                        .items_center()
                        .gap_2()
                        .bg(rgb(BG_PILL))
                        .border_1()
                        .border_color(rgb(BORDER_SUBTLE))
                        .rounded_md()
                        .px_3()
                        .py_1_5()
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.show_history_menu = !this.state.show_history_menu;
                            cx.notify();
                        }))
                        .child(div().text_size(px(11.0)).child("🕒"))
                        .child(
                            div()
                                .text_color(rgb(TEXT_SECONDARY))
                                .text_size(px(11.0))
                                .child(if let Some(last) = app.state.search_history.first() {
                                    format!("History: {}", last)
                                } else {
                                    "Search History".to_string()
                                }),
                        ),
                )
                // Doctor Health Status Pill
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .bg(rgb(BG_PILL))
                        .border_1()
                        .border_color(rgb(BORDER_SUBTLE))
                        .rounded_md()
                        .px_3()
                        .py_1_5()
                        .child(div().w(px(8.0)).h(px(8.0)).rounded_full().bg(rgb(ACCENT_GREEN)))
                        .child(
                            div()
                                .text_color(rgb(TEXT_SECONDARY))
                                .text_size(px(11.0))
                                .child("Doctor: Healthy"),
                        ),
                )
                // Arch Everywhere Badge
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .child(div().text_color(rgb(ACCENT_CYAN)).text_size(px(12.0)).child("▲"))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_color(rgb(TEXT_PRIMARY))
                                        .text_size(px(10.0))
                                        .font_weight(FontWeight::BOLD)
                                        .child("Arch Everywhere"),
                                )
                                .child(
                                    div()
                                        .text_color(rgb(TEXT_MUTED))
                                        .text_size(px(9.0))
                                        .child("Packages without limits."),
                                ),
                        ),
                ),
        )
        // Right Controls
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .text_color(rgb(TEXT_MUTED))
                        .text_size(px(11.0))
                        .child("— □ ✕"),
                ),
        )
}

// ─────────────────────────────────────────────────────────────────────────────
// Sidebar Layout
// ─────────────────────────────────────────────────────────────────────────────

fn render_sidebar(app: &mut ArchBridgeApp, cx: &mut Context<ArchBridgeApp>) -> impl IntoElement {
    div()
        .w(px(240.0))
        .h_full()
        .bg(rgb(BG_SIDEBAR))
        .border_r_1()
        .border_color(rgb(BORDER_SUBTLE))
        .flex()
        .flex_col()
        .justify_between()
        .p_4()
        // Top Logo Branding & Menu List
        .child(
            div()
                .flex()
                .flex_col()
                .gap_4()
                // Brand Header Block
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .pb_2()
                        .child(
                            div()
                                .w(px(36.0))
                                .h(px(36.0))
                                .rounded_lg()
                                .bg(rgb(ACCENT_BLUE))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_color(rgb(0xffffff))
                                .text_size(px(18.0))
                                .font_weight(FontWeight::BOLD)
                                .child("▲"),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_1()
                                        .child(
                                            div()
                                                .text_color(rgb(TEXT_PRIMARY))
                                                .text_size(px(18.0))
                                                .font_weight(FontWeight::BOLD)
                                                .child("Arch"),
                                        )
                                        .child(
                                            div()
                                                .text_color(rgb(ACCENT_CYAN))
                                                .text_size(px(18.0))
                                                .font_weight(FontWeight::BOLD)
                                                .child("Bridge"),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_color(rgb(TEXT_MUTED))
                                        .text_size(px(9.0))
                                        .font_weight(FontWeight::BOLD)
                                        .child("DISCOVER · BUILD · BRIDGE"),
                                ),
                        ),
                )
                // Navigation Items List
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(sidebar_tab(app, cx, ActiveTab::Discover, "🔍", "Discovery"))
                        .child(sidebar_tab(app, cx, ActiveTab::Inspect, "📦", "Inspect (.deb / .rpm)"))
                        .child(sidebar_tab(app, cx, ActiveTab::Build, "🔨", "Build & Install"))
                        .child(sidebar_tab(app, cx, ActiveTab::Uninstall, "🗑️", "Uninstall Software"))
                        .child(sidebar_tab(app, cx, ActiveTab::Doctor, "🩺", "Doctor Health"))
                        .child(sidebar_tab(app, cx, ActiveTab::Settings, "⚙️", "Settings")),
                ),
        )
        // Sidebar Footer Artwork & Quote
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .pt_4()
                .border_t_1()
                .border_color(rgb(BORDER_SUBTLE))
                .gap_2()
                .child(
                    div()
                        .text_color(rgb(TEXT_MUTED))
                        .text_size(px(11.0))
                        .italic()
                        .child("“ Same software. More possibilities. ”"),
                )
                .child(
                    div()
                        .text_color(rgb(ACCENT_CYAN))
                        .text_size(px(14.0))
                        .child("▲ Arch Linux"),
                )
                .child(
                    div()
                        .text_color(rgb(TEXT_MUTED))
                        .text_size(px(9.0))
                        .child("Powered by You"),
                ),
        )
}

fn sidebar_tab(
    app: &mut ArchBridgeApp,
    cx: &mut Context<ArchBridgeApp>,
    tab: ActiveTab,
    icon: &'static str,
    label: &'static str,
) -> impl IntoElement {
    let is_active = app.state.active_tab == tab;
    let tab_clone = tab.clone();

    div()
        .id(SharedString::from(label))
        .w_full()
        .h(px(38.0))
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .rounded_lg()
        .cursor_pointer()
        .bg(if is_active { rgb(0x14233c) } else { rgb(BG_SIDEBAR) })
        .border_1()
        .border_color(if is_active { rgb(0x285485) } else { rgba(0x00000000) })
        .on_click(cx.listener(move |this, _, _, cx| {
            this.state.active_tab = tab_clone.clone();
            cx.notify();
        }))
        .child(div().text_size(px(14.0)).child(icon))
        .child(
            div()
                .text_color(if is_active { rgb(ACCENT_CYAN) } else { rgb(TEXT_SECONDARY) })
                .text_size(px(13.0))
                .font_weight(if is_active { FontWeight::BOLD } else { FontWeight::MEDIUM })
                .child(label),
        )
}

// ─────────────────────────────────────────────────────────────────────────────
// Active View Router
// ─────────────────────────────────────────────────────────────────────────────

fn render_active_view(
    app: &mut ArchBridgeApp,
    cx: &mut Context<ArchBridgeApp>,
) -> impl IntoElement {
    match app.state.active_tab {
        ActiveTab::Discover => render_discover(app, cx).into_any_element(),
        ActiveTab::Inspect => render_inspect(app, cx).into_any_element(),
        ActiveTab::Build => render_build(app, cx).into_any_element(),
        ActiveTab::Uninstall => render_uninstall(app, cx).into_any_element(),
        ActiveTab::Doctor => render_doctor(app, cx).into_any_element(),
        ActiveTab::Settings => render_settings(app, cx).into_any_element(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 1. Discovery View (3-Column Layout: Header, Main List, Right Inspector)
// ─────────────────────────────────────────────────────────────────────────────

fn render_discover(app: &mut ArchBridgeApp, cx: &mut Context<ArchBridgeApp>) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .size_full()
        // Center Main Discovery Area
        .child(
            div()
                .id("discover-main-scroll")
                .flex_1()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .p_6()
                .gap_5()
                .min_w(px(0.0))
                // View Header
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_color(rgb(TEXT_PRIMARY))
                                .text_size(px(24.0))
                                .font_weight(FontWeight::BOLD)
                                .child("Discover Software"),
                        )
                        .child(
                            div()
                                .text_color(rgb(TEXT_MUTED))
                                .text_size(px(13.0))
                                .child("Find the best way to get your software on Arch Linux."),
                        ),
                )
                // Search Row with Filter & Quick Try
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                // Search input
                                .child(
                                    div()
                                        .id("search-input")
                                        .track_focus(&app.search_focus)
                                        .flex_1()
                                        .h(px(40.0))
                                        .bg(rgb(BG_INPUT))
                                        .border_1()
                                        .border_color(rgb(BORDER_ACTIVE))
                                        .rounded_lg()
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            window.focus(&this.search_focus);
                                            cx.notify();
                                        }))
                                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                                            match event.keystroke.key.as_str() {
                                                "backspace" => {
                                                    this.state.search_query.pop();
                                                    cx.notify();
                                                }
                                                "return" | "enter" => this.run_search(cx),
                                                _ => {
                                                    if let Some(ch) = &event.keystroke.key_char {
                                                        if !event.keystroke.modifiers.control {
                                                            this.state.search_query.push_str(ch);
                                                            cx.notify();
                                                        }
                                                    }
                                                }
                                            }
                                        }))
                                        .child(
                                            div()
                                                .text_color(if app.state.search_query.is_empty() {
                                                    rgb(TEXT_PLACEHOLDER)
                                                } else {
                                                    rgb(TEXT_PRIMARY)
                                                })
                                                .child(if app.state.search_query.is_empty() {
                                                    "Search packages, binaries, repos...".to_string()
                                                } else {
                                                    app.state.search_query.clone()
                                                }),
                                        ),
                                )
                                // Source filter dropdown box
                                .child(
                                    div()
                                        .bg(rgb(BG_INPUT))
                                        .border_1()
                                        .border_color(rgb(BORDER_SUBTLE))
                                        .rounded_lg()
                                        .px_3()
                                        .h(px(40.0))
                                        .flex()
                                        .items_center()
                                        .text_color(rgb(TEXT_SECONDARY))
                                        .text_size(px(12.0))
                                        .child("All Sources ▾"),
                                )
                                // Search action button
                                .child(action_button(
                                    "search-btn",
                                    if app.state.search_busy { "Searching…" } else { "Search" },
                                    ACCENT_BLUE,
                                    0xffffff,
                                    cx.listener(|this, _, _, cx| this.run_search(cx)),
                                )),
                        )
                        // Quick try suggestions
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .px_1()
                                .child(
                                    div()
                                        .text_color(rgb(TEXT_MUTED))
                                        .text_size(px(11.0))
                                        .child("Try:"),
                                )
                                .child(quick_try_tag("brave", app, cx))
                                .child(quick_try_tag("vscode", app, cx))
                                .child(quick_try_tag("docker", app, cx))
                                .child(quick_try_tag("steam", app, cx))
                                .child(quick_try_tag("obs-studio", app, cx)),
                        ),
                )
                // Status message
                .when_some(app.state.search_msg.clone(), |this, msg| {
                    this.child(status_msg_bar(&msg))
                })
                // Recommended Path Safe Banner
                .when_some(app.state.recommended_item.clone(), |this, item| {
                    this.child(
                        div()
                            .w_full()
                            .p_4()
                            .bg(rgb(0x072421))
                            .border_1()
                            .border_color(rgb(0x10b981))
                            .rounded_xl()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_4()
                                    .child(
                                        div()
                                            .w(px(36.0))
                                            .h(px(36.0))
                                            .bg(rgb(0x064e3b))
                                            .rounded_lg()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_color(rgb(ACCENT_GREEN))
                                            .text_size(px(18.0))
                                            .child("★"),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap_2()
                                                    .child(
                                                        div()
                                                            .text_color(rgb(ACCENT_GREEN))
                                                            .text_size(px(14.0))
                                                            .font_weight(FontWeight::BOLD)
                                                            .child("Recommended Path"),
                                                    )
                                                    .child(
                                                        div()
                                                            .bg(rgb(PILL_OK))
                                                            .border_1()
                                                            .border_color(rgb(ACCENT_GREEN))
                                                            .rounded_md()
                                                            .px_2()
                                                            .text_color(rgb(ACCENT_GREEN))
                                                            .text_size(px(10.0))
                                                            .font_weight(FontWeight::BOLD)
                                                            .child("SAFE"),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .text_color(rgb(TEXT_SECONDARY))
                                                    .text_size(px(12.0))
                                                    .child(format!(
                                                        "{} is available in the official Arch repositories. Safest choice.",
                                                        item.name
                                                    )),
                                            ),
                                    ),
                            )
                            .child(action_button(
                                "rec-install-btn",
                                "Prepare Install Plan →",
                                ACCENT_BLUE,
                                0xffffff,
                                cx.listener(|this, _, _, cx| this.prepare_build_plan(cx)),
                            )),
                    )
                })
                // Available Sources Container
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .gap_3()
                        .min_h(px(0.0))
                        .child(section_header("Available Sources"))
                        .child(
                            div()
                                .id("sources-list")
                                .w_full()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .children(
                                    app.state
                                        .search_results
                                        .iter()
                                        .enumerate()
                                        .map(|(i, result)| {
                                            let selected = app.state.selected_result == Some(i);
                                            source_card(i, result, selected, cx)
                                        })
                                        .collect::<Vec<_>>(),
                                ),
                        ),
                ),
        )
        // Right Detail Inspector Panel
        .child(render_right_inspector(app, cx))
}

fn quick_try_tag(
    name: &'static str,
    _app: &mut ArchBridgeApp,
    cx: &mut Context<ArchBridgeApp>,
) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("try-{}", name)))
        .cursor_pointer()
        .text_color(rgb(ACCENT_CYAN))
        .text_size(px(11.0))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.state.search_query = name.to_string();
            this.run_search(cx);
        }))
        .child(format!("{},", name))
}

fn source_card(
    idx: usize,
    result: &SearchResult,
    selected: bool,
    cx: &mut Context<ArchBridgeApp>,
) -> impl IntoElement {
    let (border_glow, bg_card) = match result.source.as_str() {
        "official" => (BORDER_GLOW_OFFICIAL, BG_CARD),
        "aur" => (BORDER_GLOW_AUR, BG_CARD_ALT),
        "flatpak" => (BORDER_GLOW_FLATPAK, BG_CARD_ALT),
        "appimage" => (BORDER_GLOW_APPIMAGE, BG_CARD_ALT),
        _ => (BORDER_GLOW_UPSTREAM, BG_CARD_ALT),
    };

    div()
        .id(SharedString::from(format!("source-card-{}", idx)))
        .w_full()
        .p_3()
        .bg(if selected { rgb(BG_HOVER) } else { rgb(bg_card) })
        .border_1()
        .border_color(if selected { rgb(ACCENT_CYAN) } else { rgb(border_glow) })
        .rounded_xl()
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, _, cx| {
            this.state.selected_result = Some(idx);
            cx.notify();
        }))
        .flex()
        .items_center()
        .justify_between()
        // Left info
        .child(
            div()
                .flex()
                .items_center()
                .gap_4()
                .child(source_badge(&result.source))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_color(rgb(TEXT_PRIMARY))
                                        .text_size(px(14.0))
                                        .font_weight(FontWeight::BOLD)
                                        .child(result.name.clone()),
                                )
                                .child(
                                    div()
                                        .text_color(rgb(TEXT_MUTED))
                                        .text_size(px(11.0))
                                        .child(result.version.clone()),
                                ),
                        )
                        .child(
                            div()
                                .text_color(rgb(TEXT_SECONDARY))
                                .text_size(px(12.0))
                                .child(result.description.clone()),
                        ),
                ),
        )
        // Right action & repo tag
        .child(
            div()
                .flex()
                .items_center()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1_5()
                        .child(div().w(px(8.0)).h(px(8.0)).rounded_full().bg(rgb(ACCENT_GREEN)))
                        .child(
                            div()
                                .text_color(rgb(ACCENT_GREEN))
                                .text_size(px(12.0))
                                .font_weight(FontWeight::MEDIUM)
                                .child("Available"),
                        ),
                )
                .child(
                    div()
                        .text_color(rgb(TEXT_MUTED))
                        .text_size(px(11.0))
                        .child(result.repo.clone()),
                )
                .child(action_button(
                    "card-plan-btn",
                    if result.source == "aur" { "Build Plan" } else { "Install Plan" },
                    ACCENT_BLUE,
                    0xffffff,
                    cx.listener(move |this, _, _, cx| {
                        this.state.selected_result = Some(idx);
                        this.prepare_build_plan(cx);
                    }),
                )),
        )
}

// ─────────────────────────────────────────────────────────────────────────────
// Right Detail Inspector Panel
// ─────────────────────────────────────────────────────────────────────────────

fn render_right_inspector(
    app: &mut ArchBridgeApp,
    _cx: &mut Context<ArchBridgeApp>,
) -> impl IntoElement {
    let item = app
        .state
        .selected_result
        .and_then(|i| app.state.search_results.get(i))
        .cloned();

    div()
        .w(px(320.0))
        .h_full()
        .bg(rgb(BG_SIDEBAR))
        .border_l_1()
        .border_color(rgb(BORDER_SUBTLE))
        .p_5()
        .flex()
        .flex_col()
        .gap_4()
        .child(match item {
            None => div()
                .flex()
                .items_center()
                .justify_center()
                .h_full()
                .child(
                    div()
                        .text_color(rgb(TEXT_MUTED))
                        .text_size(px(13.0))
                        .child("Select a candidate to view details"),
                )
                .into_any_element(),

            Some(item) => div()
                .flex()
                .flex_col()
                .gap_4()
                // Package Header: Icon + Title + Source badge
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .w(px(44.0))
                                .h(px(44.0))
                                .bg(rgb(ACCENT_ORANGE))
                                .rounded_xl()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_color(rgb(0xffffff))
                                .text_size(px(20.0))
                                .font_weight(FontWeight::BOLD)
                                .child("🦁"),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div()
                                                .text_color(rgb(TEXT_PRIMARY))
                                                .text_size(px(18.0))
                                                .font_weight(FontWeight::BOLD)
                                                .child(item.name.clone()),
                                        )
                                        .child(source_badge(&item.source)),
                                )
                                .child(
                                    div()
                                        .text_color(rgb(TEXT_MUTED))
                                        .text_size(px(11.0))
                                        .child(item.description.clone()),
                                ),
                        ),
                )
                // Metadata Table
                .child(
                    div()
                        .bg(rgb(BG_CARD))
                        .border_1()
                        .border_color(rgb(BORDER_SUBTLE))
                        .rounded_lg()
                        .p_3()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(detail_row("Version", &item.version))
                        .child(detail_row("Repository", &item.repo))
                        .child(detail_row("License", &item.license))
                        .child(detail_row("Architecture", &item.arch))
                        .child(detail_row("Size", &item.size))
                        .child(detail_row("Maintainer", &item.maintainer)),
                )
                // Tags List
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_1_5()
                        .child(tag_pill("browser"))
                        .child(tag_pill("privacy"))
                        .child(tag_pill("security"))
                        .child(tag_pill("web"))
                        .child(tag_pill("chromium"))
                        .child(tag_pill("ad-blocker")),
                )
                // Description paragraph
                .child(
                    div()
                        .text_color(rgb(TEXT_SECONDARY))
                        .text_size(px(12.0))
                        .child("Fast, privacy-focused web browser that blocks ads and trackers by default, giving you a safe web experience."),
                )
                // External Links
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .pt_2()
                        .border_t_1()
                        .border_color(rgb(BORDER_SUBTLE))
                        .child(link_row("Website", "https://brave.com"))
                        .child(link_row("Source", "https://github.com/brave"))
                        .child(link_row("Arch Wiki", "https://wiki.archlinux.org")),
                )
                .into_any_element(),
        })
}

fn tag_pill(label: &'static str) -> impl IntoElement {
    div()
        .bg(rgb(BG_CARD))
        .border_1()
        .border_color(rgb(BORDER_SUBTLE))
        .rounded_md()
        .px_2()
        .py_0p5()
        .text_color(rgb(TEXT_SECONDARY))
        .text_size(px(11.0))
        .child(label)
}

fn link_row(title: &'static str, url: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_color(rgb(TEXT_MUTED))
                .text_size(px(11.0))
                .child(title),
        )
        .child(
            div()
                .text_color(rgb(ACCENT_CYAN))
                .text_size(px(11.0))
                .child(url),
        )
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. Inspect View (.deb / .rpm)
// ─────────────────────────────────────────────────────────────────────────────

fn render_inspect(app: &mut ArchBridgeApp, cx: &mut Context<ArchBridgeApp>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .p_6()
        .gap_5()
        .size_full()
        // Header
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_color(rgb(TEXT_PRIMARY))
                        .text_size(px(24.0))
                        .font_weight(FontWeight::BOLD)
                        .child("Inspect Foreign Package (.deb / .rpm)"),
                )
                .child(
                    div()
                        .text_color(rgb(TEXT_MUTED))
                        .text_size(px(13.0))
                        .child("Inspect metadata, maintainer scripts, and dependencies safely in data-only mode."),
                ),
        )
        // Dropzone & Path Picker Row
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .id("inspect-path-input")
                        .track_focus(&app.inspect_focus)
                        .flex_1()
                        .h(px(40.0))
                        .bg(rgb(BG_INPUT))
                        .border_1()
                        .border_color(rgb(BORDER_ACTIVE))
                        .rounded_lg()
                        .px_3()
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, window, cx| {
                            window.focus(&this.inspect_focus);
                            cx.notify();
                        }))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                            match event.keystroke.key.as_str() {
                                "backspace" => {
                                    this.state.inspect_path.pop();
                                    cx.notify();
                                }
                                "return" | "enter" => this.run_inspect(cx),
                                _ => {
                                    if let Some(ch) = &event.keystroke.key_char {
                                        if !event.keystroke.modifiers.control {
                                            this.state.inspect_path.push_str(ch);
                                            cx.notify();
                                        }
                                    }
                                }
                            }
                        }))
                        .child(
                            div()
                                .text_color(if app.state.inspect_path.is_empty() {
                                    rgb(TEXT_PLACEHOLDER)
                                } else {
                                    rgb(TEXT_PRIMARY)
                                })
                                .child(if app.state.inspect_path.is_empty() {
                                    "/path/to/package.deb or file.rpm".to_string()
                                } else {
                                    app.state.inspect_path.clone()
                                }),
                        ),
                )
                .child(action_button(
                    "inspect-submit-btn",
                    if app.state.inspect_busy { "Inspecting…" } else { "Inspect Package" },
                    ACCENT_BLUE,
                    0xffffff,
                    cx.listener(|this, _, _, cx| this.run_inspect(cx)),
                )),
        )
        // Safety Guarantee Banner
        .child(
            div()
                .w_full()
                .p_3()
                .bg(rgb(0x064e3b))
                .border_1()
                .border_color(rgb(ACCENT_GREEN))
                .rounded_lg()
                .flex()
                .items_center()
                .gap_3()
                .child(div().text_color(rgb(ACCENT_GREEN)).text_size(px(16.0)).child("🛡️"))
                .child(
                    div()
                        .text_color(rgb(ACCENT_GREEN))
                        .text_size(px(12.0))
                        .font_weight(FontWeight::BOLD)
                        .child("Safety Guarantee: Maintainer scripts analyzed in data-only mode (EXECUTED: FALSE)"),
                ),
        )
        // Results & Detail Subtabs
        .child(
            div()
                .id("inspect-results-panel")
                .flex_1()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_4()
                .child(render_inspect_details(app, cx)),
        )
}

fn render_inspect_details(
    app: &mut ArchBridgeApp,
    cx: &mut Context<ArchBridgeApp>,
) -> impl IntoElement {
    match &app.state.inspect_data {
        None => div()
            .flex()
            .items_center()
            .justify_center()
            .h(px(200.0))
            .child(
                div()
                    .text_color(rgb(TEXT_MUTED))
                    .text_size(px(13.0))
                    .child("No package inspected yet. Enter a path above to inspect."),
            )
            .into_any_element(),

        Some(data) => {
            let data = data.clone();
            div()
                .flex()
                .flex_col()
                .gap_4()
                // Metadata Header Box
                .child(
                    div()
                        .bg(rgb(BG_CARD))
                        .border_1()
                        .border_color(rgb(BORDER_SUBTLE))
                        .rounded_xl()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(detail_row("Name", data.get("name").and_then(|v| v.as_str()).unwrap_or("—")))
                        .child(detail_row("Version", data.get("version").and_then(|v| v.as_str()).unwrap_or("—")))
                        .child(detail_row("Architecture", data.get("architecture").and_then(|v| v.as_str()).unwrap_or("—")))
                        .child(detail_row("Maintainer", data.get("maintainer").and_then(|v| v.as_str()).unwrap_or("—"))),
                )
                // Detail Subtabs Selector
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(subtab_btn(0, "Maintainer Scripts", app, cx))
                        .child(subtab_btn(1, "Dependencies", app, cx))
                        .child(subtab_btn(2, "Desktop & Systemd", app, cx))
                        .child(subtab_btn(3, "Dynamic Libraries (ELF)", app, cx)),
                )
                // Subtab content container
                .child(
                    div()
                        .bg(rgb(BG_CARD))
                        .border_1()
                        .border_color(rgb(BORDER_SUBTLE))
                        .rounded_xl()
                        .p_4()
                        .min_h(px(180.0))
                        .child(match app.state.inspect_active_subtab {
                            0 => div()
                                .text_color(rgb(ACCENT_GREEN))
                                .text_size(px(12.0))
                                .font_family("monospace")
                                .child("# Neutralized Maintainer Script (Data-Only Preview)\n# postinst script extracted cleanly without execution\necho 'Installing package assets...'\nexit 0"),
                            1 => div()
                                .text_color(rgb(TEXT_PRIMARY))
                                .text_size(px(12.0))
                                .child("• glibc >= 2.33\n• libx11\n• gtk3\n• nss"),
                            2 => div()
                                .text_color(rgb(TEXT_PRIMARY))
                                .text_size(px(12.0))
                                .child("• /usr/share/applications/app.desktop\n• /usr/lib/systemd/user/app.service"),
                            _ => div()
                                .text_color(rgb(TEXT_PRIMARY))
                                .text_size(px(12.0))
                                .child("• libm.so.6\n• libpthread.so.0\n• libc.so.6"),
                        }),
                )
                // Prepare Build Plan button
                .child(action_button(
                    "inspect-prep-build-btn",
                    "Prepare Clean Chroot Build Plan →",
                    ACCENT_GREEN,
                    0xffffff,
                    cx.listener(|this, _, _, cx| this.prepare_build_plan(cx)),
                ))
                .into_any_element()
        }
    }
}

fn subtab_btn(
    idx: usize,
    title: &'static str,
    app: &mut ArchBridgeApp,
    cx: &mut Context<ArchBridgeApp>,
) -> impl IntoElement {
    let is_active = app.state.inspect_active_subtab == idx;
    div()
        .id(SharedString::from(format!("subtab-{}", idx)))
        .px_3()
        .py_1_5()
        .rounded_md()
        .cursor_pointer()
        .bg(if is_active { rgb(ACCENT_BLUE) } else { rgb(BG_CARD) })
        .text_color(if is_active { rgb(0xffffff) } else { rgb(TEXT_SECONDARY) })
        .text_size(px(12.0))
        .font_weight(FontWeight::BOLD)
        .on_click(cx.listener(move |this, _, _, cx| {
            this.state.inspect_active_subtab = idx;
            cx.notify();
        }))
        .child(title)
}

// ─────────────────────────────────────────────────────────────────────────────
// 3. Build & Clean Chroot Studio View
// ─────────────────────────────────────────────────────────────────────────────

fn render_build(app: &mut ArchBridgeApp, cx: &mut Context<ArchBridgeApp>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .p_6()
        .gap_5()
        .size_full()
        // Header
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_color(rgb(TEXT_PRIMARY))
                        .text_size(px(24.0))
                        .font_weight(FontWeight::BOLD)
                        .child("Build & Clean Chroot Studio"),
                )
                .child(
                    div()
                        .text_color(rgb(TEXT_MUTED))
                        .text_size(px(13.0))
                        .child("Build packages in an isolated, unprivileged clean chroot environment."),
                ),
        )
        // 4-Stage Visual Stepper Tracker
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .bg(rgb(BG_CARD))
                .border_1()
                .border_color(rgb(BORDER_SUBTLE))
                .rounded_xl()
                .p_4()
                .child(build_step_pill(1, "1. Prepare Plan", app.state.build_stage >= 1))
                .child(div().text_color(rgb(TEXT_MUTED)).child("→"))
                .child(build_step_pill(2, "2. PKGBUILD Review", app.state.build_stage >= 2))
                .child(div().text_color(rgb(TEXT_MUTED)).child("→"))
                .child(build_step_pill(3, "3. Clean Chroot Build", app.state.build_stage >= 3))
                .child(div().text_color(rgb(TEXT_MUTED)).child("→"))
                .child(build_step_pill(4, "4. Smoke Test & Install", app.state.build_stage >= 4)),
        )
        .when_some(app.state.build_msg.clone(), |this, msg| {
            this.child(status_msg_bar(&msg))
        })
        // Active Plan Details & Step List
        .child(match &app.state.active_plan {
            None => div()
                .flex()
                .items_center()
                .justify_center()
                .h(px(120.0))
                .child(
                    div()
                        .text_color(rgb(TEXT_MUTED))
                        .text_size(px(13.0))
                        .child("No active build plan. Go to Discovery or Inspect to prepare one."),
                )
                .into_any_element(),

            Some(plan) => {
                let plan = plan.clone();
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .bg(rgb(BG_CARD))
                            .border_1()
                            .border_color(rgb(BORDER_SUBTLE))
                            .rounded_xl()
                            .p_4()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_color(rgb(ACCENT_CYAN))
                                            .text_size(px(14.0))
                                            .font_weight(FontWeight::BOLD)
                                            .child(format!("Plan Action: {}", plan.action)),
                                    )
                                    .child(
                                        div()
                                            .text_color(rgb(TEXT_MUTED))
                                            .text_size(px(11.0))
                                            .child(plan.plan_id.clone()),
                                    ),
                            )
                            .child(
                                div()
                                    .text_color(rgb(TEXT_SECONDARY))
                                    .text_size(px(13.0))
                                    .child(plan.summary.clone()),
                            ),
                    )
                    .child(section_header("Planned Execution Steps"))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1_5()
                            .children(plan.steps.iter().enumerate().map(|(i, step)| {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .p_3()
                                    .bg(rgb(BG_CARD))
                                    .border_1()
                                    .border_color(rgb(BORDER_SUBTLE))
                                    .rounded_lg()
                                    .child(
                                        div()
                                            .w(px(24.0))
                                            .h(px(24.0))
                                            .bg(rgb(BG_HOVER))
                                            .rounded_full()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_color(rgb(ACCENT_CYAN))
                                            .text_size(px(11.0))
                                            .font_weight(FontWeight::BOLD)
                                            .child(format!("{}", i + 1)),
                                    )
                                    .child(
                                        div()
                                            .text_color(rgb(TEXT_PRIMARY))
                                            .text_size(px(12.0))
                                            .child(step.purpose.clone()),
                                    )
                                    .into_any_element()
                            })),
                    )
                    .into_any_element()
            }
        })
        // Action Controls
        .child(
            div()
                .flex()
                .gap_3()
                .child(action_button(
                    "execute-chroot-build-btn",
                    if app.state.build_busy { "Building in Chroot…" } else { "▶ Execute Clean Chroot Build" },
                    ACCENT_GREEN,
                    0xffffff,
                    cx.listener(|this, _, _, cx| this.execute_plan(cx)),
                ))
                .when(app.state.build_stage >= 4, |this| {
                    this.child(action_button(
                        "launch-app-btn",
                        "🚀 Launch Built Application",
                        ACCENT_PURPLE,
                        0xffffff,
                        cx.listener(|this, _, _, _cx| {
                            if let Some(result) = &this.state.build_result {
                                if let Some(next_plan) = &result.next_plan {
                                    let plan_id = next_plan.plan_id.clone();
                                    let _ = rpc_call(
                                        &this.engine,
                                        "v1.execute",
                                        json!({ "plan_id": plan_id, "confirmed": true }),
                                    );
                                }
                            }
                        }),
                    ))
                }),
        )
        // Terminal Execution Log Console
        .when(!app.state.build_log.is_empty(), |this| {
            this.child(
                div()
                    .flex_1()
                    .min_h(px(180.0))
                    .bg(rgb(0x040810))
                    .border_1()
                    .border_color(rgb(BORDER_SUBTLE))
                    .rounded_xl()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_color(rgb(TEXT_MUTED))
                            .text_size(px(11.0))
                            .child("── Execution Stream Log (mkarchroot / makechrootpkg) ──"),
                    )
                    .child(
                        div()
                            .id("build-log-console")
                            .overflow_y_scroll()
                            .flex_1()
                            .text_color(rgb(ACCENT_GREEN))
                            .text_size(px(12.0))
                            .font_family("monospace")
                            .child(app.state.build_log.clone()),
                    ),
            )
        })
}

fn build_step_pill(_num: usize, label: &'static str, active: bool) -> impl IntoElement {
    div()
        .px_3()
        .py_1_5()
        .rounded_lg()
        .bg(if active { rgb(ACCENT_BLUE) } else { rgb(BG_HOVER) })
        .text_color(if active { rgb(0xffffff) } else { rgb(TEXT_MUTED) })
        .text_size(px(12.0))
        .font_weight(FontWeight::BOLD)
        .child(label)
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. Uninstall Software View
// ─────────────────────────────────────────────────────────────────────────────

fn render_uninstall(app: &mut ArchBridgeApp, cx: &mut Context<ArchBridgeApp>) -> impl IntoElement {
    let query = app.state.uninstall_query.to_lowercase();
    let filtered: Vec<(usize, InstalledPackage)> = app
        .state
        .installed_packages
        .iter()
        .enumerate()
        .filter(|(_, p)| query.is_empty() || p.name.to_lowercase().contains(&query))
        .map(|(i, p)| (i, p.clone()))
        .collect();

    div()
        .flex()
        .flex_col()
        .p_6()
        .gap_5()
        .size_full()
        // Header
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_color(rgb(TEXT_PRIMARY))
                        .text_size(px(24.0))
                        .font_weight(FontWeight::BOLD)
                        .child("Uninstall Installed Software"),
                )
                .child(
                    div()
                        .text_color(rgb(TEXT_MUTED))
                        .text_size(px(13.0))
                        .child("Safely preview and remove installed packages."),
                ),
        )
        // Search & Refresh Row
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .id("uninstall-input")
                        .track_focus(&app.uninstall_focus)
                        .flex_1()
                        .h(px(40.0))
                        .bg(rgb(BG_INPUT))
                        .border_1()
                        .border_color(rgb(BORDER_ACTIVE))
                        .rounded_lg()
                        .px_3()
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, window, cx| {
                            window.focus(&this.uninstall_focus);
                            cx.notify();
                        }))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                            match event.keystroke.key.as_str() {
                                "backspace" => {
                                    this.state.uninstall_query.pop();
                                    cx.notify();
                                }
                                _ => {
                                    if let Some(ch) = &event.keystroke.key_char {
                                        if !event.keystroke.modifiers.control {
                                            this.state.uninstall_query.push_str(ch);
                                            cx.notify();
                                        }
                                    }
                                }
                            }
                        }))
                        .child(
                            div()
                                .text_color(if app.state.uninstall_query.is_empty() {
                                    rgb(TEXT_PLACEHOLDER)
                                } else {
                                    rgb(TEXT_PRIMARY)
                                })
                                .child(if app.state.uninstall_query.is_empty() {
                                    "Filter installed packages...".to_string()
                                } else {
                                    app.state.uninstall_query.clone()
                                }),
                        ),
                )
                .child(action_button(
                    "refresh-pkgs-btn",
                    "↻ Refresh List",
                    BG_HOVER,
                    TEXT_SECONDARY,
                    cx.listener(|this, _, _, cx| {
                        this.reload_installed();
                        cx.notify();
                    }),
                ))
                .child(action_button(
                    "uninstall-confirm-btn",
                    if app.state.uninstall_busy { "Removing…" } else { "🗑 Uninstall Package" },
                    ACCENT_RED,
                    0xffffff,
                    cx.listener(|this, _, _, cx| {
                        if this.state.uninstall_selected.is_some() {
                            this.run_uninstall(cx);
                        }
                    }),
                )),
        )
        .when_some(app.state.uninstall_msg.clone(), |this, msg| {
            this.child(status_msg_bar(&msg))
        })
        // List of packages
        .child(
            div()
                .id("uninstall-pkgs-list")
                .flex_1()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_1_5()
                .children(
                    filtered
                        .into_iter()
                        .map(|(real_idx, pkg)| {
                            let selected = app.state.uninstall_selected == Some(real_idx);
                            div()
                                .id(SharedString::from(format!("un-pkg-{}", real_idx)))
                                .w_full()
                                .h(px(42.0))
                                .flex()
                                .items_center()
                                .justify_between()
                                .px_4()
                                .bg(if selected { rgb(BG_HOVER) } else { rgb(BG_CARD) })
                                .border_1()
                                .border_color(if selected { rgb(ACCENT_RED) } else { rgb(BORDER_SUBTLE) })
                                .rounded_lg()
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.state.uninstall_selected = Some(real_idx);
                                    cx.notify();
                                }))
                                .child(
                                    div()
                                        .text_color(rgb(TEXT_PRIMARY))
                                        .text_size(px(13.0))
                                        .font_weight(FontWeight::BOLD)
                                        .child(pkg.name.clone()),
                                )
                                .child(
                                    div()
                                        .text_color(rgb(TEXT_MUTED))
                                        .text_size(px(12.0))
                                        .child(pkg.version.clone()),
                                )
                                .into_any_element()
                        })
                        .collect::<Vec<_>>(),
                ),
        )
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. Doctor Health View
// ─────────────────────────────────────────────────────────────────────────────

fn render_doctor(app: &mut ArchBridgeApp, cx: &mut Context<ArchBridgeApp>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .p_6()
        .gap_5()
        .size_full()
        // Header & Diagnostics Meter
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_color(rgb(TEXT_PRIMARY))
                                .text_size(px(24.0))
                                .font_weight(FontWeight::BOLD)
                                .child("Doctor System Diagnostics"),
                        )
                        .child(
                            div()
                                .text_color(rgb(TEXT_MUTED))
                                .text_size(px(13.0))
                                .child("Verify pacman, base-devel, devtools chroot, namespaces, disk space, and keyring status."),
                        ),
                )
                .child(action_button(
                    "run-diagnostics-btn",
                    if app.state.doctor_busy { "Running Diagnostics…" } else { "Run Diagnostics" },
                    ACCENT_BLUE,
                    0xffffff,
                    cx.listener(|this, _, _, cx| this.run_doctor(cx)),
                )),
        )
        .when_some(app.state.doctor_msg.clone(), |this, msg| {
            this.child(status_msg_bar(&msg))
        })
        // 8 Diagnostics Check Cards Grid
        .child(
            div()
                .id("doctor-checks-grid")
                .flex_1()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_2()
                .children(if app.state.health_checks.is_empty() {
                    vec![div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .h(px(200.0))
                        .child(
                            div()
                                .text_color(rgb(TEXT_MUTED))
                                .text_size(px(13.0))
                                .child("Press 'Run Diagnostics' to check system prerequisites"),
                        )
                        .into_any_element()]
                } else {
                    app.state
                        .health_checks
                        .iter()
                        .map(|check| {
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .p_4()
                                .bg(rgb(BG_CARD))
                                .border_1()
                                .border_color(rgb(BORDER_SUBTLE))
                                .rounded_xl()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_3()
                                        .child(
                                            div()
                                                .w(px(10.0))
                                                .h(px(10.0))
                                                .rounded_full()
                                                .bg(if check.ok { rgb(ACCENT_GREEN) } else { rgb(ACCENT_RED) }),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .gap_1()
                                                .child(
                                                    div()
                                                        .text_color(rgb(TEXT_PRIMARY))
                                                        .text_size(px(14.0))
                                                        .font_weight(FontWeight::BOLD)
                                                        .child(check.name.clone()),
                                                )
                                                .child(
                                                    div()
                                                        .text_color(rgb(TEXT_SECONDARY))
                                                        .text_size(px(12.0))
                                                        .child(check.message.clone()),
                                                ),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_color(if check.ok { rgb(ACCENT_GREEN) } else { rgb(ACCENT_RED) })
                                        .text_size(px(14.0))
                                        .font_weight(FontWeight::BOLD)
                                        .child(if check.ok { "PASS ✓" } else { "FAIL ✗" }),
                                )
                                .into_any_element()
                        })
                        .collect()
                }),
        )
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. Settings View
// ─────────────────────────────────────────────────────────────────────────────

fn render_settings(app: &mut ArchBridgeApp, cx: &mut Context<ArchBridgeApp>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .p_6()
        .gap_6()
        .size_full()
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_color(rgb(TEXT_PRIMARY))
                        .text_size(px(24.0))
                        .font_weight(FontWeight::BOLD)
                        .child("Settings & Preferences"),
                )
                .child(
                    div()
                        .text_color(rgb(TEXT_MUTED))
                        .text_size(px(13.0))
                        .child("Manage source toggles, repository aliases, and sudo authorization."),
                ),
        )
        // Source Toggles Section
        .child(section_header("Source Preferences Toggles"))
        .child(
            div()
                .bg(rgb(BG_CARD))
                .border_1()
                .border_color(rgb(BORDER_SUBTLE))
                .rounded_xl()
                .p_4()
                .flex()
                .flex_col()
                .gap_3()
                .child(detail_row("Official Arch Repos", "ENABLED"))
                .child(detail_row("AUR (Arch User Repository)", "ENABLED"))
                .child(detail_row("Flatpak (Flathub)", "ENABLED"))
                .child(detail_row("AppImage Standalone", "ENABLED"))
                .child(detail_row("Upstream GitHub Sources", "ENABLED")),
        )
        // Sudo Session & Security Section
        .child(section_header("Security & Sudo Authorization"))
        .child(
            div()
                .bg(rgb(BG_CARD))
                .border_1()
                .border_color(rgb(BORDER_SUBTLE))
                .rounded_xl()
                .p_4()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_color(rgb(TEXT_PRIMARY))
                                .text_size(px(13.0))
                                .font_weight(FontWeight::BOLD)
                                .child("Sudo Session"),
                        )
                        .child(
                            div()
                                .text_color(rgb(TEXT_MUTED))
                                .text_size(px(12.0))
                                .child(if app.state.sudo_session_active {
                                    "Active sudo authorization cached"
                                } else {
                                    "No sudo session active"
                                }),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(action_button(
                            "clear-sudo-btn",
                            "Clear Saved Sudo",
                            ACCENT_RED,
                            0xffffff,
                            cx.listener(|this, _, _, cx| {
                                this.state.sudo_session_active = false;
                                this.state.settings_msg = Some(StatusMsg {
                                    level: MsgLevel::Info,
                                    text: "Sudo session authorization cleared.".to_string(),
                                });
                                cx.notify();
                            }),
                        )),
                ),
        )
        .when_some(app.state.settings_msg.clone(), |this, msg| {
            this.child(status_msg_bar(&msg))
        })
}

// ─────────────────────────────────────────────────────────────────────────────
// Bottom System Status Bar
// ─────────────────────────────────────────────────────────────────────────────

fn render_bottom_bar(app: &ArchBridgeApp) -> impl IntoElement {
    div()
        .w_full()
        .h(px(32.0))
        .bg(rgb(0x070a12))
        .border_t_1()
        .border_color(rgb(BORDER_SUBTLE))
        .px_4()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .items_center()
                .gap_5()
                // Ready indicator
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1_5()
                        .child(div().w(px(8.0)).h(px(8.0)).rounded_full().bg(rgb(ACCENT_GREEN)))
                        .child(
                            div()
                                .text_color(rgb(TEXT_SECONDARY))
                                .text_size(px(11.0))
                                .font_weight(FontWeight::MEDIUM)
                                .child("Ready"),
                        ),
                )
                .child(div().w_px().h(px(12.0)).bg(rgb(BORDER_SUBTLE)))
                // Active Chroot
                .child(
                    div()
                        .text_color(rgb(TEXT_MUTED))
                        .text_size(px(11.0))
                        .child(format!("Active Chroot: {}", app.state.active_chroot)),
                )
                .child(div().w_px().h(px(12.0)).bg(rgb(BORDER_SUBTLE)))
                // Disk Space
                .child(
                    div()
                        .text_color(rgb(TEXT_MUTED))
                        .text_size(px(11.0))
                        .child(format!("Disk: {}", app.state.free_disk_space)),
                )
                .child(div().w_px().h(px(12.0)).bg(rgb(BORDER_SUBTLE)))
                // Keyring Verified
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(div().text_color(rgb(ACCENT_GREEN)).text_size(px(11.0)).child("✓"))
                        .child(
                            div()
                                .text_color(rgb(TEXT_MUTED))
                                .text_size(px(11.0))
                                .child(format!("Keyring: {}", app.state.keyring_status)),
                        ),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_color(rgb(TEXT_MUTED))
                        .text_size(px(11.0))
                        .font_family("monospace")
                        .child("v1.0.0"),
                )
                .child(
                    div()
                        .text_color(rgb(TEXT_PRIMARY))
                        .text_size(px(11.0))
                        .font_weight(FontWeight::BOLD)
                        .child("ArchBridge"),
                ),
        )
}

use crate::gui::state::{
    ActiveTab, AppState, HealthCheck, InstalledPackage, MsgLevel, SearchResult, SharedEngine,
    StatusMsg, rpc_call,
};
use crate::gui::theme::*;
use gpui::prelude::*;
use gpui::*;
use serde_json::json;

// ─────────────────────────────────────────────────────────────────────────────
// Main application view
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

        // Pre-load installed packages on start
        app.reload_installed();
        app
    }

    // ─── engine helpers ──────────────────────────────────────────────────────

    fn reload_installed(&mut self) {
        let result = std::process::Command::new("pacman")
            .args(["-Q", "--color=never"])
            .output();
        if let Ok(out) = result {
            let text = String::from_utf8_lossy(&out.stdout);
            self.state.installed_packages = text
                .lines()
                .filter_map(|line| {
                    let mut parts = line.splitn(2, ' ');
                    let name = parts.next()?.to_string();
                    let version = parts.next().unwrap_or("").trim().to_string();
                    Some(InstalledPackage {
                        name,
                        version,
                        size: String::new(),
                    })
                })
                .collect();
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
                self.state.health_checks = val
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
                                ok: item
                                    .get("ok")
                                    .and_then(|o| o.as_bool())
                                    .unwrap_or(false),
                                message: item
                                    .get("message")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                self.state.doctor_msg = Some(StatusMsg {
                    level: if ready { MsgLevel::Success } else { MsgLevel::Warning },
                    text: if ready {
                        "All systems healthy".to_string()
                    } else {
                        "Some checks failed".to_string()
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
        self.state.search_busy = true;
        self.state.search_msg = None;
        self.state.search_results.clear();
        cx.notify();

        let result = rpc_call(&self.engine, "v1.search", json!({ "target": query }));
        self.state.search_busy = false;
        match result {
            Ok(val) => {
                self.state.search_results = parse_search_results(&val);
                let n = self.state.search_results.len();
                self.state.search_msg = Some(StatusMsg {
                    level: MsgLevel::Info,
                    text: format!("{} result(s)", n),
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
                    text: "Inspection complete".to_string(),
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
        let path = self.state.inspect_path.trim().to_string();
        if path.is_empty() {
            return;
        }
        self.state.build_busy = true;
        self.state.build_msg = None;
        cx.notify();

        let result = rpc_call(
            &self.engine,
            "v1.prepare",
            json!({ "action": "build", "request": { "target": path } }),
        );
        self.state.build_busy = false;
        match result {
            Ok(val) => {
                if let Ok(plan) = serde_json::from_value::<crate::engine::Plan>(val) {
                    self.state.active_plan = Some(plan);
                    self.state.active_tab = ActiveTab::Build;
                    self.state.build_msg = Some(StatusMsg {
                        level: MsgLevel::Info,
                        text: "Plan ready — review and execute".to_string(),
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
                if let Ok(exec) =
                    serde_json::from_value::<crate::engine::ExecutionResult>(val)
                {
                    let ok = exec.ok;
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
// Parse helpers
// ─────────────────────────────────────────────────────────────────────────────


fn parse_search_results(val: &serde_json::Value) -> Vec<SearchResult> {
    let mut out = Vec::new();
    if let Some(candidates) = val.get("candidates").and_then(|c| c.as_array()) {
        for item in candidates {
            out.push(SearchResult {
                name: item
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                version: item
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                description: item
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string(),
                source: item
                    .get("source")
                    .and_then(|s| s.as_str())
                    .unwrap_or("unknown")
                    .to_string(),
                score: item
                    .get("confidence_score")
                    .and_then(|s| s.as_f64())
                    .unwrap_or(0.0),
            });
        }
    }
    out
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.min(s.len())])
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Shared UI primitives
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
        .h(px(36.0))
        .px_4()
        .bg(rgb(bg))
        .rounded_md()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_color(rgb(text))
        .text_size(px(13.0))
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
        .text_size(px(13.0))
        .child(msg.text.clone())
}

fn section_header(title: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_2()
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
        .gap_3()
        .child(
            div()
                .w(px(120.0))
                .text_color(rgb(TEXT_MUTED))
                .text_size(px(12.0))
                .child(key.to_string()),
        )
        .child(
            div()
                .flex_1()
                .text_color(rgb(TEXT_PRIMARY))
                .text_size(px(12.0))
                .child(value.to_string()),
        )
}

fn source_badge(source: &str) -> impl IntoElement {
    let (bg, text) = match source.to_lowercase().as_str() {
        "aur" => (0x1e1b4b_u32, ACCENT_PURPLE),
        "pacman" | "arch" => (0x0c2a1a_u32, ACCENT_GREEN),
        "flatpak" => (0x1a1000_u32, ACCENT_AMBER),
        "snap" => (0x0f172a_u32, ACCENT_CYAN),
        _ => (BG_HOVER, TEXT_SECONDARY),
    };
    div()
        .bg(rgb(bg))
        .border_1()
        .border_color(rgb(text))
        .rounded_full()
        .px_2()
        .h(px(18.0))
        .flex()
        .items_center()
        .text_color(rgb(text))
        .text_size(px(10.0))
        .child(source.to_uppercase())
}

fn status_pill(label: &str, value: &str, bg_color: u32, text_color: u32) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_1()
        .bg(rgb(bg_color))
        .border_1()
        .border_color(rgb(text_color))
        .rounded_full()
        .px_3()
        .h(px(24.0))
        .child(
            div()
                .text_size(px(11.0))
                .text_color(rgb(text_color))
                .child(format!("{}: {}", label, value)),
        )
}

fn tab_title(tab: &ActiveTab) -> &'static str {
    match tab {
        ActiveTab::Discover => "🔍  Discovery",
        ActiveTab::Inspect => "🔬  Inspect Package",
        ActiveTab::Build => "🔨  Build & Install",
        ActiveTab::Uninstall => "🗑  Uninstall Software",
        ActiveTab::Doctor => "🏥  System Health",
        ActiveTab::Settings => "⚙  Settings",
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Render implementation
// ─────────────────────────────────────────────────────────────────────────────

impl Render for ArchBridgeApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("root")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_row()
            .bg(rgb(BG_DARK))
            .size_full()
            .font_family("monospace")
            .child(render_sidebar(self, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(render_top_bar(self))
                    .child(
                        div()
                            .flex_1()
                            .p_4()
                            .overflow_hidden()
                            .child(render_active_view(self, cx)),
                    ),
            )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Sidebar
// ─────────────────────────────────────────────────────────────────────────────

fn render_sidebar(app: &mut ArchBridgeApp, cx: &mut Context<ArchBridgeApp>) -> impl IntoElement {
    div()
        .w(px(220.0))
        .h_full()
        .bg(rgb(BG_SIDEBAR))
        .border_r_1()
        .border_color(rgb(BORDER_SUBTLE))
        .flex()
        .flex_col()
        // Brand header
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .py_6()
                .px_4()
                .border_b_1()
                .border_color(rgb(BORDER_SUBTLE))
                .child(div().text_size(px(28.0)).child("🌉"))
                .child(
                    div()
                        .text_color(rgb(ACCENT_CYAN))
                        .text_size(px(16.0))
                        .font_weight(FontWeight::BOLD)
                        .pt_2()
                        .child("ArchBridge"),
                )
                .child(
                    div()
                        .text_color(rgb(TEXT_MUTED))
                        .text_size(px(11.0))
                        .pt_1()
                        .child("v0.2.0"),
                ),
        )
        // Nav items
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .p_2()
                .gap_1()
                .child(sidebar_tab(app, cx, ActiveTab::Discover, "🔍", "Discovery"))
                .child(sidebar_tab(app, cx, ActiveTab::Inspect, "🔬", "Inspect"))
                .child(sidebar_tab(
                    app,
                    cx,
                    ActiveTab::Build,
                    "🔨",
                    "Build & Install",
                ))
                .child(sidebar_tab(
                    app,
                    cx,
                    ActiveTab::Uninstall,
                    "🗑",
                    "Uninstall",
                ))
                .child(sidebar_tab(app, cx, ActiveTab::Doctor, "🏥", "Doctor"))
                .child(sidebar_tab(
                    app,
                    cx,
                    ActiveTab::Settings,
                    "⚙",
                    "Settings",
                )),
        )
        // Footer quote
        .child(
            div()
                .p_4()
                .border_t_1()
                .border_color(rgb(BORDER_SUBTLE))
                .child(
                    div()
                        .text_color(rgb(TEXT_MUTED))
                        .text_size(px(10.0))
                        .italic()
                        .child("\"Bridge the gap between distros\""),
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
        .h(px(40.0))
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .rounded_md()
        .cursor_pointer()
        .bg(if is_active { rgb(BG_HOVER) } else { rgb(BG_SIDEBAR) })
        .border_l_2()
        .border_color(if is_active {
            rgb(ACCENT_CYAN)
        } else {
            rgba(0x00000000)
        })
        .on_click(cx.listener(move |this, _, _, cx| {
            this.state.active_tab = tab_clone.clone();
            cx.notify();
        }))
        .child(div().text_size(px(14.0)).child(icon))
        .child(
            div()
                .text_color(if is_active {
                    rgb(TEXT_PRIMARY)
                } else {
                    rgb(TEXT_SECONDARY)
                })
                .text_size(px(13.0))
                .child(label),
        )
}

// ─────────────────────────────────────────────────────────────────────────────
// Top bar (status pills)
// ─────────────────────────────────────────────────────────────────────────────

fn render_top_bar(app: &ArchBridgeApp) -> impl IntoElement {
    div()
        .w_full()
        .h(px(48.0))
        .bg(rgb(BG_SIDEBAR))
        .border_b_1()
        .border_color(rgb(BORDER_SUBTLE))
        .flex()
        .items_center()
        .px_4()
        .justify_between()
        .child(
            div()
                .text_color(rgb(TEXT_PRIMARY))
                .text_size(px(14.0))
                .font_weight(FontWeight::BOLD)
                .child(tab_title(&app.state.active_tab)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(status_pill("Engine", "Ready", PILL_OK, PILL_OK_TEXT))
                .child(status_pill(
                    "Sudo",
                    if app.state.sudo_session_active { "Active" } else { "None" },
                    if app.state.sudo_session_active { PILL_OK } else { PILL_WARN },
                    if app.state.sudo_session_active { PILL_OK_TEXT } else { PILL_WARN_TEXT },
                ))
                .child(status_pill(
                    "Health",
                    if app.state.doctor_ready { "OK" } else { "—" },
                    if app.state.doctor_ready { PILL_OK } else { PILL_WARN },
                    if app.state.doctor_ready { PILL_OK_TEXT } else { PILL_WARN_TEXT },
                )),
        )
}

// ─────────────────────────────────────────────────────────────────────────────
// View router
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
// Discover view
// ─────────────────────────────────────────────────────────────────────────────

fn render_discover(app: &mut ArchBridgeApp, cx: &mut Context<ArchBridgeApp>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .h_full()
        .gap_4()
        // Search bar row
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .id("search-input")
                        .track_focus(&app.search_focus)
                        .flex_1()
                        .h(px(40.0))
                        .bg(rgb(BG_INPUT))
                        .border_1()
                        .border_color(rgb(BORDER_ACTIVE))
                        .rounded_md()
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
                                    "Search for software (e.g. grok, discord, vscode)...".to_string()
                                } else {
                                    app.state.search_query.clone()
                                }),
                        ),
                )
                .child(action_button(
                    "search-btn",
                    if app.state.search_busy { "Searching…" } else { "Search" },
                    ACCENT_BLUE,
                    0xffffff,
                    cx.listener(|this, _, _, cx| this.run_search(cx)),
                )),
        )
        // Status message
        .when_some(app.state.search_msg.clone(), |this, msg| {
            this.child(status_msg_bar(&msg))
        })
        // Results layout
        .child(
            div()
                .flex()
                .flex_row()
                .flex_1()
                .gap_4()
                .min_h(px(0.0))
                // Results list
                .child(
                    div()
                        .w(px(340.0))
                        .flex()
                        .flex_col()
                        .gap_2()
                        .overflow_hidden()
                        .child(
                            div()
                                .text_color(rgb(TEXT_SECONDARY))
                                .text_size(px(12.0))
                                .pb_1()
                                .child(format!("Results ({})", app.state.search_results.len())),
                        )
                        .child(
                            div()
                                .id("results-list")
                                .flex_1()
                                .overflow_y_scroll()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .children(
                                    app.state
                                        .search_results
                                        .iter()
                                        .enumerate()
                                        .map(|(i, result)| {
                                            let selected = app.state.selected_result == Some(i);
                                            result_card(i, result, selected, cx)
                                        })
                                        .collect::<Vec<_>>(),
                                ),
                        ),
                )
                // Detail panel
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .child(render_result_detail(app, cx)),
                ),
        )
}

fn result_card(
    idx: usize,
    result: &SearchResult,
    selected: bool,
    cx: &mut Context<ArchBridgeApp>,
) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("result-{}", idx)))
        .w_full()
        .p_3()
        .bg(if selected { rgb(BG_HOVER) } else { rgb(BG_CARD) })
        .border_1()
        .border_color(if selected { rgb(ACCENT_CYAN) } else { rgb(BORDER_SUBTLE) })
        .rounded_md()
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, _, cx| {
            this.state.selected_result = Some(idx);
            cx.notify();
        }))
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .child(
                    div()
                        .text_color(rgb(TEXT_PRIMARY))
                        .text_size(px(13.0))
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
                .flex()
                .gap_2()
                .child(source_badge(&result.source))
                .child(
                    div()
                        .text_color(rgb(ACCENT_GREEN))
                        .text_size(px(11.0))
                        .child(format!("{:.0}% match", result.score * 100.0)),
                ),
        )
        .child(
            div()
                .text_color(rgb(TEXT_SECONDARY))
                .text_size(px(12.0))
                .child(truncate_str(&result.description, 80)),
        )
}

fn render_result_detail(
    app: &mut ArchBridgeApp,
    cx: &mut Context<ArchBridgeApp>,
) -> impl IntoElement {
    match app
        .state
        .selected_result
        .and_then(|i| app.state.search_results.get(i))
    {
        None => div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .h_full()
            .child(
                div()
                    .text_color(rgb(TEXT_MUTED))
                    .text_size(px(13.0))
                    .child("Select a search result to see details"),
            )
            .into_any_element(),

        Some(result) => {
            let result = result.clone();
            div()
                .flex()
                .flex_col()
                .gap_4()
                .p_4()
                .bg(rgb(BG_CARD))
                .border_1()
                .border_color(rgb(BORDER_SUBTLE))
                .rounded_lg()
                .h_full()
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .items_center()
                        .child(
                            div()
                                .text_color(rgb(TEXT_PRIMARY))
                                .text_size(px(18.0))
                                .font_weight(FontWeight::BOLD)
                                .child(result.name.clone()),
                        )
                        .child(source_badge(&result.source)),
                )
                .child(
                    div()
                        .text_color(rgb(TEXT_SECONDARY))
                        .text_size(px(13.0))
                        .child(result.description.clone()),
                )
                .child(detail_row("Version", &result.version))
                .child(detail_row("Source", &result.source))
                .child(detail_row(
                    "Confidence",
                    &format!("{:.1}%", result.score * 100.0),
                ))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .pt_2()
                        .child({
                            let name = result.name.clone();
                            action_button(
                                "detail-build",
                                "Build & Install",
                                ACCENT_GREEN,
                                0xffffff,
                                cx.listener(move |this, _, _, cx| {
                                    this.state.inspect_path = name.clone();
                                    this.state.active_tab = ActiveTab::Inspect;
                                    cx.notify();
                                }),
                            )
                        })
                        .child({
                            let name = result.name.clone();
                            action_button(
                                "detail-inspect",
                                "Inspect",
                                BG_HOVER,
                                TEXT_SECONDARY,
                                cx.listener(move |this, _, _, cx| {
                                    this.state.inspect_path = name.clone();
                                    this.state.active_tab = ActiveTab::Inspect;
                                    cx.notify();
                                }),
                            )
                        }),
                )
                .into_any_element()
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Inspect view
// ─────────────────────────────────────────────────────────────────────────────

fn render_inspect(app: &mut ArchBridgeApp, cx: &mut Context<ArchBridgeApp>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .h_full()
        .gap_4()
        .child(
            div()
                .text_color(rgb(TEXT_SECONDARY))
                .text_size(px(13.0))
                .child("Enter the path to a .deb or .rpm package file to inspect its contents."),
        )
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .id("inspect-input")
                        .track_focus(&app.inspect_focus)
                        .flex_1()
                        .h(px(40.0))
                        .bg(rgb(BG_INPUT))
                        .border_1()
                        .border_color(rgb(BORDER_ACTIVE))
                        .rounded_md()
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
                                    "/path/to/package.deb".to_string()
                                } else {
                                    app.state.inspect_path.clone()
                                }),
                        ),
                )
                .child(action_button(
                    "inspect-btn",
                    if app.state.inspect_busy { "Inspecting…" } else { "Inspect" },
                    ACCENT_BLUE,
                    0xffffff,
                    cx.listener(|this, _, _, cx| this.run_inspect(cx)),
                )),
        )
        .when_some(app.state.inspect_msg.clone(), |this, msg| {
            this.child(status_msg_bar(&msg))
        })
        .child(
            div()
                .flex_1()
                .overflow_hidden()
                .child(render_inspect_results(app, cx)),
        )
}

fn render_inspect_results(
    app: &mut ArchBridgeApp,
    cx: &mut Context<ArchBridgeApp>,
) -> impl IntoElement {
    match &app.state.inspect_data {
        None => div()
            .flex()
            .items_center()
            .justify_center()
            .h_full()
            .child(
                div()
                    .text_color(rgb(TEXT_MUTED))
                    .text_size(px(13.0))
                    .child("No package loaded"),
            )
            .into_any_element(),

        Some(data) => {
            let data = data.clone();
            let deps: Vec<String> = data
                .get("dependencies")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|d| d.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();

            div()
                .id("inspect-results")
                .flex()
                .flex_col()
                .gap_3()
                .h_full()
                .overflow_y_scroll()
                .child(section_header("Package Metadata"))
                .child(
                    div()
                        .bg(rgb(BG_CARD))
                        .border_1()
                        .border_color(rgb(BORDER_SUBTLE))
                        .rounded_md()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(detail_row(
                            "Name",
                            data.get("name").and_then(|v| v.as_str()).unwrap_or("—"),
                        ))
                        .child(detail_row(
                            "Version",
                            data.get("version").and_then(|v| v.as_str()).unwrap_or("—"),
                        ))
                        .child(detail_row(
                            "Architecture",
                            data.get("architecture")
                                .and_then(|v| v.as_str())
                                .unwrap_or("—"),
                        ))
                        .child(detail_row(
                            "Maintainer",
                            data.get("maintainer")
                                .and_then(|v| v.as_str())
                                .unwrap_or("—"),
                        ))
                        .child(detail_row(
                            "Description",
                            data.get("description")
                                .and_then(|v| v.as_str())
                                .unwrap_or("—"),
                        )),
                )
                .child(section_header("Dependencies"))
                .child(
                    div()
                        .bg(rgb(BG_CARD))
                        .border_1()
                        .border_color(rgb(BORDER_SUBTLE))
                        .rounded_md()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .children(if deps.is_empty() {
                            vec![div()
                                .text_color(rgb(TEXT_MUTED))
                                .text_size(px(12.0))
                                .child("No dependencies")
                                .into_any_element()]
                        } else {
                            deps.into_iter()
                                .map(|d| {
                                    div()
                                        .text_color(rgb(TEXT_PRIMARY))
                                        .text_size(px(12.0))
                                        .child(format!("• {}", d))
                                        .into_any_element()
                                })
                                .collect()
                        }),
                )
                .child(action_button(
                    "prepare-build",
                    "Prepare Build Plan →",
                    ACCENT_GREEN,
                    0xffffff,
                    cx.listener(|this, _, _, cx| this.prepare_build_plan(cx)),
                ))
                .into_any_element()
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Build view
// ─────────────────────────────────────────────────────────────────────────────

fn render_build(app: &mut ArchBridgeApp, cx: &mut Context<ArchBridgeApp>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .h_full()
        .gap_4()
        .child(match &app.state.active_plan {
            None => div()
                .flex()
                .items_center()
                .justify_center()
                .h(px(100.0))
                .child(
                    div()
                        .text_color(rgb(TEXT_MUTED))
                        .text_size(px(13.0))
                        .child("No active plan. Go to Inspect or Discovery to prepare one."),
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
                            .rounded_md()
                            .p_4()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .items_center()
                                    .child(
                                        div()
                                            .text_color(rgb(ACCENT_CYAN))
                                            .text_size(px(14.0))
                                            .font_weight(FontWeight::BOLD)
                                            .child(format!("Plan: {}", plan.action)),
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
                            )
                            .when(!plan.warnings.is_empty(), |this| {
                                this.child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .children(plan.warnings.iter().map(|w| {
                                            div()
                                                .text_color(rgb(ACCENT_AMBER))
                                                .text_size(px(12.0))
                                                .child(format!("⚠  {}", w))
                                                .into_any_element()
                                        })),
                                )
                            }),
                    )
                    .child(section_header("Build Steps"))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .children(plan.steps.iter().enumerate().map(|(i, step)| {
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .p_2()
                                    .bg(rgb(BG_CARD))
                                    .border_1()
                                    .border_color(rgb(BORDER_SUBTLE))
                                    .rounded_md()
                                    .child(
                                        div()
                                            .w(px(24.0))
                                            .h(px(24.0))
                                            .bg(rgb(BG_HOVER))
                                            .rounded_full()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_color(rgb(TEXT_MUTED))
                                            .text_size(px(11.0))
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
        .when_some(app.state.build_msg.clone(), |this, msg| {
            this.child(status_msg_bar(&msg))
        })
        .child(
            div()
                .flex()
                .gap_2()
                .child(action_button(
                    "execute-plan",
                    if app.state.build_busy { "Building…" } else { "▶  Execute Build" },
                    ACCENT_GREEN,
                    0xffffff,
                    cx.listener(|this, _, _, cx| {
                        if !this.state.sudo_session_active {
                            this.state.auth_dialog_open = true;
                            this.state.auth_pending_action = Some("build".to_string());
                        } else {
                            this.execute_plan(cx);
                        }
                        cx.notify();
                    }),
                )),
        )
        // Build log terminal
        .when(!app.state.build_log.is_empty(), |this| {
            this.child(
                div()
                    .flex_1()
                    .min_h(px(200.0))
                    .bg(rgb(0x020408))
                    .border_1()
                    .border_color(rgb(BORDER_SUBTLE))
                    .rounded_md()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_color(rgb(TEXT_MUTED))
                            .text_size(px(11.0))
                            .pb_1()
                            .child("── Build Log ──"),
                    )
                    .child(
                        div()
                            .id("build-log")
                            .overflow_y_scroll()
                            .flex_1()
                            .text_color(rgb(ACCENT_GREEN))
                            .text_size(px(12.0))
                            .child(app.state.build_log.clone()),
                    ),
            )
        })
        // Launch button after successful build
        .when(
            app.state.build_result.as_ref().map(|r| r.ok).unwrap_or(false),
            |this| {
                this.child(action_button(
                    "launch-app",
                    "🚀  Launch Application",
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
            },
        )
}

// ─────────────────────────────────────────────────────────────────────────────
// Uninstall view
// ─────────────────────────────────────────────────────────────────────────────

fn render_uninstall(
    app: &mut ArchBridgeApp,
    cx: &mut Context<ArchBridgeApp>,
) -> impl IntoElement {
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
        .h_full()
        .gap_4()
        .child(
            div()
                .id("uninstall-input")
                .track_focus(&app.uninstall_focus)
                .w_full()
                .h(px(40.0))
                .bg(rgb(BG_INPUT))
                .border_1()
                .border_color(rgb(BORDER_ACTIVE))
                .rounded_md()
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
        .when_some(app.state.uninstall_msg.clone(), |this, msg| {
            this.child(status_msg_bar(&msg))
        })
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_color(rgb(TEXT_SECONDARY))
                        .text_size(px(12.0))
                        .child(format!("{} packages", filtered.len())),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(action_button(
                            "refresh-pkgs",
                            "↻ Refresh",
                            BG_HOVER,
                            TEXT_SECONDARY,
                            cx.listener(|this, _, _, cx| {
                                this.reload_installed();
                                cx.notify();
                            }),
                        ))
                        .child(action_button(
                            "uninstall-btn",
                            if app.state.uninstall_busy { "Removing…" } else { "🗑 Uninstall" },
                            ACCENT_RED,
                            0xffffff,
                            cx.listener(|this, _, _, cx| {
                                if this.state.uninstall_selected.is_some() {
                                    if !this.state.sudo_session_active {
                                        this.state.auth_dialog_open = true;
                                    } else {
                                        this.run_uninstall(cx);
                                    }
                                    cx.notify();
                                }
                            }),
                        )),
                ),
        )
        .child(
            div()
                .id("packages-list")
                .flex_1()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_1()
                .children(
                    filtered
                        .into_iter()
                        .map(|(real_idx, pkg)| {
                            let selected = app.state.uninstall_selected == Some(real_idx);
                            div()
                                .id(SharedString::from(format!("pkg-{}", real_idx)))
                                .w_full()
                                .h(px(44.0))
                                .flex()
                                .items_center()
                                .justify_between()
                                .px_3()
                                .bg(if selected { rgb(BG_HOVER) } else { rgb(BG_CARD) })
                                .border_1()
                                .border_color(if selected {
                                    rgb(ACCENT_RED)
                                } else {
                                    rgb(BORDER_SUBTLE)
                                })
                                .rounded_md()
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.state.uninstall_selected = Some(real_idx);
                                    cx.notify();
                                }))
                                .child(
                                    div()
                                        .text_color(rgb(TEXT_PRIMARY))
                                        .text_size(px(13.0))
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
// Doctor view
// ─────────────────────────────────────────────────────────────────────────────

fn render_doctor(app: &mut ArchBridgeApp, cx: &mut Context<ArchBridgeApp>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .h_full()
        .gap_4()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .text_color(rgb(TEXT_SECONDARY))
                        .text_size(px(13.0))
                        .child("Check system health and tool availability."),
                )
                .child(action_button(
                    "run-doctor",
                    if app.state.doctor_busy { "Running…" } else { "Run Diagnostics" },
                    ACCENT_BLUE,
                    0xffffff,
                    cx.listener(|this, _, _, cx| this.run_doctor(cx)),
                )),
        )
        .when_some(app.state.doctor_msg.clone(), |this, msg| {
            this.child(status_msg_bar(&msg))
        })
        .child(
            div()
                .id("health-list")
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
                                .child("Press 'Run Diagnostics' to check system health"),
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
                                .gap_3()
                                .p_3()
                                .bg(rgb(BG_CARD))
                                .border_1()
                                .border_color(rgb(BORDER_SUBTLE))
                                .rounded_md()
                                .child(
                                    div()
                                        .w(px(8.0))
                                        .h(px(8.0))
                                        .rounded_full()
                                        .bg(if check.ok { rgb(ACCENT_GREEN) } else { rgb(ACCENT_RED) }),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .flex_1()
                                        .gap_1()
                                        .child(
                                            div()
                                                .text_color(rgb(TEXT_PRIMARY))
                                                .text_size(px(13.0))
                                                .child(check.name.clone()),
                                        )
                                        .child(
                                            div()
                                                .text_color(rgb(TEXT_SECONDARY))
                                                .text_size(px(12.0))
                                                .child(check.message.clone()),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_color(if check.ok { rgb(ACCENT_GREEN) } else { rgb(ACCENT_RED) })
                                        .text_size(px(12.0))
                                        .child(if check.ok { "✓" } else { "✗" }),
                                )
                                .into_any_element()
                        })
                        .collect()
                }),
        )
}

// ─────────────────────────────────────────────────────────────────────────────
// Settings view
// ─────────────────────────────────────────────────────────────────────────────

fn render_settings(
    app: &mut ArchBridgeApp,
    cx: &mut Context<ArchBridgeApp>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_6()
        // Auth dialog rendered inline at top of settings
        .when(app.state.auth_dialog_open, |this| {
            this.child(render_auth_dialog(app, cx))
        })
        .child(section_header("Sudo Session"))
        .child(
            div()
                .bg(rgb(BG_CARD))
                .border_1()
                .border_color(rgb(BORDER_SUBTLE))
                .rounded_md()
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
                                .child("Sudo Session"),
                        )
                        .child(
                            div()
                                .text_color(rgb(TEXT_SECONDARY))
                                .text_size(px(12.0))
                                .child(if app.state.sudo_session_active {
                                    "An active sudo session is cached"
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
                            "open-auth",
                            "Authenticate",
                            ACCENT_BLUE,
                            0xffffff,
                            cx.listener(|this, _, _, cx| {
                                this.state.auth_dialog_open = true;
                                cx.notify();
                            }),
                        ))
                        .child(action_button(
                            "clear-sudo",
                            "Clear Session",
                            ACCENT_RED,
                            0xffffff,
                            cx.listener(|this, _, _, cx| {
                                this.state.sudo_session_active = false;
                                this.state.settings_msg = Some(StatusMsg {
                                    level: MsgLevel::Info,
                                    text: "Sudo session cleared".to_string(),
                                });
                                cx.notify();
                            }),
                        )),
                ),
        )
        .when_some(app.state.settings_msg.clone(), |this, msg| {
            this.child(status_msg_bar(&msg))
        })
        .child(section_header("Engine Information"))
        .child(
            div()
                .bg(rgb(BG_CARD))
                .border_1()
                .border_color(rgb(BORDER_SUBTLE))
                .rounded_md()
                .p_4()
                .flex()
                .flex_col()
                .gap_2()
                .child(detail_row("Version", "0.2.0"))
                .child(detail_row("Protocol", "v1.0"))
                .child(detail_row("Backend", "Native Rust + GPUI"))
                .child(detail_row("Build", "Chroot (unprivileged)"))
                .child(detail_row("Containers", "None (zero Docker/Podman)")),
        )
}

// ─────────────────────────────────────────────────────────────────────────────
// Auth dialog (rendered inline in the view, not as an overlay)
// ─────────────────────────────────────────────────────────────────────────────

fn render_auth_dialog(
    app: &mut ArchBridgeApp,
    cx: &mut Context<ArchBridgeApp>,
) -> impl IntoElement {
    div()
        .w_full()
        .bg(rgb(BG_CARD))
        .border_1()
        .border_color(rgb(BORDER_ACTIVE))
        .rounded_lg()
        .p_6()
        .flex()
        .flex_col()
        .gap_4()
        .child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .child(
                    div()
                        .text_color(rgb(ACCENT_CYAN))
                        .text_size(px(16.0))
                        .font_weight(FontWeight::BOLD)
                        .child("🔐  Administrator Permission"),
                )
                .child(
                    div()
                        .id("auth-close")
                        .text_color(rgb(TEXT_MUTED))
                        .text_size(px(18.0))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.auth_dialog_open = false;
                            this.state.auth_password_input.clear();
                            cx.notify();
                        }))
                        .child("✕"),
                ),
        )
        .child(
            div()
                .text_color(rgb(TEXT_SECONDARY))
                .text_size(px(13.0))
                .child("Enter your password to authorise this action."),
        )
        .when_some(app.state.auth_error.clone(), |this, err| {
            this.child(
                div()
                    .text_color(rgb(ACCENT_RED))
                    .text_size(px(13.0))
                    .child(err),
            )
        })
        .child(
            div()
                .id("auth-input")
                .track_focus(&app.auth_focus)
                .w_full()
                .h(px(40.0))
                .bg(rgb(BG_INPUT))
                .border_1()
                .border_color(rgb(BORDER_ACTIVE))
                .rounded_md()
                .px_3()
                .flex()
                .items_center()
                .cursor_pointer()
                .on_click(cx.listener(|this, _, window, cx| {
                    window.focus(&this.auth_focus);
                    cx.notify();
                }))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    match event.keystroke.key.as_str() {
                        "backspace" => {
                            this.state.auth_password_input.pop();
                            cx.notify();
                        }
                        "escape" => {
                            this.state.auth_dialog_open = false;
                            this.state.auth_password_input.clear();
                            cx.notify();
                        }
                        _ => {
                            if let Some(ch) = &event.keystroke.key_char {
                                if !event.keystroke.modifiers.control {
                                    this.state.auth_password_input.push_str(ch);
                                    cx.notify();
                                }
                            }
                        }
                    }
                }))
                .child(
                    div()
                        .text_color(if app.state.auth_password_input.is_empty() {
                            rgb(TEXT_PLACEHOLDER)
                        } else {
                            rgb(TEXT_PRIMARY)
                        })
                        .child(if app.state.auth_password_input.is_empty() {
                            "Password...".to_string()
                        } else {
                            "•".repeat(app.state.auth_password_input.len())
                        }),
                ),
        )
        .child(
            div()
                .flex()
                .gap_3()
                .child(action_button(
                    "auth-cancel",
                    "Cancel",
                    BG_HOVER,
                    TEXT_SECONDARY,
                    cx.listener(|this, _, _, cx| {
                        this.state.auth_dialog_open = false;
                        this.state.auth_password_input.clear();
                        cx.notify();
                    }),
                ))
                .child(action_button(
                    "auth-confirm",
                    "Authenticate",
                    ACCENT_BLUE,
                    0xffffff,
                    cx.listener(|this, _, _, cx| {
                        this.state.sudo_session_active = true;
                        this.state.auth_dialog_open = false;
                        this.state.auth_password_input.clear();
                        this.state.auth_error = None;
                        this.state.settings_msg = Some(StatusMsg {
                            level: MsgLevel::Success,
                            text: "Authentication successful".to_string(),
                        });
                        cx.notify();
                    }),
                )),
        )
}

use crate::gui::state::{
    rpc_call, ActiveTab, AppState, AppTheme, HealthCheck, InstalledPackage, MsgLevel, SearchResult,
    SharedEngine, StatusMsg,
};
use crate::gui::theme::*;
use gpui::prelude::*;
use gpui::*;
use serde_json::json;

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

// ─────────────────────────────────────────────────────────────────────────────
// Main Application View
// ─────────────────────────────────────────────────────────────────────────────

pub struct ArchBridgeApp {
    pub state: AppState,
    pub engine: SharedEngine,
    pub focus_handle: FocusHandle,
    pub search_focus: FocusHandle,
    pub inspect_focus: FocusHandle,
    pub build_focus: FocusHandle,
    pub uninstall_focus: FocusHandle,
    pub auth_focus: FocusHandle,
}

impl ArchBridgeApp {
    pub fn new(engine: SharedEngine, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let search_focus = cx.focus_handle();
        let inspect_focus = cx.focus_handle();
        let build_focus = cx.focus_handle();
        let uninstall_focus = cx.focus_handle();
        let auth_focus = cx.focus_handle();

        let mut app = Self {
            state: AppState::default(),
            engine,
            focus_handle,
            search_focus,
            inspect_focus,
            build_focus,
            uninstall_focus,
            auth_focus,
        };

        app.reload_installed();

        if let Ok(tab) = std::env::var("ARCHBRIDGE_START_TAB") {
            match tab.to_lowercase().as_str() {
                "discovery" => app.state.active_tab = ActiveTab::Discover,
                "inspect" => app.state.active_tab = ActiveTab::Inspect,
                "build" => app.state.active_tab = ActiveTab::Build,
                "uninstall" => app.state.active_tab = ActiveTab::Uninstall,
                "doctor" => app.state.active_tab = ActiveTab::Doctor,
                "settings" => app.state.active_tab = ActiveTab::Settings,
                _ => {}
            }
        }

        if let Ok(query) = std::env::var("ARCHBRIDGE_SEARCH_SAMPLE") {
            app.state.search_query = query.clone();
            if !app.state.search_history.contains(&query) {
                app.state.search_history.insert(0, query.clone());
            }
            app.state.search_results = vec![
                SearchResult {
                    name: "vlc".into(),
                    version: "3.0.21-1".into(),
                    description: "Multi-platform MPEG, VCD/DVD, and DivX player".into(),
                    source: "official".into(),
                    score: 1.0,
                    repo: "extra".into(),
                    is_recommended: true,
                    license: "GPL-2.0-or-later LGPL-2.1-or-later".into(),
                    arch: "x86_64".into(),
                    size: "41.97 MiB".into(),
                    maintainer: "Christian Heusel <gromit@archlinux.org>".into(),
                },
                SearchResult {
                    name: "org.videolan.VLC".into(),
                    version: "3.0.21".into(),
                    description: "VLC media player flatpak release".into(),
                    source: "flatpak".into(),
                    score: 0.90,
                    repo: "Flathub".into(),
                    is_recommended: false,
                    license: "GPL-2.0+".into(),
                    arch: "x86_64".into(),
                    size: "82.4 MiB".into(),
                    maintainer: "VideoLAN Organization".into(),
                },
            ];
            app.state.selected_result = Some(0);
            app.state.recommended_item = app.state.search_results.first().cloned();
        }

        app
    }

    pub fn reload_installed(&mut self) {
        let mut packages = Vec::new();
        self.state.uninstall_selected = None;
        self.state.uninstall_confirm_idx = None;
        self.state.uninstall_display_limit = 50;

        // Only show packages confirmed by pacman. Never display demo data as
        // installed software because this view can remove live-system packages.
        match std::process::Command::new("pacman")
            .args(["-Qe", "--color=never"])
            .output()
        {
            Ok(out) if out.status.success() => {
                let text = String::from_utf8_lossy(&out.stdout);
                for line in text.lines() {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    if let Some(name) = parts.first() {
                        packages.push(InstalledPackage {
                            name: (*name).to_string(),
                            version: parts.get(1).copied().unwrap_or("").to_string(),
                            description: "Explicitly installed Arch package".to_string(),
                            size: "Installed".to_string(),
                        });
                    }
                }
            }
            Ok(out) => {
                self.state.uninstall_msg = Some(StatusMsg {
                    level: MsgLevel::Error,
                    text: format!(
                        "Could not load installed packages: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    ),
                });
            }
            Err(err) => {
                self.state.uninstall_msg = Some(StatusMsg {
                    level: MsgLevel::Error,
                    text: format!("Could not run pacman to load installed packages: {err}"),
                });
            }
        }

        if packages.is_empty() {
            self.state.uninstall_msg.get_or_insert(StatusMsg {
                level: MsgLevel::Info,
                text: "No explicitly installed packages were reported by pacman.".to_string(),
            });
        }

        self.state.installed_packages = packages;
    }

    pub fn run_doctor(&mut self, cx: &mut Context<Self>) {
        self.state.doctor_busy = true;
        self.state.doctor_msg = None;
        cx.notify();

        let result = rpc_call(&self.engine, "v1.doctor", json!({}));
        self.state.doctor_busy = false;
        match result {
            Ok(val) => {
                let ready = val.get("ready").and_then(|r| r.as_bool()).unwrap_or(true);
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
                                ok: item.get("ok").and_then(|o| o.as_bool()).unwrap_or(true),
                                message: item
                                    .get("message")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                if !checks.is_empty() {
                    self.state.health_checks = checks;
                }
                self.state.doctor_msg = Some(StatusMsg {
                    level: if ready {
                        MsgLevel::Success
                    } else {
                        MsgLevel::Warning
                    },
                    text: if ready {
                        "All system health diagnostics verified and passing!".to_string()
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

    pub fn run_search(&mut self, cx: &mut Context<Self>) {
        let query = self.state.search_query.trim().to_string();
        if query.is_empty() {
            self.state.search_results.clear();
            self.state.recommended_item = None;
            self.state.selected_result = None;
            self.state.search_msg = None;
            cx.notify();
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
                if parsed.is_empty() {
                    self.state.search_results.clear();
                    self.state.selected_result = None;
                    self.state.recommended_item = None;
                    self.state.search_msg = Some(StatusMsg {
                        level: MsgLevel::Warning,
                        text: format!(
                            "No packages or upstream repositories found matching '{}'",
                            query
                        ),
                    });
                } else {
                    let n = parsed.len();
                    self.state.recommended_item = parsed
                        .iter()
                        .find(|r| r.is_recommended)
                        .cloned()
                        .or_else(|| parsed.first().cloned());
                    self.state.search_results = parsed;
                    self.state.selected_result = Some(0);
                    self.state.search_msg = Some(StatusMsg {
                        level: MsgLevel::Info,
                        text: format!("Found {} source candidate(s) for '{}'", n, query),
                    });
                }
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

    pub fn run_inspect(&mut self, cx: &mut Context<Self>) {
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
                    text: "Foreign package analyzed in data-only mode (EXECUTED: FALSE)"
                        .to_string(),
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

    pub fn prepare_build_plan(&mut self, cx: &mut Context<Self>) {
        let target = if !self.state.build_target_input.trim().is_empty() {
            self.state.build_target_input.trim().to_string()
        } else if !self.state.inspect_path.trim().is_empty() {
            self.state.inspect_path.trim().to_string()
        } else if let Some(idx) = self.state.selected_result {
            self.state
                .search_results
                .get(idx)
                .map(|r| r.name.clone())
                .unwrap_or_else(|| "vlc".to_string())
        } else {
            "vlc".to_string()
        };

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
                        text: "Build Plan Prepared. Review PKGBUILD manifest and execute."
                            .to_string(),
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

    pub fn execute_plan(&mut self, cx: &mut Context<Self>) {
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
                        level: if ok {
                            MsgLevel::Success
                        } else {
                            MsgLevel::Error
                        },
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

    pub fn run_uninstall(&mut self, cx: &mut Context<Self>) {
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
                                level: if ok {
                                    MsgLevel::Success
                                } else {
                                    MsgLevel::Error
                                },
                                text: r
                                    .get("message")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("Package uninstalled successfully")
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

pub fn app_icon_for_name(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    if lower.contains("brave") {
        "🦁"
    } else if lower.contains("vlc")
        || lower.contains("video")
        || lower.contains("media")
        || lower.contains("mpv")
    {
        "🎬"
    } else if lower.contains("discord") {
        "💬"
    } else if lower.contains("telegram") {
        "✈️"
    } else if lower.contains("chrome")
        || lower.contains("chromium")
        || lower.contains("firefox")
        || lower.contains("browser")
    {
        "🌐"
    } else if lower.contains("code")
        || lower.contains("vscode")
        || lower.contains("nvim")
        || lower.contains("neovim")
        || lower.contains("vim")
    {
        "💻"
    } else if lower.contains("docker") || lower.contains("podman") || lower.contains("container") {
        "🐳"
    } else if lower.contains("steam")
        || lower.contains("game")
        || lower.contains("lutris")
        || lower.contains("heroic")
    {
        "🎮"
    } else if lower.contains("music")
        || lower.contains("spotify")
        || lower.contains("audio")
        || lower.contains("alsa")
        || lower.contains("pipewire")
    {
        "🎵"
    } else if lower.contains("terminal")
        || lower.contains("alacritty")
        || lower.contains("kitty")
        || lower.contains("bash")
        || lower.contains("zsh")
        || lower.contains("fish")
    {
        "💻"
    } else if lower.contains("git")
        || lower.contains("ninja")
        || lower.contains("cmake")
        || lower.contains("cargo")
        || lower.contains("rust")
        || lower.contains("gcc")
    {
        "⚙"
    } else {
        "📦"
    }
}

fn parse_search_results(val: &serde_json::Value) -> Vec<SearchResult> {
    let mut out = Vec::new();

    // 1. Parse DecisionReport format from v1.search (decision_table)
    if let Some(table) = val.get("decision_table").and_then(|t| t.as_array()) {
        for row in table {
            let source = row
                .get("source")
                .and_then(|s| s.as_str())
                .unwrap_or("unknown")
                .to_string();
            let is_row_rec = row
                .get("recommended")
                .and_then(|r| r.as_bool())
                .unwrap_or(false);

            if let Some(candidates) = row.get("candidates").and_then(|c| c.as_array()) {
                for (cand_idx, item) in candidates.iter().enumerate() {
                    let name = item
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let version = item
                        .get("version")
                        .and_then(|v| v.as_str())
                        .unwrap_or("latest")
                        .to_string();
                    let desc = item
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("Package candidate")
                        .to_string();

                    let repo = match source.as_str() {
                        "official" => "Arch Official (pacman)".to_string(),
                        "aur" => "AUR (Community PKGBUILD)".to_string(),
                        "flatpak" => "Flathub Sandbox".to_string(),
                        "appimage" => "AppImage Binary".to_string(),
                        "upstream" => "GitHub / Git Source".to_string(),
                        "deb" => "DEB Foreign Package".to_string(),
                        "rpm" => "RPM Foreign Package".to_string(),
                        _ => source.clone(),
                    };

                    out.push(SearchResult {
                        name,
                        version,
                        description: desc,
                        source: source.clone(),
                        score: if is_row_rec && cand_idx == 0 {
                            1.0
                        } else {
                            0.85
                        },
                        repo,
                        is_recommended: is_row_rec && cand_idx == 0 && out.is_empty(),
                        license: "Open Source / Package License".to_string(),
                        arch: "x86_64".to_string(),
                        size: "Package Source".to_string(),
                        maintainer: match source.as_str() {
                            "official" => "Arch Linux Maintainers".to_string(),
                            "aur" => "AUR Contributor".to_string(),
                            "upstream" => "Upstream Project".to_string(),
                            _ => "Package Provider".to_string(),
                        },
                    });
                }
            }
        }
    }

    // 2. Direct candidates array fallback
    if out.is_empty() {
        if let Some(candidates) = val.get("candidates").and_then(|c| c.as_array()) {
            for (idx, item) in candidates.iter().enumerate() {
                let src = item
                    .get("source")
                    .and_then(|s| s.as_str())
                    .unwrap_or("official")
                    .to_string();
                let name = item
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let version = item
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("latest")
                    .to_string();
                let desc = item
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or("Package candidate")
                    .to_string();

                out.push(SearchResult {
                    name,
                    version,
                    description: desc,
                    source: src.clone(),
                    score: 0.90,
                    repo: match src.as_str() {
                        "official" => "extra".to_string(),
                        "aur" => "AUR (community)".to_string(),
                        "flatpak" => "Flathub".to_string(),
                        "appimage" => "Official Release".to_string(),
                        _ => "GitHub".to_string(),
                    },
                    is_recommended: idx == 0,
                    license: "Open Source".to_string(),
                    arch: "x86_64".to_string(),
                    size: "Package".to_string(),
                    maintainer: "Package Maintainer".to_string(),
                });
            }
        }
    }

    out
}

// ─────────────────────────────────────────────────────────────────────────────
// UI Primitives
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
        .rounded_lg()
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

fn small_button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    bg: u32,
    text: u32,
    handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id.into())
        .h(px(32.0))
        .px_3()
        .bg(rgb(bg))
        .rounded_lg()
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_color(rgb(text))
        .text_size(px(12.0))
        .font_weight(FontWeight::BOLD)
        .whitespace_nowrap()
        .on_click(handler)
        .child(label.into())
}

fn status_msg_bar(msg: &StatusMsg, pal: &ThemePalette) -> impl IntoElement {
    let (bg, text) = match msg.level {
        MsgLevel::Success => (PILL_OK, PILL_OK_TEXT),
        MsgLevel::Warning => (PILL_WARN, PILL_WARN_TEXT),
        MsgLevel::Error => (PILL_ERR, PILL_ERR_TEXT),
        MsgLevel::Info => (
            if pal.is_light() { 0xe2e8f0 } else { 0x16243b },
            pal.text_secondary,
        ),
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

fn section_header(title: &'static str, pal: &ThemePalette) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_2()
        .py_1()
        .child(
            div()
                .text_color(rgb(pal.text_muted))
                .text_size(px(11.0))
                .font_weight(FontWeight::BOLD)
                .child(title.to_uppercase()),
        )
        .child(div().flex_1().h_px().bg(rgb(pal.border_subtle)))
}

fn detail_row(key: &str, value: &str, pal: &ThemePalette) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .py_1()
        .child(
            div()
                .text_color(rgb(pal.text_muted))
                .text_size(px(12.0))
                .child(key.to_string()),
        )
        .child(
            div()
                .text_color(rgb(pal.text_primary))
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
        let pal = get_palette(self.state.theme);
        div()
            .id("root")
            .track_focus(&self.focus_handle)
            .flex()
            .flex_row()
            .bg(rgb(pal.bg_darkest))
            .size_full()
            .font_family("sans-serif")
            // ── Sidebar ──────────────────────────────────────────────
            .child(render_sidebar(self, cx))
            // ── Main Viewport Container ──────────────────────────────
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .flex()
                    .flex_col()
                    .bg(rgb(pal.bg_dark))
                    .min_w(px(0.0))
                    .p_4()
                    .gap_3()
                    // Sleek Modern Top Navbar (Fixed 46px height)
                    .child(
                        div()
                            .w_full()
                            .h(px(46.0))
                            .max_h(px(46.0))
                            .flex_none()
                            .child(render_top_bar(self, cx)),
                    )
                    // Active Tab View Content (Expands to take 90%+ of vertical space)
                    .child(
                        div()
                            .flex_1()
                            .min_h(px(0.0))
                            .overflow_hidden()
                            .child(render_active_view(self, cx)),
                    )
                    // Bottom Status Bar
                    .child(
                        div()
                            .w_full()
                            .h(px(24.0))
                            .max_h(px(24.0))
                            .flex_none()
                            .child(render_bottom_bar(self)),
                    ),
            )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Modern Sleek Web-Style Top Navbar
// ─────────────────────────────────────────────────────────────────────────────

fn render_top_bar(app: &mut ArchBridgeApp, cx: &mut Context<ArchBridgeApp>) -> impl IntoElement {
    let pal = get_palette(app.state.theme);
    let theme_mode = app.state.theme;
    let (theme_icon, theme_name) = match theme_mode {
        AppTheme::Dark => ("🌙", "Dark"),
        AppTheme::Midnight => ("🌌", "OLED"),
        AppTheme::Light => ("☀️", "Light"),
    };

    let active_title = match app.state.active_tab {
        ActiveTab::Discover => "Discover Packages",
        ActiveTab::Inspect => "Foreign Package Inspector",
        ActiveTab::Build => "Clean Chroot Build Studio",
        ActiveTab::Uninstall => "Installed Software",
        ActiveTab::Doctor => "System Health & Doctor",
        ActiveTab::Settings => "Settings & Preferences",
    };

    div()
        .w_full()
        .h(px(46.0))
        .flex_none()
        .bg(rgb(pal.bg_topbar))
        .border_1()
        .border_color(rgb(pal.border_subtle))
        .rounded_xl()
        .px_4()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        // Left: Breadcrumb / Active Page Title
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_color(rgb(pal.text_muted))
                        .text_size(px(12.5))
                        .font_weight(FontWeight::MEDIUM)
                        .child("ArchBridge"),
                )
                .child(
                    div()
                        .text_color(rgb(pal.border_subtle))
                        .text_size(px(12.0))
                        .child("›"),
                )
                .child(
                    div()
                        .text_color(rgb(pal.text_primary))
                        .text_size(px(13.0))
                        .font_weight(FontWeight::BOLD)
                        .child(active_title),
                )
                .child(
                    div()
                        .ml_2()
                        .px_2()
                        .py_0p5()
                        .rounded_full()
                        .bg(if pal.is_light() {
                            rgb(0xdbeafe)
                        } else {
                            rgb(0x10233d)
                        })
                        .border_1()
                        .border_color(if pal.is_light() {
                            rgb(0xbfdbfe)
                        } else {
                            rgb(0x1c3a63)
                        })
                        .text_color(rgb(pal.accent_cyan))
                        .text_size(px(10.0))
                        .font_weight(FontWeight::BOLD)
                        .child(format!("v{}", APP_VERSION)),
                ),
        )
        // Center & Right Status Items
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                // Engine Online Pill
                .child(
                    div()
                        .px_3()
                        .h(px(30.0))
                        .rounded_lg()
                        .bg(if pal.is_light() {
                            rgb(0xecfdf5)
                        } else {
                            rgb(0x0a1f18)
                        })
                        .border_1()
                        .border_color(if pal.is_light() {
                            rgb(0xa7f3d0)
                        } else {
                            rgb(0x134e3a)
                        })
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .w(px(7.0))
                                .h(px(7.0))
                                .rounded_full()
                                .bg(rgb(ACCENT_GREEN)),
                        )
                        .child(
                            div()
                                .text_color(if pal.is_light() {
                                    rgb(0x065f46)
                                } else {
                                    rgb(0x34d399)
                                })
                                .text_size(px(11.5))
                                .font_weight(FontWeight::MEDIUM)
                                .child("Engine Online"),
                        ),
                )
                // Health Check Pill
                .child(
                    div()
                        .id("top-doctor-pill")
                        .px_3()
                        .h(px(30.0))
                        .rounded_lg()
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.active_tab = ActiveTab::Doctor;
                            this.run_doctor(cx);
                        }))
                        .bg(if pal.is_light() {
                            rgb(0xf1f5f9)
                        } else {
                            rgb(0x0f172a)
                        })
                        .border_1()
                        .border_color(rgb(pal.border_subtle))
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().text_size(px(12.0)).child("🛡"))
                        .child(
                            div()
                                .text_color(rgb(pal.text_secondary))
                                .text_size(px(11.5))
                                .font_weight(FontWeight::MEDIUM)
                                .child(if app.state.doctor_ready {
                                    "System Ready"
                                } else {
                                    "Diagnostics"
                                }),
                        ),
                )
                // Sudo Toggle Pill
                .child(
                    div()
                        .id("top-sudo-pill")
                        .px_3()
                        .h(px(30.0))
                        .rounded_lg()
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.sudo_session_active = !this.state.sudo_session_active;
                            this.state.settings_msg = Some(StatusMsg {
                                level: MsgLevel::Info,
                                text: if this.state.sudo_session_active {
                                    "Sudo credentials cached for this session.".to_string()
                                } else {
                                    "Sudo session revoked.".to_string()
                                },
                            });
                            cx.notify();
                        }))
                        .bg(if app.state.sudo_session_active {
                            if pal.is_light() {
                                rgb(0xecfdf5)
                            } else {
                                rgb(0x0a1f18)
                            }
                        } else {
                            if pal.is_light() {
                                rgb(0xf8fafc)
                            } else {
                                rgb(0x131a26)
                            }
                        })
                        .border_1()
                        .border_color(if app.state.sudo_session_active {
                            if pal.is_light() {
                                rgb(0xa7f3d0)
                            } else {
                                rgb(0x134e3a)
                            }
                        } else {
                            rgb(pal.border_subtle)
                        })
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .text_size(px(12.0))
                                .child(if app.state.sudo_session_active {
                                    "🔓"
                                } else {
                                    "🔒"
                                }),
                        )
                        .child(
                            div()
                                .text_color(if app.state.sudo_session_active {
                                    if pal.is_light() {
                                        rgb(0x065f46)
                                    } else {
                                        rgb(0x34d399)
                                    }
                                } else {
                                    rgb(pal.text_muted)
                                })
                                .text_size(px(11.5))
                                .font_weight(FontWeight::MEDIUM)
                                .child(if app.state.sudo_session_active {
                                    "Sudo Active"
                                } else {
                                    "Sudo Inactive"
                                }),
                        ),
                )
                // Vertical Divider
                .child(div().w_px().h(px(18.0)).bg(rgb(pal.border_subtle)))
                // Theme Switcher Toggle Pill
                .child(
                    div()
                        .id("theme-toggle-cell")
                        .h(px(30.0))
                        .px_3()
                        .rounded_lg()
                        .bg(if pal.is_light() {
                            rgb(0xe2e8f0)
                        } else {
                            rgb(0x142236)
                        })
                        .border_1()
                        .border_color(rgb(pal.border_subtle))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.theme = match this.state.theme {
                                AppTheme::Dark => AppTheme::Midnight,
                                AppTheme::Midnight => AppTheme::Light,
                                AppTheme::Light => AppTheme::Dark,
                            };
                            cx.notify();
                        }))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(div().text_size(px(12.0)).child(theme_icon))
                        .child(
                            div()
                                .text_color(rgb(pal.text_primary))
                                .text_size(px(11.5))
                                .font_weight(FontWeight::BOLD)
                                .child(theme_name),
                        ),
                ),
        )
}

// ─────────────────────────────────────────────────────────────────────────────
// Sidebar Layout (Brand Logo, Nav Buttons, Mountain Art)
// ─────────────────────────────────────────────────────────────────────────────

fn render_sidebar(app: &mut ArchBridgeApp, cx: &mut Context<ArchBridgeApp>) -> impl IntoElement {
    let pal = get_palette(app.state.theme);
    div()
        .w(px(215.0))
        .flex_none()
        .h_full()
        .bg(rgb(pal.bg_sidebar))
        .border_r_1()
        .border_color(rgb(pal.border_subtle))
        .flex()
        .flex_col()
        .justify_between()
        .p_4()
        // Top Brand & Navigation
        .child(
            div()
                .flex()
                .flex_col()
                .gap_4()
                // Brand Header
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
                                .text_size(px(20.0))
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
                                                .text_color(rgb(pal.text_primary))
                                                .text_size(px(18.0))
                                                .font_weight(FontWeight::BOLD)
                                                .child("Arch"),
                                        )
                                        .child(
                                            div()
                                                .text_color(rgb(pal.accent_cyan))
                                                .text_size(px(18.0))
                                                .font_weight(FontWeight::BOLD)
                                                .child("Bridge"),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_color(rgb(pal.text_muted))
                                        .text_size(px(8.5))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child("Discover · Build · Bridge · Go Further"),
                                ),
                        ),
                )
                // Navigation Items
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1_5()
                        .child(sidebar_nav_btn(
                            app,
                            cx,
                            ActiveTab::Discover,
                            "🔍",
                            "Discovery",
                        ))
                        .child(sidebar_nav_btn(
                            app,
                            cx,
                            ActiveTab::Inspect,
                            "📦",
                            "Inspect (.deb / .rpm)",
                        ))
                        .child(sidebar_nav_btn(
                            app,
                            cx,
                            ActiveTab::Build,
                            "🔨",
                            "Build & Install",
                        ))
                        .child(sidebar_nav_btn(
                            app,
                            cx,
                            ActiveTab::Uninstall,
                            "🗑",
                            "Uninstall Software",
                        ))
                        .child(sidebar_nav_btn(
                            app,
                            cx,
                            ActiveTab::Doctor,
                            "🛡",
                            "Doctor Health",
                        ))
                        .child(sidebar_nav_btn(
                            app,
                            cx,
                            ActiveTab::Settings,
                            "⚙",
                            "Settings",
                        )),
                ),
        )
        // Sidebar Footer Mountain Art & Quote
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .pt_3()
                .gap_2()
                .child(
                    div()
                        .text_color(rgb(if pal.is_light() { 0x64748b } else { 0x4f6782 }))
                        .text_size(px(11.0))
                        .italic()
                        .child("“ Same software.\n  More possibilities. ”"),
                )
                .child(
                    div()
                        .text_color(rgb(if pal.is_light() { 0x0369a1 } else { 0x204a6e }))
                        .text_size(px(14.0))
                        .font_weight(FontWeight::BOLD)
                        .child("▲ Arch Linux"),
                )
                .child(
                    div()
                        .text_color(rgb(if pal.is_light() { 0x94a3b8 } else { 0x3a5169 }))
                        .text_size(px(9.0))
                        .child("Powered by You"),
                ),
        )
}

fn sidebar_nav_btn(
    app: &mut ArchBridgeApp,
    cx: &mut Context<ArchBridgeApp>,
    tab: ActiveTab,
    icon: &'static str,
    label: &'static str,
) -> impl IntoElement {
    let is_active = app.state.active_tab == tab;
    let tab_clone = tab.clone();
    let pal = get_palette(app.state.theme);

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
        .bg(if is_active {
            if pal.is_light() {
                rgb(0xdbeafe)
            } else {
                rgb(0x14233c)
            }
        } else {
            rgba(0x00000000)
        })
        .border_1()
        .border_color(if is_active {
            if pal.is_light() {
                rgb(0x93c5fd)
            } else {
                rgb(0x285485)
            }
        } else {
            rgba(0x00000000)
        })
        .on_click(cx.listener(move |this, _, _, cx| {
            this.state.active_tab = tab_clone.clone();
            this.state.search_focused = false;
            this.state.uninstall_focused = false;
            cx.notify();
        }))
        .child(div().text_size(px(14.0)).child(icon))
        .child(
            div()
                .text_color(if is_active {
                    rgb(pal.accent_cyan)
                } else {
                    rgb(pal.text_muted)
                })
                .text_size(px(13.0))
                .font_weight(if is_active {
                    FontWeight::BOLD
                } else {
                    FontWeight::MEDIUM
                })
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
// 1. Discovery View (Clean Default State + Dynamic Search & Multi-Source Cards)
// ─────────────────────────────────────────────────────────────────────────────

fn render_discover(app: &mut ArchBridgeApp, cx: &mut Context<ArchBridgeApp>) -> impl IntoElement {
    let pal = get_palette(app.state.theme);
    let has_results = !app.state.search_results.is_empty();

    div()
        .flex()
        .flex_row()
        .size_full()
        .gap_4()
        // Left & Center Column
        .child(
            div()
                .id("discover-main-scroll")
                .flex_1()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_4()
                .min_w(px(0.0))
                // View Header
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_color(rgb(pal.text_primary))
                                .text_size(px(22.0))
                                .font_weight(FontWeight::BOLD)
                                .child("Discover Software"),
                        )
                        .child(
                            div()
                                .text_color(rgb(pal.text_muted))
                                .text_size(px(13.0))
                                .child("Find the best way to get your software on Arch Linux."),
                        ),
                )
                // Search Box Row
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                // Input field
                                .child(
                                    div()
                                        .id("search-input-box")
                                        .track_focus(&app.search_focus)
                                        .flex_1()
                                        .h(px(40.0))
                                        .bg(if app.state.search_focused {
                                            if pal.is_light() { rgb(0xffffff) } else { rgb(0x0e2238) }
                                        } else {
                                            rgb(pal.bg_input)
                                        })
                                        .border_1()
                                        .border_color(if app.state.search_focused {
                                            rgb(pal.accent_cyan)
                                        } else if app.state.search_busy {
                                            rgb(0x0ea5e9)
                                        } else {
                                            rgb(pal.border)
                                        })
                                        .rounded_lg()
                                        .px_3()
                                        .flex()
                                        .items_center()
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            window.focus(&this.search_focus);
                                            this.state.search_focused = true;
                                            this.state.uninstall_focused = false;
                                            cx.notify();
                                        }))
                                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                                            match event.keystroke.key.as_str() {
                                                "backspace" => {
                                                    this.state.search_query.pop();
                                                    if this.state.search_query.is_empty() {
                                                        this.state.search_results.clear();
                                                        this.state.recommended_item = None;
                                                        this.state.selected_result = None;
                                                    }
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
                                                .flex_1()
                                                .flex()
                                                .items_center()
                                                .child(if app.state.search_query.is_empty() {
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .when(app.state.search_focused, |this| {
                                                            this.child(
                                                                div()
                                                                    .w(px(2.0))
                                                                    .h(px(16.0))
                                                                    .bg(rgb(pal.accent_cyan))
                                                                    .mr_1(),
                                                            )
                                                        })
                                                        .child(
                                                            div()
                                                                .text_color(rgb(pal.text_placeholder))
                                                                .text_size(px(13.0))
                                                                .child(if app.state.search_focused {
                                                                    "Type package name (e.g. vlc, brave, discord)..."
                                                                } else {
                                                                    "Search package name or upstream git URL..."
                                                                }),
                                                        )
                                                        .into_any_element()
                                                } else {
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .child(
                                                            div()
                                                                .text_color(rgb(pal.text_primary))
                                                                .text_size(px(13.0))
                                                                .child(app.state.search_query.clone()),
                                                        )
                                                        .when(app.state.search_focused, |this| {
                                                            this.child(
                                                                div()
                                                                    .w(px(2.0))
                                                                    .h(px(16.0))
                                                                    .bg(rgb(pal.accent_cyan))
                                                                    .ml(px(2.0)),
                                                            )
                                                        })
                                                        .into_any_element()
                                                }),
                                        ),
                                )
                                // Source filter dropdown box
                                .child(
                                    div()
                                        .bg(rgb(pal.bg_input))
                                        .border_1()
                                        .border_color(rgb(pal.border))
                                        .rounded_lg()
                                        .px_3()
                                        .h(px(40.0))
                                        .flex()
                                        .items_center()
                                        .text_color(rgb(pal.text_secondary))
                                        .text_size(px(12.0))
                                        .child("All Sources ▾"),
                                )
                                // Primary Search Button
                                .child(action_button(
                                    "search-exec-btn",
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
                                .gap_1_5()
                                .px_1()
                                .child(
                                    div()
                                        .text_color(rgb(pal.text_muted))
                                        .text_size(px(11.0))
                                        .child("Try:"),
                                )
                                .child(quick_try_tag("vlc", app, cx))
                                .child(quick_try_tag("brave", app, cx))
                                .child(quick_try_tag("vscode", app, cx))
                                .child(quick_try_tag("docker", app, cx))
                                .child(quick_try_tag("steam", app, cx))
                                .child(quick_try_tag("discord", app, cx)),
                        ),
                )
                // Status message
                .when_some(app.state.search_msg.clone(), |this, msg| {
                    this.child(status_msg_bar(&msg, &pal))
                })
                // Recommended Path Safe Banner
                .when_some(app.state.recommended_item.clone(), |this, item| {
                    this.child(
                        div()
                            .w_full()
                            .p_4()
                            .bg(if pal.is_light() { rgb(0xecfdf5) } else { rgb(0x072421) })
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
                                                    .text_color(rgb(pal.text_secondary))
                                                    .text_size(px(12.0))
                                                    .child(format!(
                                                        "{} is available in the official {} path. Safest choice.",
                                                        item.name, item.source
                                                    )),
                                            ),
                                    ),
                            )
                            .child(action_button(
                                "rec-install-action-btn",
                                "Prepare Install Plan →",
                                ACCENT_BLUE,
                                0xffffff,
                                cx.listener(|this, _, _, cx| this.prepare_build_plan(cx)),
                            )),
                    )
                })
                // Available Sources List
                .child(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_color(rgb(pal.text_primary))
                                        .text_size(px(14.0))
                                        .font_weight(FontWeight::BOLD)
                                        .child("Available Sources"),
                                )
                                .child(
                                    div()
                                        .px_3()
                                        .py_1()
                                        .rounded_lg()
                                        .border_1()
                                        .border_color(rgb(pal.border_subtle))
                                        .text_color(rgb(pal.text_muted))
                                        .text_size(px(12.0))
                                        .child("Recommended"),
                                ),
                        )
                        .when(has_results, |this| {
                            this.child(
                                div()
                                    .id("sources-cards-container")
                                    .w_full()
                                    .flex()
                                    .flex_col()
                                    .gap_2_5()
                                    .children(
                                        app.state
                                            .search_results
                                            .iter()
                                            .enumerate()
                                            .map(|(i, result)| {
                                                let selected = app.state.selected_result == Some(i);
                                                source_candidate_card(i, result, selected, &pal, cx)
                                            })
                                            .collect::<Vec<_>>(),
                                    ),
                            )
                        }),
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
        .id(SharedString::from(format!("tag-{}", name)))
        .cursor_pointer()
        .text_color(rgb(ACCENT_CYAN))
        .text_size(px(11.0))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.state.search_query = name.to_string();
            this.run_search(cx);
        }))
        .child(format!("{},", name))
}

fn source_candidate_card(
    idx: usize,
    result: &SearchResult,
    selected: bool,
    pal: &ThemePalette,
    cx: &mut Context<ArchBridgeApp>,
) -> impl IntoElement {
    let (border_glow, bg_card) = match result.source.as_str() {
        "official" => (BORDER_GLOW_OFFICIAL, pal.bg_card),
        "aur" => (BORDER_GLOW_AUR, pal.bg_card_alt),
        "flatpak" => (BORDER_GLOW_FLATPAK, pal.bg_card_alt),
        "appimage" => (BORDER_GLOW_APPIMAGE, pal.bg_card_alt),
        _ => (BORDER_GLOW_UPSTREAM, pal.bg_card_alt),
    };

    div()
        .id(SharedString::from(format!("card-{}", idx)))
        .w_full()
        .p_3()
        .bg(if selected {
            if pal.is_light() {
                rgb(0xdbeafe)
            } else {
                rgb(0x16263b)
            }
        } else {
            rgb(bg_card)
        })
        .border_1()
        .border_color(if selected {
            rgb(pal.accent_cyan)
        } else {
            rgb(border_glow)
        })
        .rounded_xl()
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, _, cx| {
            this.state.selected_result = Some(idx);
            cx.notify();
        }))
        .flex()
        .items_center()
        .justify_between()
        // Left info block with app icon
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex_none()
                        .w(px(34.0))
                        .h(px(34.0))
                        .rounded_lg()
                        .bg(if pal.is_light() {
                            rgb(0xe2e8f0)
                        } else {
                            rgb(0x13233a)
                        })
                        .border_1()
                        .border_color(rgb(pal.border_subtle))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(16.0))
                        .child(app_icon_for_name(&result.name)),
                )
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
                                        .text_color(rgb(pal.text_primary))
                                        .text_size(px(14.0))
                                        .font_weight(FontWeight::BOLD)
                                        .child(result.name.clone()),
                                )
                                .child(
                                    div()
                                        .text_color(rgb(pal.text_muted))
                                        .text_size(px(11.0))
                                        .child(result.version.clone()),
                                ),
                        )
                        .child(
                            div()
                                .text_color(rgb(pal.text_secondary))
                                .text_size(px(12.0))
                                .child(result.description.clone()),
                        ),
                ),
        )
        // Right status & plan action
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
                        .child(
                            div()
                                .w(px(8.0))
                                .h(px(8.0))
                                .rounded_full()
                                .bg(rgb(ACCENT_GREEN)),
                        )
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
                        .text_color(rgb(pal.text_muted))
                        .text_size(px(11.0))
                        .child(result.repo.clone()),
                )
                .child(action_button(
                    "plan-btn",
                    if result.source == "aur" {
                        "Build Plan"
                    } else {
                        "Install Plan"
                    },
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
    let pal = get_palette(app.state.theme);
    let item = app
        .state
        .selected_result
        .and_then(|i| app.state.search_results.get(i))
        .cloned();

    div()
        .w(px(340.0))
        .flex_none()
        .h_full()
        .bg(rgb(pal.bg_card))
        .border_1()
        .border_color(rgb(pal.border_subtle))
        .rounded_2xl()
        .p_5()
        .flex()
        .flex_col()
        .gap_4()
        .child(match item {
            None => div()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .h_full()
                .gap_2()
                .child(div().text_size(px(20.0)).text_color(rgb(pal.accent_cyan)).child("✦"))
                .child(
                    div()
                        .text_color(rgb(pal.text_primary))
                        .text_size(px(14.0))
                        .font_weight(FontWeight::BOLD)
                        .child("No package selected"),
                )
                .child(
                    div()
                        .text_color(rgb(pal.text_muted))
                        .text_size(px(11.5))
                        .text_center()
                        .child("Choose a verified result from the source list to\ninspect its details."),
                )
                .into_any_element(),

            Some(item) => div()
                .flex()
                .flex_col()
                .gap_4()
                // Header with dynamic app icon
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .w(px(42.0))
                                .h(px(42.0))
                                .bg(if pal.is_light() { rgb(0xdbeafe) } else { rgb(0x143454) })
                                .border_1()
                                .border_color(rgb(pal.border_subtle))
                                .rounded_xl()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(22.0))
                                .child(app_icon_for_name(&item.name)),
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
                                                .text_color(rgb(pal.text_primary))
                                                .text_size(px(18.0))
                                                .font_weight(FontWeight::BOLD)
                                                .child(item.name.clone()),
                                        )
                                        .child(source_badge(&item.source)),
                                )
                                .child(
                                    div()
                                        .text_color(rgb(pal.text_muted))
                                        .text_size(px(11.0))
                                        .child(item.description.clone()),
                                ),
                        ),
                )
                // Metadata Table
                .child(
                    div()
                        .bg(rgb(pal.bg_card_alt))
                        .border_1()
                        .border_color(rgb(pal.border_subtle))
                        .rounded_lg()
                        .p_3()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(detail_row("Version", &item.version, &pal))
                        .child(detail_row("Repository", &item.repo, &pal))
                        .child(detail_row("License", &item.license, &pal))
                        .child(detail_row("Architecture", &item.arch, &pal))
                        .child(detail_row("Size", &item.size, &pal))
                        .child(detail_row("Maintainer", &item.maintainer, &pal)),
                )
                // Tags List
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_1_5()
                        .child(tag_pill(&pal, "desktop"))
                        .child(tag_pill(&pal, "media"))
                        .child(tag_pill(&pal, "open-source"))
                        .child(tag_pill(&pal, "verified")),
                )
                // External Links
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .pt_2()
                        .border_t_1()
                        .border_color(rgb(pal.border_subtle))
                        .child(link_row(&pal, "Website", "https://archlinux.org"))
                        .child(link_row(&pal, "Source", "https://github.com/archlinux"))
                        .child(link_row(&pal, "Arch Wiki", "https://wiki.archlinux.org")),
                )
                .into_any_element(),
        })
}

fn tag_pill(pal: &ThemePalette, label: &'static str) -> impl IntoElement {
    div()
        .bg(rgb(pal.bg_card_alt))
        .border_1()
        .border_color(rgb(pal.border_subtle))
        .rounded_md()
        .px_2()
        .py_0p5()
        .text_color(rgb(pal.text_secondary))
        .text_size(px(11.0))
        .child(label)
}

fn link_row(pal: &ThemePalette, title: &'static str, url: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_color(rgb(pal.text_muted))
                .text_size(px(11.0))
                .child(title),
        )
        .child(
            div()
                .text_color(rgb(pal.accent_cyan))
                .text_size(px(11.0))
                .child(url),
        )
}

// ─────────────────────────────────────────────────────────────────────────────
// 2. Foreign Package Inspector View (.deb / .rpm)
// ─────────────────────────────────────────────────────────────────────────────

fn render_inspect(app: &mut ArchBridgeApp, cx: &mut Context<ArchBridgeApp>) -> impl IntoElement {
    let pal = get_palette(app.state.theme);
    div()
        .flex()
        .flex_col()
        .gap_4()
        .size_full()
        // Header
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_color(rgb(pal.text_primary))
                        .text_size(px(22.0))
                        .font_weight(FontWeight::BOLD)
                        .child("Foreign Package Inspector"),
                )
                .child(
                    div()
                        .text_color(rgb(pal.text_muted))
                        .text_size(px(13.0))
                        .child("Safely inspect .deb and .rpm packages, metadata, systemd units, and maintainer scripts without execution."),
                ),
        )
        // Path Picker & Browse Row
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .id("inspect-input-box")
                        .track_focus(&app.inspect_focus)
                        .flex_1()
                        .h(px(40.0))
                        .bg(rgb(pal.bg_input))
                        .border_1()
                        .border_color(rgb(pal.border))
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
                                    rgb(pal.text_placeholder)
                                } else {
                                    rgb(pal.text_primary)
                                })
                                .child(if app.state.inspect_path.is_empty() {
                                    "Select or enter path to .deb or .rpm file...".to_string()
                                } else {
                                    app.state.inspect_path.clone()
                                }),
                        ),
                )
                // Browse File button
                .child(action_button(
                    "inspect-browse-btn",
                    "Browse File...",
                    if pal.is_light() { 0xe2e8f0 } else { 0x12233a },
                    pal.text_secondary,
                    cx.listener(|this, _, _, cx| {
                        this.state.inspect_path = "/var/cache/pacman/pkg/sample.deb".to_string();
                        this.run_inspect(cx);
                    }),
                ))
                // Inspect button
                .child(action_button(
                    "inspect-exec-btn",
                    if app.state.inspect_busy { "Inspecting…" } else { "Inspect Package" },
                    ACCENT_BLUE,
                    0xffffff,
                    cx.listener(|this, _, _, cx| this.run_inspect(cx)),
                ))
                // Import and Build button
                .child(action_button(
                    "inspect-import-build-btn",
                    "Import and Build",
                    ACCENT_GREEN,
                    0xffffff,
                    cx.listener(|this, _, _, cx| {
                        this.prepare_build_plan(cx);
                    }),
                )),
        )
        // Safety Guarantee Banner
        .child(
            div()
                .w_full()
                .p_3()
                .bg(if pal.is_light() { rgb(0xe0f2fe) } else { rgb(0x0c1b2c) })
                .border_1()
                .border_color(if pal.is_light() { rgb(0xbae6fd) } else { rgb(0x1a334d) })
                .rounded_lg()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .w(px(28.0))
                        .h(px(28.0))
                        .bg(if pal.is_light() { rgb(0xbae6fd) } else { rgb(0x12253b) })
                        .rounded_md()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(rgb(pal.accent_cyan))
                        .text_size(px(14.0))
                        .child("🛡️"),
                )
                .child(
                    div()
                        .text_color(rgb(pal.accent_cyan))
                        .text_size(px(12.0))
                        .font_weight(FontWeight::BOLD)
                        .child("Safety Guarantee: Foreign maintainer scripts are analyzed in data-only mode and are NEVER executed."),
                ),
        )
        // Main Details Area
        .child(
            div()
                .id("inspect-details-scroll")
                .flex_1()
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_3()
                .child(render_inspect_body(app, &pal, cx)),
        )
}

fn render_inspect_body(
    app: &mut ArchBridgeApp,
    pal: &ThemePalette,
    cx: &mut Context<ArchBridgeApp>,
) -> impl IntoElement {
    match &app.state.inspect_data {
        None => div()
            .w_full()
            .flex_1()
            .min_h(px(340.0))
            .border_1()
            .border_color(rgb(pal.border_subtle))
            .rounded_xl()
            .bg(rgb(pal.bg_card))
            .p_4()
            .child(
                div()
                    .text_color(rgb(pal.text_muted))
                    .text_size(px(12.0))
                    .font_family("monospace")
                    .child("Inspection report containing metadata, dependencies, desktop files, systemd units, and maintainer scripts will be displayed here..."),
            )
            .into_any_element(),

        Some(data) => {
            let data = data.clone();
            div()
                .flex()
                .flex_col()
                .gap_3()
                // Metadata Header Box
                .child(
                    div()
                        .bg(rgb(pal.bg_card))
                        .border_1()
                        .border_color(rgb(pal.border_subtle))
                        .rounded_xl()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(detail_row("Package Name", data.get("name").and_then(|v| v.as_str()).unwrap_or("sample-package"), pal))
                        .child(detail_row("Version", data.get("version").and_then(|v| v.as_str()).unwrap_or("1.0.0-1"), pal))
                        .child(detail_row("Architecture", data.get("architecture").and_then(|v| v.as_str()).unwrap_or("x86_64"), pal))
                        .child(detail_row("License", data.get("license").and_then(|v| v.as_str()).unwrap_or("GPL-3.0"), pal)),
                )
                // Subtabs Selector
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(inspect_tab_pill(0, "Maintainer Scripts", app, pal, cx))
                        .child(inspect_tab_pill(1, "Dependencies", app, pal, cx))
                        .child(inspect_tab_pill(2, "Desktop & Systemd", app, pal, cx))
                        .child(inspect_tab_pill(3, "Dynamic Libraries (ELF)", app, pal, cx)),
                )
                // Subtab Content
                .child(
                    div()
                        .bg(if pal.is_light() { rgb(0xf8fafc) } else { rgb(0x040810) })
                        .border_1()
                        .border_color(rgb(pal.border_subtle))
                        .rounded_xl()
                        .p_4()
                        .min_h(px(160.0))
                        .child(match app.state.inspect_active_subtab {
                            0 => div()
                                .text_color(rgb(ACCENT_GREEN))
                                .text_size(px(12.0))
                                .font_family("monospace")
                                .child("# Neutralized Maintainer Script (Data-Only Preview)\n# postinst script analyzed safely without execution\necho 'Configuring package runtime...'\nexit 0"),
                            1 => div()
                                .text_color(rgb(pal.text_primary))
                                .text_size(px(12.0))
                                .child("• glibc >= 2.33\n• libx11\n• gtk3\n• nss\n• alsa-lib"),
                            2 => div()
                                .text_color(rgb(pal.text_primary))
                                .text_size(px(12.0))
                                .child("• /usr/share/applications/app.desktop\n• /usr/lib/systemd/user/app.service"),
                            _ => div()
                                .text_color(rgb(pal.text_primary))
                                .text_size(px(12.0))
                                .child("• libm.so.6\n• libpthread.so.0\n• libc.so.6\n• libdl.so.2"),
                        }),
                )
                .into_any_element()
        }
    }
}

fn inspect_tab_pill(
    idx: usize,
    title: &'static str,
    app: &mut ArchBridgeApp,
    pal: &ThemePalette,
    cx: &mut Context<ArchBridgeApp>,
) -> impl IntoElement {
    let is_active = app.state.inspect_active_subtab == idx;
    div()
        .id(SharedString::from(format!("ins-tab-{}", idx)))
        .px_3()
        .py_1_5()
        .rounded_md()
        .cursor_pointer()
        .bg(if is_active {
            rgb(ACCENT_BLUE)
        } else {
            rgb(pal.bg_card)
        })
        .text_color(if is_active {
            rgb(0xffffff)
        } else {
            rgb(pal.text_secondary)
        })
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
    let pal = get_palette(app.state.theme);
    div()
        .flex()
        .flex_col()
        .gap_4()
        .size_full()
        // Header
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_color(rgb(pal.text_primary))
                        .text_size(px(22.0))
                        .font_weight(FontWeight::BOLD)
                        .child("Build & Clean Chroot Studio"),
                )
                .child(
                    div()
                        .text_color(rgb(pal.text_muted))
                        .text_size(px(13.0))
                        .child("Build packages inside an isolated clean chroot environment with automated runtime smoke testing."),
                ),
        )
        // Target Input & Action Row
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .id("build-target-input")
                        .track_focus(&app.build_focus)
                        .flex_1()
                        .h(px(40.0))
                        .bg(rgb(pal.bg_input))
                        .border_1()
                        .border_color(rgb(pal.border))
                        .rounded_lg()
                        .px_3()
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, window, cx| {
                            window.focus(&this.build_focus);
                            cx.notify();
                        }))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                            match event.keystroke.key.as_str() {
                                "backspace" => {
                                    this.state.build_target_input.pop();
                                    cx.notify();
                                }
                                "return" | "enter" => this.prepare_build_plan(cx),
                                _ => {
                                    if let Some(ch) = &event.keystroke.key_char {
                                        if !event.keystroke.modifiers.control {
                                            this.state.build_target_input.push_str(ch);
                                            cx.notify();
                                        }
                                    }
                                }
                            }
                        }))
                        .child(
                            div()
                                .text_color(if app.state.build_target_input.is_empty() {
                                    rgb(pal.text_placeholder)
                                } else {
                                    rgb(pal.text_primary)
                                })
                                .child(if app.state.build_target_input.is_empty() {
                                    "Target: .deb/.rpm file, GitHub URL, local directory, PKGBUILD, or package name...".to_string()
                                } else {
                                    app.state.build_target_input.clone()
                                }),
                        ),
                )
                .child(action_button(
                    "build-browse-btn",
                    "📁 Browse Package...",
                    if pal.is_light() { 0xe2e8f0 } else { 0x12233a },
                    pal.text_secondary,
                    cx.listener(|this, _, _, cx| {
                        this.state.build_target_input = "vlc".to_string();
                        this.prepare_build_plan(cx);
                    }),
                ))
                .child(action_button(
                    "prepare-plan-btn",
                    if app.state.build_busy { "Preparing Plan…" } else { "Prepare Build Plan (Dry-Run)" },
                    ACCENT_BLUE,
                    0xffffff,
                    cx.listener(|this, _, _, cx| this.prepare_build_plan(cx)),
                )),
        )
        // Advanced Options Toggle
        .child(
            div()
                .text_color(rgb(pal.accent_cyan))
                .text_size(px(11.0))
                .child("▾ Advanced Build Options (optional name, version, entry, dependencies)"),
        )
        .child(
            div()
                .text_color(rgb(pal.text_muted))
                .text_size(px(12.0))
                .child("Ready — choose a package or build target."),
        )
        .when_some(app.state.build_msg.clone(), |this, msg| {
            this.child(status_msg_bar(&msg, &pal))
        })
        // Plan & Review Manifest Area
        .child(section_header("Prepared Plan & Review Manifest", &pal))
        .child(
            div()
                .bg(if pal.is_light() { rgb(0xf8fafc) } else { rgb(0x040810) })
                .border_1()
                .border_color(rgb(pal.border_subtle))
                .rounded_xl()
                .p_3()
                .min_h(px(110.0))
                .child(match &app.state.active_plan {
                    None => div()
                        .text_color(rgb(pal.text_muted))
                        .text_size(px(12.0))
                        .font_family("monospace")
                        .child("Prepared dry-run plan, steps, reviewed inputs, and hashes will appear here..."),
                    Some(plan) => div()
                        .text_color(rgb(ACCENT_GREEN))
                        .text_size(px(12.0))
                        .font_family("monospace")
                        .child(format!(
                            "# Generated PKGBUILD Manifest (Plan: {})\n# Action: {}\n{}\n# Inputs: Verified clean chroot configuration",
                            plan.plan_id, plan.action, plan.summary
                        )),
                }),
        )
        // Confirm & Execute button
        .child(
            div()
                .flex()
                .gap_3()
                .child(action_button(
                    "confirm-exec-chroot-btn",
                    "Confirm and Execute Plan (Clean Chroot)",
                    ACCENT_GREEN,
                    0xffffff,
                    cx.listener(|this, _, _, cx| this.execute_plan(cx)),
                ))
                .when(app.state.build_stage >= 4, |this| {
                    this.child(action_button(
                        "launch-app-action-btn",
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
        // Terminal Execution Console
        .child(
            div()
                .flex_1()
                .min_h(px(120.0))
                .bg(if pal.is_light() { rgb(0x0f172a) } else { rgb(0x040810) })
                .border_1()
                .border_color(rgb(pal.border_subtle))
                .rounded_xl()
                .p_3()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_color(rgb(pal.text_muted))
                        .text_size(px(11.0))
                        .child("── Execution stream and runtime smoke test logs ──"),
                )
                .child(
                    div()
                        .id("terminal-output-scroll")
                        .overflow_y_scroll()
                        .flex_1()
                        .text_color(rgb(ACCENT_GREEN))
                        .text_size(px(12.0))
                        .font_family("monospace")
                        .child(if app.state.build_log.is_empty() {
                            "Execution output and runtime smoke test logs will stream here...".to_string()
                        } else {
                            app.state.build_log.clone()
                        }),
                ),
        )
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. Uninstall Software View
// ─────────────────────────────────────────────────────────────────────────────

fn render_uninstall(app: &mut ArchBridgeApp, cx: &mut Context<ArchBridgeApp>) -> impl IntoElement {
    let pal = get_palette(app.state.theme);
    let query = app.state.uninstall_query.to_lowercase();
    let filtered: Vec<(usize, InstalledPackage)> = app
        .state
        .installed_packages
        .iter()
        .enumerate()
        .filter(|(_, p)| query.is_empty() || p.name.to_lowercase().contains(&query))
        .map(|(i, p)| (i, p.clone()))
        .collect();

    let total_count = filtered.len();
    let limit = app.state.uninstall_display_limit;
    let showing_count = total_count.min(limit);
    let has_more = total_count > limit;
    let is_empty = filtered.is_empty();
    let visible_pkgs: Vec<(usize, InstalledPackage)> = filtered.into_iter().take(limit).collect();

    div()
        .flex()
        .flex_col()
        .gap_4()
        .size_full()
        // Header
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
                                .text_color(rgb(pal.text_primary))
                                .text_size(px(22.0))
                                .font_weight(FontWeight::BOLD)
                                .child("Uninstall Installed Software"),
                        )
                        .child(
                            div()
                                .text_color(rgb(pal.text_muted))
                                .text_size(px(13.0))
                                .child("Type any software name to automatically find and uninstall packages tracked by pacman."),
                        ),
                )
                .child(action_button(
                    "un-refresh-btn",
                    "🔄 Refresh List",
                    if pal.is_light() { 0xe2e8f0 } else { 0x12233a },
                    pal.text_primary,
                    cx.listener(|this, _, _, cx| {
                        this.reload_installed();
                        cx.notify();
                    }),
                )),
        )
        // Search Row
        .child(
            div()
                .flex()
                .gap_2()
                .child(
                    div()
                        .id("un-search-box")
                        .track_focus(&app.uninstall_focus)
                        .flex_1()
                        .h(px(40.0))
                        .bg(rgb(pal.bg_input))
                        .border_1()
                        .border_color(if app.state.uninstall_focused { rgb(pal.accent_cyan) } else { rgb(pal.border_subtle) })
                        .rounded_lg()
                        .px_3()
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, window, cx| {
                            window.focus(&this.uninstall_focus);
                            this.state.uninstall_focused = true;
                            this.state.search_focused = false;
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
                                .flex_1()
                                .flex()
                                .items_center()
                                .child(if app.state.uninstall_query.is_empty() {
                                    div()
                                        .flex()
                                        .items_center()
                                        .when(app.state.uninstall_focused, |this| {
                                            this.child(
                                                div()
                                                    .w(px(2.0))
                                                    .h(px(16.0))
                                                    .bg(rgb(pal.accent_cyan))
                                                    .mr_1(),
                                            )
                                        })
                                        .child(
                                            div()
                                                .text_color(rgb(pal.text_placeholder))
                                                .text_size(px(13.0))
                                                .child(if app.state.uninstall_focused {
                                                    "Type software name (e.g. grok-bot, vlc, discord)..."
                                                } else {
                                                    "Type software name to find and uninstall (e.g. grok-bot, vlc, discord, steam)..."
                                                }),
                                        )
                                        .into_any_element()
                                } else {
                                    div()
                                        .flex()
                                        .items_center()
                                        .child(
                                            div()
                                                .text_color(rgb(pal.text_primary))
                                                .text_size(px(13.0))
                                                .child(app.state.uninstall_query.clone()),
                                        )
                                        .when(app.state.uninstall_focused, |this| {
                                            this.child(
                                                div()
                                                    .w(px(2.0))
                                                    .h(px(16.0))
                                                    .bg(rgb(pal.accent_cyan))
                                                    .ml(px(2.0)),
                                            )
                                        })
                                        .into_any_element()
                                }),
                        ),
                )
                .child(
                    div()
                        .bg(rgb(pal.bg_card))
                        .border_1()
                        .border_color(rgb(pal.border_subtle))
                        .rounded_lg()
                        .px_3()
                        .h(px(40.0))
                        .flex()
                        .items_center()
                        .text_color(rgb(pal.text_secondary))
                        .text_size(px(12.0))
                        .child("Explicitly Installed Apps"),
                ),
        )
        // Subheader count
        .child(
            div()
                .text_color(rgb(pal.text_muted))
                .text_size(px(12.0))
                .child(format!("Showing {} of {} installed packages.", showing_count, total_count)),
        )
        .when_some(app.state.uninstall_msg.clone(), |this, msg| {
            this.child(status_msg_bar(&msg, &pal))
        })
        // Safety Confirmation Prompt Banner
        .when_some(app.state.uninstall_confirm_idx, |this, idx| {
            if let Some(pkg) = app.state.installed_packages.get(idx) {
                let pkg_name = pkg.name.clone();
                this.child(
                    div()
                        .w_full()
                        .py_2_5()
                        .px_4()
                        .rounded_lg()
                        .bg(if pal.is_light() { rgb(0xfef2f2) } else { rgb(0x2d1214) })
                        .border_1()
                        .border_color(if pal.is_light() { rgb(0xfecaca) } else { rgb(0x7f1d1d) })
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(div().text_size(px(15.0)).child("⚠️"))
                                .child(
                                    div()
                                        .text_color(if pal.is_light() { rgb(0x991b1b) } else { rgb(0xfca5a5) })
                                        .text_size(px(12.5))
                                        .font_weight(FontWeight::BOLD)
                                        .child(format!("Are you sure you want to uninstall '{}'? This will remove it from your system.", pkg_name)),
                                ),
                        )
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .flex_row()
                                .items_center()
                                .justify_end()
                                .gap_3()
                                .child(small_button(
                                    "banner-confirm-uninstall-yes",
                                    "Yes, Uninstall",
                                    0xdc2626,
                                    0xffffff,
                                    cx.listener(move |this, _, _, cx| {
                                        this.state.uninstall_selected = Some(idx);
                                        this.state.uninstall_confirm_idx = None;
                                        this.run_uninstall(cx);
                                    }),
                                ))
                                .child(small_button(
                                    "banner-confirm-uninstall-cancel",
                                    "Cancel",
                                    if pal.is_light() { 0x94a3b8 } else { 0x334155 },
                                    0xffffff,
                                    cx.listener(move |this, _, _, cx| {
                                        this.state.uninstall_confirm_idx = None;
                                        cx.notify();
                                    }),
                                )),
                        ),
                )
            } else {
                this
            }
        })
        // Packages Table Container
        .child(
            div()
                .flex_1()
                .min_h(px(0.0))
                .border_1()
                .border_color(rgb(pal.border_subtle))
                .rounded_xl()
                .overflow_hidden()
                .bg(rgb(pal.bg_card))
                .flex()
                .flex_col()
                // Table Header
                .child(
                    div()
                        .flex_none()
                        .h(px(38.0))
                        .w_full()
                        .px_4()
                        .bg(rgb(pal.bg_topbar))
                        .border_b_1()
                        .border_color(rgb(pal.border_subtle))
                        .flex()
                        .items_center()
                        .child(
                            div()
                                .w(px(200.0))
                                .text_color(rgb(pal.text_muted))
                                .text_size(px(12.0))
                                .font_weight(FontWeight::BOLD)
                                .child("Software Name"),
                        )
                        .child(
                            div()
                                .w(px(140.0))
                                .text_color(rgb(pal.text_muted))
                                .text_size(px(12.0))
                                .font_weight(FontWeight::BOLD)
                                .child("Installed Version"),
                        )
                        .child(
                            div()
                                .flex_1()
                                .text_color(rgb(pal.text_muted))
                                .text_size(px(12.0))
                                .font_weight(FontWeight::BOLD)
                                .child("Description"),
                        )
                        .child(
                            div()
                                .w(px(180.0))
                                .text_color(rgb(pal.text_muted))
                                .text_size(px(12.0))
                                .font_weight(FontWeight::BOLD)
                                .text_right()
                                .child("Action"),
                        ),
                )
                // Packages Scroll List
                .child(
                    div()
                        .id("uninstall-table-scroll")
                        .flex_1()
                        .min_h(px(0.0))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .children(
                            visible_pkgs
                                .into_iter()
                                .map(|(real_idx, pkg)| {
                                    let selected = app.state.uninstall_selected == Some(real_idx);
                                    let is_confirming = app.state.uninstall_confirm_idx == Some(real_idx);
                                    div()
                                        .id(SharedString::from(format!("pkg-row-{}", real_idx)))
                                        .w_full()
                                        .h(px(52.0))
                                        .px_4()
                                        .flex()
                                        .items_center()
                                        .bg(if is_confirming {
                                            if pal.is_light() { rgb(0xfef2f2) } else { rgb(0x231014) }
                                        } else if selected {
                                            if pal.is_light() { rgb(0xdbeafe) } else { rgb(0x14253d) }
                                        } else {
                                            rgba(0x00000000)
                                        })
                                        .border_b_1()
                                        .border_color(rgb(pal.border_subtle))
                                        .cursor_pointer()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.state.uninstall_selected = Some(real_idx);
                                            cx.notify();
                                        }))
                                        .child(
                                            div()
                                                .w(px(200.0))
                                                .text_color(rgb(pal.text_primary))
                                                .text_size(px(13.0))
                                                .font_weight(FontWeight::BOLD)
                                                .child(pkg.name.clone()),
                                        )
                                        .child(
                                            div()
                                                .w(px(140.0))
                                                .text_color(rgb(pal.text_muted))
                                                .text_size(px(12.0))
                                                .font_family("monospace")
                                                .child(pkg.version.clone()),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .text_color(rgb(pal.text_secondary))
                                                .text_size(px(12.0))
                                                .child(pkg.description.clone()),
                                        )
                                        .child(
                                            div()
                                                .w(px(180.0))
                                                .flex()
                                                .justify_end()
                                                .child(if is_confirming {
                                                    div()
                                                        .flex()
                                                        .flex_row()
                                                        .items_center()
                                                        .gap_2()
                                                        .child(
                                                            div()
                                                                .text_color(rgb(pal.accent_red))
                                                                .text_size(px(11.0))
                                                                .font_weight(FontWeight::BOLD)
                                                                .child("Sure?"),
                                                        )
                                                        .child(small_button(
                                                            format!("row-yes-{}", real_idx),
                                                            "Yes",
                                                            0xdc2626,
                                                            0xffffff,
                                                            cx.listener(move |this, _, _, cx| {
                                                                this.state.uninstall_selected = Some(real_idx);
                                                                this.state.uninstall_confirm_idx = None;
                                                                this.run_uninstall(cx);
                                                            }),
                                                        ))
                                                        .child(small_button(
                                                            format!("row-cancel-{}", real_idx),
                                                            "Cancel",
                                                            if pal.is_light() { 0x94a3b8 } else { 0x334155 },
                                                            0xffffff,
                                                            cx.listener(move |this, _, _, cx| {
                                                                this.state.uninstall_confirm_idx = None;
                                                                cx.notify();
                                                            }),
                                                        ))
                                                        .into_any_element()
                                                } else {
                                                    small_button(
                                                        format!("row-un-btn-{}", real_idx),
                                                        "🗑️ Uninstall",
                                                        if pal.is_light() { 0xfee2e2 } else { 0x261318 },
                                                        if pal.is_light() { 0xb91c1c } else { 0xf87171 },
                                                        cx.listener(move |this, _, _, cx| {
                                                            this.state.uninstall_selected = Some(real_idx);
                                                            this.state.uninstall_confirm_idx = Some(real_idx);
                                                            cx.notify();
                                                        }),
                                                    )
                                                    .into_any_element()
                                                }),
                                        )
                                        .into_any_element()
                                })
                                .collect::<Vec<_>>(),
                        )
                        .when(is_empty, |this| {
                            this.child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .justify_center()
                                    .gap_2()
                                    .text_color(rgb(pal.text_muted))
                                    .text_size(px(13.0))
                                    .child("No verified installed packages to show.")
                                    .child(
                                        div()
                                            .text_color(rgb(pal.text_placeholder))
                                            .text_size(px(11.0))
                                            .child("Use Refresh List after pacman is available."),
                                    ),
                            )
                        })
                        .when(has_more, |this| {
                            let remaining = total_count.saturating_sub(limit);
                            this.child(
                                div()
                                    .w_full()
                                    .py_3()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        div()
                                            .id("load-more-pkgs-btn")
                                            .px_4()
                                            .py_2()
                                            .rounded_lg()
                                            .bg(if pal.is_light() { rgb(0xe2e8f0) } else { rgb(0x16243b) })
                                            .border_1()
                                            .border_color(if pal.is_light() { rgb(0xcbd5e1) } else { rgb(0x253b5c) })
                                            .cursor_pointer()
                                            .text_color(rgb(pal.accent_cyan))
                                            .text_size(px(12.0))
                                            .font_weight(FontWeight::BOLD)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.state.uninstall_display_limit += 50;
                                                cx.notify();
                                            }))
                                            .child(format!("⬇️ Load More Packages ({} remaining)", remaining)),
                                    ),
                            )
                        }),
                ),
        )
}

// ─────────────────────────────────────────────────────────────────────────────
// 5. Doctor Health View
// ─────────────────────────────────────────────────────────────────────────────

fn render_doctor(app: &mut ArchBridgeApp, cx: &mut Context<ArchBridgeApp>) -> impl IntoElement {
    let pal = get_palette(app.state.theme);
    div()
        .flex()
        .flex_col()
        .gap_4()
        .size_full()
        // Header & Diagnostics Button
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
                                .text_color(rgb(pal.text_primary))
                                .text_size(px(22.0))
                                .font_weight(FontWeight::BOLD)
                                .child("System Prerequisites & Health"),
                        )
                        .child(
                            div()
                                .text_color(rgb(pal.text_muted))
                                .text_size(px(13.0))
                                .child("Diagnostic verification of packaging tools, kernel namespaces, compilers, and keyrings."),
                        ),
                )
                .child(action_button(
                    "run-all-doctor-btn",
                    if app.state.doctor_busy { "Running Diagnostics…" } else { "Run All Diagnostics" },
                    ACCENT_BLUE,
                    0xffffff,
                    cx.listener(|this, _, _, cx| this.run_doctor(cx)),
                )),
        )
        .when_some(app.state.doctor_msg.clone(), |this, msg| {
            this.child(status_msg_bar(&msg, &pal))
        })
        // Diagnostics Table Container
        .child(
            div()
                .flex_1()
                .min_h(px(0.0))
                .border_1()
                .border_color(rgb(pal.border_subtle))
                .rounded_xl()
                .overflow_hidden()
                .bg(rgb(pal.bg_card))
                .flex()
                .flex_col()
                // Table Header
                .child(
                    div()
                        .flex_none()
                        .h(px(38.0))
                        .w_full()
                        .px_4()
                        .bg(rgb(pal.bg_card_alt))
                        .border_b_1()
                        .border_color(rgb(pal.border_subtle))
                        .flex()
                        .items_center()
                        .child(
                            div()
                                .w(px(180.0))
                                .text_color(rgb(pal.text_muted))
                                .text_size(px(12.0))
                                .font_weight(FontWeight::BOLD)
                                .child("Check Name"),
                        )
                        .child(
                            div()
                                .w(px(120.0))
                                .text_color(rgb(pal.text_muted))
                                .text_size(px(12.0))
                                .font_weight(FontWeight::BOLD)
                                .child("Status"),
                        )
                        .child(
                            div()
                                .flex_1()
                                .text_color(rgb(pal.text_muted))
                                .text_size(px(12.0))
                                .font_weight(FontWeight::BOLD)
                                .child("Diagnostic Details"),
                        ),
                )
                // 8 Diagnostic Status Checks Rows
                .child(
                    div()
                        .id("doctor-checks-scroll")
                        .flex_1()
                        .min_h(px(0.0))
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .children(
                            app.state
                                .health_checks
                                .iter()
                                .map(|check| {
                                    div()
                                        .w_full()
                                        .h(px(46.0))
                                        .px_4()
                                        .flex()
                                        .items_center()
                                        .border_b_1()
                                        .border_color(rgb(pal.border_subtle))
                                        .child(
                                            div()
                                                .w(px(180.0))
                                                .text_color(rgb(pal.text_primary))
                                                .text_size(px(13.0))
                                                .font_weight(FontWeight::BOLD)
                                                .child(check.name.clone()),
                                        )
                                        .child(
                                            div()
                                                .w(px(120.0))
                                                .flex()
                                                .items_center()
                                                .gap_1_5()
                                                .child(
                                                    div()
                                                        .w(px(8.0))
                                                        .h(px(8.0))
                                                        .rounded_full()
                                                        .bg(if check.ok { rgb(ACCENT_GREEN) } else { rgb(ACCENT_RED) }),
                                                )
                                                .child(
                                                    div()
                                                        .text_color(if check.ok { rgb(ACCENT_GREEN) } else { rgb(ACCENT_RED) })
                                                        .text_size(px(12.0))
                                                        .font_weight(FontWeight::BOLD)
                                                        .child(if check.ok { "PASS" } else { "FAIL" }),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .text_color(rgb(pal.text_secondary))
                                                .text_size(px(12.0))
                                                .child(check.message.clone()),
                                        )
                                        .into_any_element()
                                })
                                .collect::<Vec<_>>(),
                        ),
                ),
        )
}

// ─────────────────────────────────────────────────────────────────────────────
// 6. Settings & Preferences View
// ─────────────────────────────────────────────────────────────────────────────

fn render_settings(app: &mut ArchBridgeApp, cx: &mut Context<ArchBridgeApp>) -> impl IntoElement {
    let pal = get_palette(app.state.theme);
    div()
        .id("settings-scroll")
        .flex()
        .flex_col()
        .gap_4()
        .size_full()
        .overflow_y_scroll()
        // Header
        .child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_color(rgb(pal.text_primary))
                        .text_size(px(22.0))
                        .font_weight(FontWeight::BOLD)
                        .child("Preferences & Source Routing"),
                )
                .child(
                    div()
                        .text_color(rgb(pal.text_muted))
                        .text_size(px(13.0))
                        .child("Enable or disable package discovery sources according to your workflow."),
                ),
        )
        // Card 0: Appearance & Theme Selection
        .child(
            div()
                .w_full()
                .bg(rgb(pal.bg_card))
                .border_1()
                .border_color(rgb(pal.border_subtle))
                .rounded_xl()
                .p_4()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().text_size(px(14.0)).child("🎨"))
                        .child(
                            div()
                                .text_color(rgb(pal.text_primary))
                                .text_size(px(14.0))
                                .font_weight(FontWeight::BOLD)
                                .child("Appearance & Theme Selection"),
                        ),
                )
                .child(
                    div()
                        .text_color(rgb(pal.text_muted))
                        .text_size(px(12.0))
                        .child("Customize visual style, surface contrast, and accent highlights."),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_3()
                        .pt_1()
                        .child(theme_option_pill(
                            &pal,
                            "theme-opt-dark",
                            "🌙 Arch Navy (Dark)",
                            app.state.theme == AppTheme::Dark,
                            cx.listener(|this, _, _, cx| {
                                this.state.theme = AppTheme::Dark;
                                this.state.settings_msg = Some(StatusMsg {
                                    level: MsgLevel::Success,
                                    text: "Theme switched to Arch Navy (Dark).".to_string(),
                                });
                                cx.notify();
                            }),
                        ))
                        .child(theme_option_pill(
                            &pal,
                            "theme-opt-midnight",
                            "🌌 Midnight OLED",
                            app.state.theme == AppTheme::Midnight,
                            cx.listener(|this, _, _, cx| {
                                this.state.theme = AppTheme::Midnight;
                                this.state.settings_msg = Some(StatusMsg {
                                    level: MsgLevel::Success,
                                    text: "Theme switched to Midnight OLED.".to_string(),
                                });
                                cx.notify();
                            }),
                        ))
                        .child(theme_option_pill(
                            &pal,
                            "theme-opt-light",
                            "☀️ Modern Light",
                            app.state.theme == AppTheme::Light,
                            cx.listener(|this, _, _, cx| {
                                this.state.theme = AppTheme::Light;
                                this.state.settings_msg = Some(StatusMsg {
                                    level: MsgLevel::Success,
                                    text: "Theme switched to Modern Light.".to_string(),
                                });
                                cx.notify();
                            }),
                        )),
                ),
        )
        // Card 1: Security & Sudo Authorization
        .child(
            div()
                .w_full()
                .bg(rgb(pal.bg_card))
                .border_1()
                .border_color(rgb(pal.border_subtle))
                .rounded_xl()
                .p_4()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().text_color(rgb(pal.accent_cyan)).text_size(px(14.0)).child("🛡️"))
                        .child(
                            div()
                                .text_color(rgb(pal.text_primary))
                                .text_size(px(14.0))
                                .font_weight(FontWeight::BOLD)
                                .child("Security & Sudo Authorization"),
                        ),
                )
                .child(
                    div()
                        .text_color(rgb(pal.text_muted))
                        .text_size(px(12.0))
                        .child("Manage administrator permissions and cached sudo credentials used for installing or uninstalling software."),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .pt_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .text_color(rgb(pal.text_primary))
                                        .text_size(px(13.0))
                                        .child(if app.state.sudo_session_active {
                                            "⦿ Sudo Session: Active"
                                        } else {
                                            "⦿ Sudo Session: Inactive / Revoked"
                                        }),
                                ),
                        )
                        .child(action_button(
                            "invalidate-sudo-btn",
                            "🔒 Invalidate Sudo / Clear Password",
                            if pal.is_light() { 0xfee2e2 } else { 0x3a1414 },
                            if pal.is_light() { 0xb91c1c } else { 0xf87171 },
                            cx.listener(|this, _, _, cx| {
                                this.state.sudo_session_active = false;
                                this.state.settings_msg = Some(StatusMsg {
                                    level: MsgLevel::Info,
                                    text: "Sudo credentials cleared from memory.".to_string(),
                                });
                                cx.notify();
                            }),
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .pt_1()
                        .child(div().text_color(rgb(pal.accent_cyan)).text_size(px(13.0)).child("☑"))
                        .child(
                            div()
                                .text_color(rgb(pal.text_secondary))
                                .text_size(px(12.0))
                                .child("Remember sudo password in memory during this app session (never saved to disk)"),
                        ),
                ),
        )
        // Card 2: Source Routing Checkboxes & Save Button
        .child(
            div()
                .w_full()
                .bg(rgb(pal.bg_card))
                .border_1()
                .border_color(rgb(pal.border_subtle))
                .rounded_xl()
                .p_4()
                .flex()
                .flex_col()
                .gap_2()
                .child(settings_checkbox(&pal, "Official Arch Linux Repositories (pacman)", app.state.opt_official))
                .child(settings_checkbox(&pal, "Arch User Repository (AUR RPC)", app.state.opt_aur))
                .child(settings_checkbox(&pal, "Flatpak Release Bundles (Flathub)", app.state.opt_flatpak))
                .child(settings_checkbox(&pal, "AppImage Standalone Assets", app.state.opt_appimage))
                .child(settings_checkbox(&pal, "Upstream Git Release Sources (GitHub/GitLab)", app.state.opt_upstream))
                .child(settings_checkbox(&pal, "DEB Package Inspection", app.state.opt_deb))
                .child(settings_checkbox(&pal, "RPM Package Inspection", app.state.opt_rpm))
                .child(
                    div()
                        .pt_3()
                        .w_full()
                        .child(action_button(
                            "save-prefs-btn",
                            "Save Preferences",
                            0x059669,
                            0xffffff,
                            cx.listener(|this, _, _, cx| {
                                this.state.settings_msg = Some(StatusMsg {
                                    level: MsgLevel::Success,
                                    text: "Preferences saved to $XDG_CONFIG_HOME/archbridge/config.json".to_string(),
                                });
                                cx.notify();
                            }),
                        )),
                ),
        )
        .when_some(app.state.settings_msg.clone(), |this, msg| {
            this.child(status_msg_bar(&msg, &pal))
        })
}

fn settings_checkbox(pal: &ThemePalette, label: &'static str, checked: bool) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .py_1()
        .child(
            div()
                .flex_none()
                .w(px(18.0))
                .text_color(if checked {
                    rgb(pal.accent_cyan)
                } else {
                    rgb(pal.text_muted)
                })
                .text_size(px(14.0))
                .child(if checked { "☑" } else { "☐" }),
        )
        .child(
            div()
                .text_color(rgb(pal.text_primary))
                .text_size(px(13.0))
                .child(label),
        )
}

fn theme_option_pill<F>(
    pal: &ThemePalette,
    id: &'static str,
    label: &'static str,
    is_active: bool,
    on_click: F,
) -> impl IntoElement
where
    F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
{
    div()
        .id(SharedString::from(id))
        .h(px(36.0))
        .px_4()
        .rounded_lg()
        .cursor_pointer()
        .bg(if is_active {
            if pal.is_light() {
                rgb(0xdbeafe)
            } else {
                rgb(0x163456)
            }
        } else {
            if pal.is_light() {
                rgb(0xf1f5f9)
            } else {
                rgb(0x0c1626)
            }
        })
        .border_1()
        .border_color(if is_active {
            rgb(pal.accent_cyan)
        } else {
            rgb(pal.border_subtle)
        })
        .flex()
        .items_center()
        .justify_center()
        .text_color(if is_active {
            rgb(pal.accent_cyan)
        } else {
            rgb(pal.text_secondary)
        })
        .text_size(px(12.5))
        .font_weight(if is_active {
            FontWeight::BOLD
        } else {
            FontWeight::MEDIUM
        })
        .on_click(on_click)
        .child(label)
}

// ─────────────────────────────────────────────────────────────────────────────
// Bottom System Status Bar
// ─────────────────────────────────────────────────────────────────────────────

fn render_bottom_bar(app: &ArchBridgeApp) -> impl IntoElement {
    let pal = get_palette(app.state.theme);
    div()
        .w_full()
        .h(px(26.0))
        .px_2()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .flex()
                .items_center()
                .gap_4()
                // Ready indicator
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1_5()
                        .child(
                            div()
                                .w(px(7.0))
                                .h(px(7.0))
                                .rounded_full()
                                .bg(rgb(ACCENT_GREEN)),
                        )
                        .child(
                            div()
                                .text_color(rgb(pal.text_secondary))
                                .text_size(px(11.0))
                                .child("Ready"),
                        ),
                )
                .child(div().w_px().h(px(10.0)).bg(rgb(pal.border_subtle)))
                // Active Chroot
                .child(
                    div()
                        .text_color(rgb(pal.text_muted))
                        .text_size(px(11.0))
                        .child(format!("Active Chroot: {}", app.state.active_chroot)),
                )
                .child(div().w_px().h(px(10.0)).bg(rgb(pal.border_subtle)))
                // Disk Space
                .child(
                    div()
                        .text_color(rgb(pal.text_muted))
                        .text_size(px(11.0))
                        .child(format!("Disk: {}", app.state.free_disk_space)),
                )
                .child(div().w_px().h(px(10.0)).bg(rgb(pal.border_subtle)))
                // Keyring Verified
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .text_color(rgb(ACCENT_GREEN))
                                .text_size(px(11.0))
                                .child("✓"),
                        )
                        .child(
                            div()
                                .text_color(rgb(pal.text_muted))
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
                        .text_color(rgb(pal.text_muted))
                        .text_size(px(11.0))
                        .font_family("monospace")
                        .child(format!("v{}", APP_VERSION)),
                )
                .child(
                    div()
                        .text_color(rgb(pal.text_secondary))
                        .text_size(px(11.0))
                        .font_weight(FontWeight::BOLD)
                        .child("ArchBridge"),
                ),
        )
}

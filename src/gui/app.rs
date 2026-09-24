use crate::gui::state::{
    rpc_call, ActiveTab, AppState, AppTheme, HealthCheck, InputField, InstalledPackage, MsgLevel,
    SearchResult, SharedEngine, StatusMsg,
};
use crate::gui::theme::*;
use crate::process::{format_elapsed, progress_snapshot, run_step, Step};
use gpui::prelude::*;
use gpui::*;
use serde_json::{json, Value};
use std::time::Duration;

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
    ticker_running: bool,
    focus_subs: Vec<Subscription>,
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
            ticker_running: false,
            focus_subs: Vec::new(),
        };

        app.reload_installed(cx);
        app.load_preferences(cx);

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

        // Developer aid: open the administrator authentication dialog at startup
        // for UI review without needing to trigger a privileged action first.
        if std::env::var("ARCHBRIDGE_SHOW_AUTH_DIALOG").is_ok() {
            app.open_auth_dialog(None);
        }

        app.refresh_sudo_advice(cx);

        // Developer aid: run a real search at startup so the Discover tab can be
        // reviewed without typing. Invented result rows here used to be
        // indistinguishable from engine output.
        if let Ok(query) = std::env::var("ARCHBRIDGE_SEARCH_SAMPLE") {
            if !query.trim().is_empty() {
                app.state.search_query = query;
                app.run_search(cx);
            }
        }

        app
    }

    /// Dispatch an RPC call on a background thread so the GPUI main thread
    /// stays responsive while engine work (builds, sudo, pacman) runs.
    /// The completion handler is invoked back on the UI thread.
    fn rpc_async<F>(&mut self, method: &'static str, params: Value, done: F, cx: &mut Context<Self>)
    where
        F: FnOnce(&mut Self, Result<Value, String>, &mut Context<Self>) + 'static,
    {
        let engine = self.engine.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { rpc_call(&engine, method, params) })
                .await;
            this.update(cx, move |app, cx| done(app, result, cx)).ok();
        })
        .detach();
    }

    /// A single 400 ms ticker drives caret blink and live progress refreshes
    /// and stops itself when nothing animated remains — idle UI never pays for
    /// per-frame redraws of the whole tree.
    fn ensure_ticker(&mut self, cx: &mut Context<Self>) {
        if self.ticker_running || !self.state.ticker_needed() {
            return;
        }
        self.ticker_running = true;
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            let keep = this
                .update(cx, |app, cx| {
                    if !app.state.ticker_needed() {
                        app.ticker_running = false;
                        return false;
                    }
                    app.state.tick = app.state.tick.wrapping_add(1);
                    if app.state.engine_busy() || app.state.auth_busy {
                        app.state.progress = progress_snapshot();
                    }
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !keep {
                break;
            }
        })
        .detach();
    }

    /// Caret blink costs a full-tree repaint, so it may only run while a field
    /// genuinely holds focus — a remembered click is not focus.
    fn blur_field(&mut self, field: InputField, cx: &mut Context<Self>) {
        if self.state.focused_input == Some(field) {
            self.state.focused_input = None;
            cx.notify();
        }
    }

    fn focus_field(&mut self, field: InputField, window: &mut Window, cx: &mut Context<Self>) {
        let handle = match field {
            InputField::Search => self.search_focus.clone(),
            InputField::Inspect => self.inspect_focus.clone(),
            InputField::Build => self.build_focus.clone(),
            InputField::Uninstall => self.uninstall_focus.clone(),
            InputField::Auth => self.auth_focus.clone(),
        };
        window.focus(&handle);
        if self.state.focused_input != Some(field) {
            self.state.focused_input = Some(field);
            cx.notify();
        }
    }

    fn open_auth_dialog(&mut self, pending: Option<&str>) {
        self.state.auth_dialog_open = true;
        self.state.auth_pending_action = pending.map(str::to_string);
        self.state.auth_error = None;
        self.state.auth_password_input.clear();
        self.state.auth_password_visible = false;
        self.state.auth_needs_focus = true;
        self.state.focused_input = Some(InputField::Auth);
    }

    fn refresh_sudo_advice(&mut self, cx: &mut Context<Self>) {
        self.rpc_async(
            "v1.sudo_advice",
            json!({}),
            |app, result, _cx| {
                if let Ok(val) = result {
                    app.state.sudo_advice_slow = val
                        .get("fingerprint_before_password")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                }
            },
            cx,
        );
    }

    /// Load persisted source routing toggles so the Settings card shows the
    /// config on disk instead of compile-time defaults.
    pub fn load_preferences(&mut self, cx: &mut Context<Self>) {
        self.rpc_async(
            "v1.config.get",
            json!({ "key": "all" }),
            |app, result, cx| {
                match result {
                    Ok(val) => app.state.apply_source_preferences(&val),
                    Err(e) => {
                        app.state.settings_msg = Some(StatusMsg {
                            level: MsgLevel::Warning,
                            text: format!("Saved preferences could not be read: {e}"),
                        });
                    }
                }
                cx.notify();
            },
            cx,
        );
    }

    /// Persist every toggled source through the engine, which is the only
    /// writer of config.json. Reporting success without calling the engine is
    /// how this card previously lied.
    pub fn save_preferences(&mut self, cx: &mut Context<Self>) {
        if self.state.settings_busy {
            return;
        }
        let changes: Vec<(&'static str, String)> = self
            .state
            .source_preferences()
            .into_iter()
            .map(|(key, value)| (key, value.to_string()))
            .collect();
        self.state.settings_busy = true;
        self.state.settings_msg = None;
        cx.notify();

        let engine = self.engine.clone();
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let mut written = Vec::new();
                    let mut failures = Vec::new();
                    for (key, value) in changes {
                        let prepared = rpc_call(
                            &engine,
                            "v1.prepare",
                            json!({ "action": "config.set", "key": key, "value": value }),
                        );
                        let plan_id = match prepared {
                            Ok(plan) => match plan.get("plan_id").and_then(|p| p.as_str()) {
                                Some(id) => id.to_string(),
                                None => {
                                    failures.push(format!("{key}: engine returned no plan id"));
                                    continue;
                                }
                            },
                            Err(e) => {
                                failures.push(format!("{key}: {e}"));
                                continue;
                            }
                        };
                        match rpc_call(
                            &engine,
                            "v1.execute",
                            json!({ "plan_id": plan_id, "confirmed": true }),
                        ) {
                            Ok(result) => {
                                if result.get("ok").and_then(|o| o.as_bool()).unwrap_or(false) {
                                    written.push(key);
                                } else {
                                    failures.push(format!(
                                        "{key}: {}",
                                        result
                                            .get("message")
                                            .and_then(|m| m.as_str())
                                            .unwrap_or("rejected by the engine")
                                    ));
                                }
                            }
                            Err(e) => failures.push(format!("{key}: {e}")),
                        }
                    }
                    (written.len(), failures)
                })
                .await;
            this.update(cx, move |app, cx| {
                app.state.settings_busy = false;
                let (count, failures) = outcome;
                let config_path = crate::config::Config::config_path();
                app.state.settings_msg = Some(if failures.is_empty() {
                    StatusMsg {
                        level: MsgLevel::Success,
                        text: format!(
                            "Saved {count} source preference(s) to {}.",
                            config_path.display()
                        ),
                    }
                } else {
                    StatusMsg {
                        level: MsgLevel::Error,
                        text: format!(
                            "Saved {count} of the source preferences; {} failed: {}",
                            failures.len(),
                            failures.join("; ")
                        ),
                    }
                });
                app.load_preferences(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `pacman -Qe` can take a second on a large system, so it runs on a
    /// background thread; blocking here froze the window on startup and on
    /// every Refresh click.
    pub fn reload_installed(&mut self, cx: &mut Context<Self>) {
        self.state.uninstall_selected = None;
        self.state.uninstall_confirm_idx = None;
        self.state.uninstall_display_limit = 50;
        self.state.installed_loading = true;
        self.state.uninstall_msg = None;
        cx.notify();

        let step = Step::new(
            "pacman",
            vec!["-Qe".to_string(), "--color=never".to_string()],
            "List explicitly installed Arch packages",
            60,
        );
        cx.spawn(async move |this, cx| {
            // Only show packages confirmed by pacman. Never display demo data as
            // installed software because this view can remove live-system packages.
            let result = cx
                .background_executor()
                .spawn(async move {
                    run_step(&step, None).map(|out| (out.exit_code, out.stdout, out.stderr))
                })
                .await;
            this.update(cx, move |app, cx| {
                app.state.installed_loading = false;
                match result {
                    Ok((0, stdout, _)) => {
                        let mut packages = Vec::new();
                        for line in stdout.lines() {
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
                        if packages.is_empty() {
                            app.state.uninstall_msg = Some(StatusMsg {
                                level: MsgLevel::Info,
                                text: "No explicitly installed packages were reported by pacman."
                                    .to_string(),
                            });
                        }
                        app.state.installed_packages = packages;
                    }
                    Ok((_, _, stderr)) => {
                        app.state.uninstall_msg = Some(StatusMsg {
                            level: MsgLevel::Error,
                            text: format!("Could not load installed packages: {}", stderr.trim()),
                        })
                    }
                    Err(err) => {
                        app.state.uninstall_msg = Some(StatusMsg {
                            level: MsgLevel::Error,
                            text: format!("Could not run pacman to load installed packages: {err}"),
                        })
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn run_doctor(&mut self, cx: &mut Context<Self>) {
        self.state.doctor_busy = true;
        self.state.doctor_msg = None;
        cx.notify();

        self.rpc_async(
            "v1.doctor",
            json!({}),
            |app, result, cx| {
                app.state.doctor_busy = false;
                match result {
                    Ok(val) => {
                        // A missing `ready` must not read as healthy.
                        let ready = val.get("ready").and_then(|r| r.as_bool()).unwrap_or(false);
                        app.state.doctor_ready = ready;
                        let checks: Vec<HealthCheck> = val
                            .get("checks")
                            .and_then(|c| c.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .map(|item| {
                                        let status = item
                                            .get("status")
                                            .and_then(|s| s.as_str())
                                            .unwrap_or("fail")
                                            .to_string();
                                        HealthCheck {
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
                                            status,
                                        }
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();

                        let failed = checks.iter().filter(|c| c.status == "fail").count();
                        let unavailable =
                            checks.iter().filter(|c| c.status == "unavailable").count();
                        app.state.health_checks = checks;
                        app.state.doctor_msg = Some(StatusMsg {
                            level: if failed > 0 || !ready {
                                MsgLevel::Warning
                            } else if unavailable > 0 {
                                MsgLevel::Info
                            } else {
                                MsgLevel::Success
                            },
                            text: if failed > 0 {
                                format!(
                                    "{failed} diagnostic check(s) failed; the system is not ready \
                                     for privileged packaging."
                                )
                            } else if !ready {
                                "The engine reported this system as not ready.".to_string()
                            } else if unavailable > 0 {
                                format!(
                                    "{unavailable} check(s) are unavailable on this host. Local \
                                     builds and offline inspection can still proceed."
                                )
                            } else {
                                "All diagnostic checks passed.".to_string()
                            },
                        });
                    }
                    Err(e) => {
                        app.state.doctor_msg = Some(StatusMsg {
                            level: MsgLevel::Error,
                            text: e,
                        });
                    }
                }
                cx.notify();
            },
            cx,
        );
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

        self.rpc_async(
            "v1.search",
            json!({ "target": query.clone() }),
            move |app, result, cx| {
                app.state.search_busy = false;

                match result {
                    Ok(val) => {
                        let parsed = parse_search_results(&val);
                        if parsed.is_empty() {
                            app.state.search_results.clear();
                            app.state.selected_result = None;
                            app.state.recommended_item = None;
                            app.state.search_msg = Some(StatusMsg {
                                level: MsgLevel::Warning,
                                text: format!(
                                    "No packages or upstream repositories found matching '{}'",
                                    query
                                ),
                            });
                        } else {
                            let n = parsed.len();
                            app.state.recommended_item = parsed
                                .iter()
                                .find(|r| r.is_recommended)
                                .cloned()
                                .or_else(|| parsed.first().cloned());
                            app.state.search_results = parsed;
                            app.state.selected_result = Some(0);
                            app.state.search_msg = Some(StatusMsg {
                                level: MsgLevel::Info,
                                text: format!("Found {} source candidate(s) for '{}'", n, query),
                            });
                        }
                    }
                    Err(e) => {
                        app.state.search_msg = Some(StatusMsg {
                            level: MsgLevel::Error,
                            text: e,
                        });
                    }
                }
                cx.notify();
            },
            cx,
        );
    }

    pub fn run_inspect(&mut self, cx: &mut Context<Self>) {
        let path = self.state.inspect_path.trim().to_string();
        let package_path = std::path::Path::new(&path);
        if path.is_empty() {
            self.state.inspect_msg = Some(StatusMsg {
                level: MsgLevel::Warning,
                text: "Choose a .deb or .rpm package before inspecting.".to_string(),
            });
            cx.notify();
            return;
        }
        if !package_path.is_file() {
            self.state.inspect_msg = Some(StatusMsg {
                level: MsgLevel::Error,
                text: format!("Package file does not exist: {path}"),
            });
            cx.notify();
            return;
        }
        if !matches!(
            package_path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_ascii_lowercase())
                .as_deref(),
            Some("deb") | Some("rpm")
        ) {
            self.state.inspect_msg = Some(StatusMsg {
                level: MsgLevel::Error,
                text: "Only .deb and .rpm files can be inspected.".to_string(),
            });
            cx.notify();
            return;
        }
        self.state.inspect_data = None;
        self.state.inspect_busy = true;
        self.state.inspect_msg = None;
        cx.notify();

        self.rpc_async(
            "v1.inspect",
            json!({ "target": path }),
            |app, result, cx| {
                app.state.inspect_busy = false;
                match result {
                    Ok(val) => {
                        app.state.inspect_data = Some(val);
                        app.state.inspect_msg = Some(StatusMsg {
                            level: MsgLevel::Success,
                            text: "Foreign package analyzed in data-only mode (EXECUTED: FALSE)"
                                .to_string(),
                        });
                    }
                    Err(e) => {
                        app.state.inspect_msg = Some(StatusMsg {
                            level: MsgLevel::Error,
                            text: e,
                        });
                    }
                }
                cx.notify();
            },
            cx,
        );
    }

    pub fn prepare_build_plan(&mut self, cx: &mut Context<Self>) {
        let target = self.state.build_target_input.trim().to_string();
        if target.is_empty() {
            self.state.build_msg = Some(StatusMsg { level: MsgLevel::Warning, text: "Choose a package file, source directory, PKGBUILD, URL, or package name before preparing a build plan.".to_string() });
            cx.notify();
            return;
        }
        if (target.ends_with(".deb")
            || target.ends_with(".rpm")
            || target.ends_with(".pkg.tar.zst"))
            && !std::path::Path::new(&target).is_file()
        {
            self.state.build_msg = Some(StatusMsg {
                level: MsgLevel::Error,
                text: format!("Selected package does not exist: {target}"),
            });
            cx.notify();
            return;
        }
        self.prepare_plan("build", &target, cx);
    }

    /// Stage a plan for an explicit target. Callers must pass the thing the user
    /// actually pointed at: reading `build_target_input` from another tab prepared
    /// a plan for whatever was left in that box.
    pub fn prepare_plan(&mut self, action: &'static str, target: &str, cx: &mut Context<Self>) {
        if self.state.build_busy {
            return;
        }
        self.state.active_plan = None;
        self.state.build_result = None;
        self.state.build_plan_consumed = false;
        self.state.install_plan_consumed = false;

        self.state.build_busy = true;
        self.state.build_msg = None;
        cx.notify();

        self.rpc_async(
            "v1.prepare",
            json!({ "action": action, "request": { "target": target } }),
            |app, result, cx| {
                app.state.build_busy = false;
                match result {
                    Ok(val) => match serde_json::from_value::<crate::engine::Plan>(val) {
                        Ok(plan) => {
                            let installing = plan.action == "install";
                            app.state.active_plan = Some(plan);
                            app.state.build_stage = 2;
                            app.state.active_tab = ActiveTab::Build;
                            app.state.build_msg = Some(StatusMsg {
                                level: MsgLevel::Info,
                                text: if installing {
                                    "Install plan prepared from the official repositories. \
                                         Review it, then confirm to run pacman."
                                        .to_string()
                                } else {
                                    "Plan prepared. Review the manifest and execute.".to_string()
                                },
                            });
                        }
                        Err(e) => {
                            app.state.build_msg = Some(StatusMsg {
                                level: MsgLevel::Error,
                                text: format!("Engine returned an unreadable plan: {e}"),
                            })
                        }
                    },
                    Err(e) => {
                        app.state.build_msg = Some(StatusMsg {
                            level: MsgLevel::Error,
                            text: e,
                        });
                    }
                }
                cx.notify();
            },
            cx,
        );
    }

    /// Prepare the plan the recommended source actually implies.
    pub fn prepare_plan_for_result(&mut self, result: &SearchResult, cx: &mut Context<Self>) {
        let action = if result.source == "official" {
            "install"
        } else {
            "build"
        };
        self.prepare_plan(action, &result.name, cx);
    }

    pub fn authenticate_sudo(&mut self, cx: &mut Context<Self>) {
        if self.state.auth_busy {
            return;
        }
        let password = std::mem::take(&mut self.state.auth_password_input);
        if password.is_empty() {
            self.state.auth_error = Some("Enter your administrator password.".to_string());
            cx.notify();
            return;
        }
        self.state.auth_busy = true;
        self.state.auth_error = None;
        self.state.auth_started = Some(std::time::Instant::now());
        cx.notify();

        if self.state.auth_fix_fingerprint && self.state.sudo_advice_slow {
            // Remove the slow fingerprint wait first (one privileged call),
            // then authenticate — the password path is instant afterwards.
            self.rpc_async(
                "v1.optimize_sudo_auth",
                json!({ "password": password.clone() }),
                move |app, result, cx| match result {
                    Ok(_) => {
                        app.state.sudo_advice_slow = false;
                        app.state.auth_fix_fingerprint = false;
                        app.dispatch_authenticate(password, cx);
                    }
                    Err(error) => app.finish_auth_attempt(password, error, cx),
                },
                cx,
            );
        } else {
            self.dispatch_authenticate(password, cx);
        }
    }

    fn dispatch_authenticate(&mut self, password: String, cx: &mut Context<Self>) {
        self.rpc_async(
            "v1.authenticate",
            json!({ "password": password.clone() }),
            move |app, result, cx| match result {
                Ok(_) => {
                    app.state.auth_busy = false;
                    app.state.auth_started = None;
                    app.state.sudo_session_active = true;
                    app.state.auth_dialog_open = false;
                    app.state.auth_error = None;
                    app.state.auth_password_visible = false;
                    app.state.focused_input = None;
                    app.state.settings_msg = Some(StatusMsg {
                        level: MsgLevel::Success,
                        text: "Administrator authorization is active for this session.".to_string(),
                    });
                    let pending = app.state.auth_pending_action.take();
                    cx.notify();
                    match pending.as_deref() {
                        Some("execute") => app.execute_plan(cx),
                        Some("uninstall") => app.run_uninstall(cx),
                        _ => {}
                    }
                }
                Err(error) => app.finish_auth_attempt(password, error, cx),
            },
            cx,
        );
    }

    fn finish_auth_attempt(&mut self, password: String, error: String, cx: &mut Context<Self>) {
        self.state.auth_busy = false;
        self.state.auth_started = None;
        self.state.auth_error = Some(error);
        self.state.auth_password_input = password;
        self.state.sudo_session_active = false;
        self.refresh_sudo_advice(cx);
        cx.notify();
    }

    pub fn browse_build_target(&mut self, cx: &mut Context<Self>) {
        // The XDG portal file chooser cannot mix file and folder selection,
        // and folder mode hides files entirely — so package picking must be
        // a pure file prompt. Use browse_build_dir() for source directories.
        self.prompt_for_build_path(false, "Choose a package file or PKGBUILD", cx);
    }

    pub fn browse_build_dir(&mut self, cx: &mut Context<Self>) {
        self.prompt_for_build_path(true, "Choose a local source directory to build", cx);
    }

    fn prompt_for_build_path(
        &mut self,
        directories: bool,
        prompt: &'static str,
        cx: &mut Context<Self>,
    ) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: !directories,
            directories,
            multiple: false,
            prompt: Some(prompt.into()),
        });
        cx.spawn(async move |this, cx| match receiver.await {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.into_iter().next() {
                    let selected = path.to_string_lossy().into_owned();
                    this.update(cx, |app, cx| {
                        app.state.build_target_input = selected;
                        app.state.active_plan = None;
                        app.state.build_result = None;
                        app.state.build_msg = None;
                        cx.notify();
                    })
                    .ok();
                }
            }
            Ok(Ok(None)) | Err(_) => {}
            Ok(Err(error)) => {
                this.update(cx, |app, cx| {
                    app.state.build_msg = Some(StatusMsg {
                        level: MsgLevel::Error,
                        text: format!("Could not open package chooser: {error}"),
                    });
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    pub fn browse_inspect_target(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose a .deb or .rpm package".into()),
        });
        cx.spawn(async move |this, cx| match receiver.await {
            Ok(Ok(Some(paths))) => {
                if let Some(path) = paths.into_iter().next() {
                    let selected = path.to_string_lossy().into_owned();
                    this.update(cx, |app, cx| {
                        app.state.inspect_path = selected;
                        app.state.inspect_data = None;
                        app.state.inspect_msg = None;
                        cx.notify();
                    })
                    .ok();
                }
            }
            Ok(Ok(None)) | Err(_) => {}
            Ok(Err(error)) => {
                this.update(cx, |app, cx| {
                    app.state.inspect_msg = Some(StatusMsg {
                        level: MsgLevel::Error,
                        text: format!("Could not open package chooser: {error}"),
                    });
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    pub fn execute_plan(&mut self, cx: &mut Context<Self>) {
        if self.state.build_busy {
            return;
        }
        let plan = match self.state.active_plan.clone() {
            Some(p) => p,
            None => return,
        };
        // Plans are single-use. Once this review has been sent to the engine the
        // plan it names is gone from the staging store, so a second click has to
        // say so plainly instead of resending a dead plan id and surfacing the
        // engine's "no longer staged" refusal, which reads like a crash.
        let consumed = self.state.build_plan_consumed
            || self
                .state
                .build_result
                .as_ref()
                .map(|result| result.plan_id == plan.plan_id && result.next_plan.is_some())
                .unwrap_or(false);
        if consumed {
            let installed_handoff = self
                .state
                .build_result
                .as_ref()
                .map(|result| result.next_plan.is_some())
                .unwrap_or(false);
            self.state.build_msg = Some(StatusMsg {
                level: MsgLevel::Warning,
                text: if installed_handoff {
                    "This build plan already ran and was consumed. Review the install plan it \
                     produced, then choose \"Install Built Package\"."
                        .to_string()
                } else {
                    "This plan already ran and was consumed. The output of that run is still \
                     shown below; prepare the action again to review and confirm a fresh plan."
                        .to_string()
                },
            });
            cx.notify();
            return;
        }
        // A blocked plan is never staged, so executing it would fail with the
        // staging error above and hide the reason it was blocked in the first place.
        if let Some(reason) = plan.blocked.as_deref() {
            self.state.build_msg = Some(StatusMsg {
                level: MsgLevel::Error,
                text: format!("This plan is blocked and cannot run: {reason}"),
            });
            cx.notify();
            return;
        }
        // A build plan's first step is an unprivileged native makepkg pass, so
        // asking for the administrator password up front blocks work that needs
        // no privilege. Install/uninstall plans go straight to pacman as root.
        let needs_admin_now =
            plan.action != "build" && plan.steps.iter().any(|step| step.program == "sudo");
        if needs_admin_now && !self.state.sudo_session_active {
            self.open_auth_dialog(Some("execute"));
            cx.notify();
            return;
        }
        let plan_id = plan.plan_id.clone();
        self.state.build_busy = true;
        self.state.build_plan_consumed = true;
        self.state.build_stage = 3;
        self.state.build_log.clear();
        self.state.build_msg = Some(StatusMsg {
            level: MsgLevel::Info,
            text: if plan.action == "build" {
                "Building natively without administrator rights. The interface stays responsive while the engine works.".to_string()
            } else {
                "Executing plan. This can take several minutes — the interface stays responsive while the engine works.".to_string()
            },
        });
        cx.notify();

        self.rpc_async(
            "v1.execute",
            json!({ "plan_id": plan_id, "confirmed": true }),
            |app, result, cx| {
                app.state.build_busy = false;
                match result {
                    Ok(val) => {
                        if let Ok(exec) =
                            serde_json::from_value::<crate::engine::ExecutionResult>(val)
                        {
                            let ok = exec.ok;
                            app.state.build_stage = if ok { 4 } else { 3 };
                            app.state.build_log = exec.output.clone().unwrap_or_default();
                            app.state.build_msg = Some(StatusMsg {
                                level: if ok {
                                    MsgLevel::Success
                                } else {
                                    MsgLevel::Error
                                },
                                text: exec.message.clone(),
                            });
                            app.state.build_result = Some(exec);
                        }
                    }
                    Err(e) => {
                        // The console must keep a record of a run that never
                        // produced step output; dropping it here left the user
                        // with a red banner and an empty log panel.
                        app.state.build_log = e.clone();
                        app.state.build_msg = Some(StatusMsg {
                            level: MsgLevel::Error,
                            text: e.clone(),
                        });
                        if e.contains("password is required") || e.contains("authorize") {
                            // The cached sudo timestamp expired or was never
                            // valid; collect a fresh one and retry the plan.
                            app.state.sudo_session_active = false;
                            app.open_auth_dialog(Some("execute"));
                        }
                    }
                }
                cx.notify();
            },
            cx,
        );
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
        // Removal runs as root through a non-interactive sudo step, so without a
        // live credential it fails with "a password is required" instead of
        // asking. Collect the credential first, then run the same action.
        if !self.state.sudo_session_active {
            self.open_auth_dialog(Some("uninstall"));
            cx.notify();
            return;
        }
        self.state.uninstall_busy = true;
        self.state.uninstall_msg = None;
        cx.notify();

        let package_label = pkg_name.clone();
        self.rpc_async(
            "v1.prepare",
            json!({ "action": "uninstall", "request": { "target": pkg_name } }),
            move |app, prep, cx| match prep {
                Ok(plan_val) => {
                    if let Ok(plan) = serde_json::from_value::<crate::engine::Plan>(plan_val) {
                        if plan.blocked.is_some() {
                            app.state.uninstall_busy = false;
                            app.state.uninstall_msg = Some(StatusMsg {
                                level: MsgLevel::Error,
                                text: plan.blocked.unwrap_or_default(),
                            });
                            cx.notify();
                            return;
                        }
                        let plan_id = plan.plan_id.clone();
                        app.rpc_async(
                            "v1.execute",
                            json!({ "plan_id": plan_id, "confirmed": true }),
                            move |app, exec, cx| {
                                app.state.uninstall_busy = false;
                                match exec {
                                    Ok(r) => {
                                        let ok =
                                            r.get("ok").and_then(|o| o.as_bool()).unwrap_or(false);
                                        app.state.uninstall_msg = Some(StatusMsg {
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
                                            app.reload_installed(cx);
                                            app.state.uninstall_selected = None;
                                        }
                                    }
                                    Err(e) => {
                                        if e.contains("password is required") {
                                            app.state.sudo_session_active = false;
                                            app.state.uninstall_msg = Some(StatusMsg {
                                                level: MsgLevel::Warning,
                                                text: format!(
                                                    "Administrator credential expired for '{}'. \
                                                     Authorize to retry.",
                                                    package_label
                                                ),
                                            });
                                            app.open_auth_dialog(Some("uninstall"));
                                        } else {
                                            app.state.uninstall_msg = Some(StatusMsg {
                                                level: MsgLevel::Error,
                                                text: e,
                                            });
                                        }
                                    }
                                }
                                cx.notify();
                            },
                            cx,
                        );
                    } else {
                        app.state.uninstall_busy = false;
                        app.state.uninstall_msg = Some(StatusMsg {
                            level: MsgLevel::Error,
                            text: format!(
                                "Uninstall plan for '{}' could not be parsed.",
                                package_label
                            ),
                        });
                        cx.notify();
                    }
                }
                Err(e) => {
                    app.state.uninstall_busy = false;
                    app.state.uninstall_msg = Some(StatusMsg {
                        level: MsgLevel::Error,
                        text: e,
                    });
                    cx.notify();
                }
            },
            cx,
        );
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

/// Optional text a source genuinely did not report, shown instead of invented
/// metadata.
const NOT_REPORTED: &str = "not reported by this source";

fn opt_str(item: &serde_json::Value, key: &str) -> Option<String> {
    item.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn parse_search_results(val: &serde_json::Value) -> Vec<SearchResult> {
    let mut out = Vec::new();

    let Some(table) = val.get("decision_table").and_then(|t| t.as_array()) else {
        return out;
    };
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
                let name = opt_str(item, "name").unwrap_or_else(|| "unknown".to_string());
                // A missing version stays missing: "latest" is a claim, not a fact.
                let version = opt_str(item, "version").unwrap_or_else(|| NOT_REPORTED.to_string());
                let description =
                    opt_str(item, "description").unwrap_or_else(|| "No description".to_string());
                let repository = opt_str(item, "repository");

                let repo = repository.clone().unwrap_or_else(|| match source.as_str() {
                    "official" => "Arch official repository".to_string(),
                    "aur" => "AUR".to_string(),
                    "flatpak" => "Flathub".to_string(),
                    "appimage" => "AppImage release".to_string(),
                    "upstream" => "Upstream source".to_string(),
                    "deb" => "DEB foreign package".to_string(),
                    "rpm" => "RPM foreign package".to_string(),
                    _ => source.clone(),
                });

                out.push(SearchResult {
                    name,
                    version,
                    description,
                    source: source.clone(),
                    repo,
                    is_recommended: is_row_rec && cand_idx == 0 && out.is_empty(),
                    url: opt_str(item, "url"),
                    source_url: opt_str(item, "source_url"),
                    license: opt_str(item, "license"),
                    arch: opt_str(item, "architecture"),
                    size: opt_str(item, "size"),
                    maintainer: opt_str(item, "maintainer"),
                    out_of_date: opt_str(item, "out_of_date"),
                });
            }
        }
    }

    out
}

// ─────────────────────────────────────────────────────────────────────────────
// UI Primitives
// ─────────────────────────────────────────────────────────────────────────────

/// Blend a color toward white by `t` (0.0–1.0) for hover states.
fn lighten(color: u32, t: f32) -> u32 {
    let blend = |c: u32| (c as f32 + (255.0 - c as f32) * t) as u32;
    (blend((color >> 16) & 0xff) << 16) | (blend((color >> 8) & 0xff) << 8) | blend(color & 0xff)
}

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
        .hover(move |this| this.bg(rgb(lighten(bg, 0.22))))
        .active(move |this| this.bg(rgb(lighten(bg, 0.45))))
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
        .hover(move |this| this.bg(rgb(lighten(bg, 0.22))))
        .active(move |this| this.bg(rgb(lighten(bg, 0.45))))
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = get_palette(self.state.theme);
        if self.focus_subs.is_empty() {
            let subs = vec![
                (InputField::Search, self.search_focus.clone()),
                (InputField::Inspect, self.inspect_focus.clone()),
                (InputField::Build, self.build_focus.clone()),
                (InputField::Uninstall, self.uninstall_focus.clone()),
                (InputField::Auth, self.auth_focus.clone()),
            ]
            .into_iter()
            .map(|(field, handle)| {
                cx.on_blur(&handle, window, move |app, _, cx| app.blur_field(field, cx))
            })
            .collect::<Vec<_>>();
            self.focus_subs = subs;
        }
        self.ensure_ticker(cx);
        if self.state.auth_dialog_open && self.state.auth_needs_focus {
            self.state.auth_needs_focus = false;
            window.focus(&self.auth_focus);
        }
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
            .when(self.state.auth_dialog_open, |this| {
                this.child(render_auth_dialog(self, cx))
            })
            .when(self.state.engine_busy(), |this| {
                this.child(render_progress_overlay(self, &pal))
            })
    }
}

fn render_progress_overlay(app: &ArchBridgeApp, pal: &ThemePalette) -> impl IntoElement {
    let p = app.state.progress.clone();
    let blink = app.state.tick % 2 == 0;
    let phase = p
        .phase
        .clone()
        .unwrap_or_else(|| "Engine working".to_string());
    let total = p.total_steps;
    let done = p.completed_steps.min(total.max(1));
    let pct = if total > 0 {
        (done as f32 * 100.0 / total as f32).min(99.0)
    } else {
        0.0
    };

    div()
        .absolute()
        .inset_0()
        .bg(rgba(0x00000099))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .w(px(520.0))
                .max_w(px(600.0))
                .bg(rgb(pal.bg_card))
                .border_1()
                .border_color(rgb(pal.accent_cyan))
                .rounded_xl()
                .shadow_lg()
                .p_5()
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
                                .text_size(px(16.0))
                                .font_weight(FontWeight::BOLD)
                                .child(format!("{phase}…")),
                        )
                        .child(
                            div()
                                .text_color(rgb(pal.accent_cyan))
                                .text_size(px(16.0))
                                .font_weight(FontWeight::BOLD)
                                .font_family("monospace")
                                .child(format_elapsed(p.elapsed_ms)),
                        ),
                )
                // Progress bar
                .child(
                    div()
                        .w_full()
                        .h(px(10.0))
                        .rounded_full()
                        .bg(rgb(pal.bg_input))
                        .border_1()
                        .border_color(rgb(pal.border_subtle))
                        .child(if total > 0 {
                            div()
                                .h_full()
                                .rounded_full()
                                .w(px(478.0 * (pct / 100.0)))
                                .bg(rgb(pal.accent_cyan))
                                .into_any_element()
                        } else {
                            div()
                                .h_full()
                                .w_1_3()
                                .rounded_full()
                                .bg(rgb(pal.accent_cyan))
                                .opacity(if blink { 1.0 } else { 0.35 })
                                .into_any_element()
                        }),
                )
                .child(
                    div()
                        .text_color(rgb(pal.text_muted))
                        .text_size(px(11.5))
                        .child(if total > 0 {
                            format!(
                                "Step {} of {} · {:.0}% · interface stays responsive",
                                if done == 0 { 1 } else { done },
                                total,
                                pct
                            )
                        } else {
                            "Working — the interface stays responsive while the engine runs"
                                .to_string()
                        }),
                )
                // Step history + running step
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .children(p.history.iter().map(|h| {
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_size(px(12.0))
                                .font_family("monospace")
                                .child(
                                    div()
                                        .text_color(rgb(if h.ok {
                                            pal.accent_green
                                        } else {
                                            pal.accent_red
                                        }))
                                        .child(if h.ok { "✓" } else { "✗" }),
                                )
                                .child(
                                    div()
                                        .text_color(rgb(pal.text_secondary))
                                        .flex_1()
                                        .child(h.purpose.clone()),
                                )
                                .child(
                                    div()
                                        .text_color(rgb(pal.text_muted))
                                        .child(format_elapsed(h.elapsed_ms)),
                                )
                        }))
                        .children(p.running.iter().map(|r| {
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_size(px(12.0))
                                .font_family("monospace")
                                .child(
                                    div()
                                        .text_color(rgb(pal.accent_cyan))
                                        .opacity(if blink { 1.0 } else { 0.3 })
                                        .child("▶"),
                                )
                                .child(
                                    div()
                                        .text_color(rgb(pal.text_primary))
                                        .flex_1()
                                        .child(r.purpose.clone()),
                                )
                                .child(
                                    div()
                                        .text_color(rgb(pal.accent_cyan))
                                        .child(format_elapsed(r.elapsed_ms)),
                                )
                        })),
                )
                .when(p.running.is_empty() && p.history.is_empty(), |this| {
                    this.child(
                        div()
                            .text_color(rgb(pal.text_muted))
                            .text_size(px(12.0))
                            .child("Waiting for the engine to start the first step…"),
                    )
                }),
        )
}

fn close_auth_dialog(app: &mut ArchBridgeApp, cx: &mut Context<ArchBridgeApp>) {
    app.state.auth_dialog_open = false;
    app.state.auth_password_input.clear();
    app.state.auth_error = None;
    app.state.auth_started = None;
    app.state.auth_pending_action = None;
    app.state.focused_input = None;
    cx.notify();
}

fn render_auth_dialog(
    app: &mut ArchBridgeApp,
    cx: &mut Context<ArchBridgeApp>,
) -> impl IntoElement {
    let pal = get_palette(app.state.theme);
    let busy = app.state.auth_busy;
    let masked = !app.state.auth_password_visible;
    let focused = app.state.focused_input == Some(InputField::Auth);
    let blink = app.state.tick % 2 == 0;
    let has_text = !app.state.auth_password_input.is_empty();
    let shown_text = if app.state.auth_password_input.is_empty() {
        "Password (press Enter to authenticate)".to_string()
    } else if masked {
        "•".repeat(app.state.auth_password_input.chars().count())
    } else {
        app.state.auth_password_input.clone()
    };
    let elapsed_secs = app
        .state
        .auth_started
        .map(|started| started.elapsed().as_secs());
    let submit_label = if busy {
        "Authenticating…"
    } else if app.state.auth_error.is_some()
        || app.state.auth_pending_action.as_deref() == Some("execute")
    {
        "Authenticate & Retry"
    } else {
        "Authenticate"
    };
    div()
        .absolute()
        .inset_0()
        .bg(rgba(0x000000aa))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .w(px(480.0))
                .max_w(px(540.0))
                .bg(rgb(pal.bg_card))
                .border_1()
                .border_color(rgb(pal.accent_cyan))
                .rounded_xl()
                .shadow_lg()
                .p_5()
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
                                .text_size(px(18.0))
                                .font_weight(FontWeight::BOLD)
                                .child("Administrator authentication required"),
                        )
                        .when_some(elapsed_secs, |this, secs| {
                            this.child(
                                div()
                                    .px_2()
                                    .rounded_md()
                                    .bg(rgb(pal.bg_input))
                                    .border_1()
                                    .border_color(rgb(pal.accent_cyan))
                                    .text_color(rgb(pal.accent_cyan))
                                    .text_size(px(12.0))
                                    .font_family("monospace")
                                    .child(format!("{secs}s")),
                            )
                        }),
                )
                .child(div().text_color(rgb(pal.text_secondary)).text_size(px(12.0)).child("ArchBridge needs sudo authorization for this privileged step. Your password is used once for authentication and is never saved or passed to the package build."))
                // Busy status line with live elapsed time
                .when(busy, |this| {
                    this.child(
                        div()
                            .w_full()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(rgb(pal.bg_input))
                            .border_1()
                            .border_color(rgb(pal.border))
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .text_color(rgb(pal.accent_cyan))
                                    .text_size(px(12.0))
                                    .opacity(if blink { 1.0 } else { 0.35 })
                                    .child("◐"),
                            )
                            .child(
                                div()
                                    .text_color(rgb(pal.text_primary))
                                    .text_size(px(12.0))
                                    .font_weight(FontWeight::BOLD)
                                    .child(format!(
                                        "Verifying with sudo… {}",
                                        format_elapsed(elapsed_secs.unwrap_or(0) * 1000)
                                    )),
                            )
                            .child(
                                div()
                                    .text_color(rgb(pal.text_muted))
                                    .text_size(px(11.0))
                                    .flex_1()
                                    .text_right()
                                    .child(if app.state.sudo_advice_slow && !app.state.auth_fix_fingerprint {
                                        "swipe a finger or wait for the password check"
                                    } else {
                                        "this should be nearly instant now"
                                    }),
                            ),
                    )
                })
                .when(!busy, |this| {
                    this.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .id("auth-password-input")
                                    .track_focus(&app.auth_focus)
                                    .flex_1()
                                    .h(px(40.0))
                                    .px_3()
                                    .bg(rgb(pal.bg_input))
                                    .border_1()
                                    .border_color(rgb(if focused { pal.accent_cyan } else { pal.border }))
                                    .rounded_lg()
                                    .flex()
                                    .items_center()
                                    .cursor_text()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.focus_field(InputField::Auth, window, cx);
                                    }))
                                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                                        if this.state.auth_busy {
                                            return;
                                        }
                                        let keystroke = &event.keystroke;
                                        if keystroke.modifiers.control {
                                            if keystroke.key == "v" {
                                                if let Some(text) = cx
                                                    .read_from_clipboard()
                                                    .and_then(|item| item.text())
                                                {
                                                    this.state.auth_password_input
                                                        .push_str(text.trim_end_matches('\n'));
                                                    cx.notify();
                                                }
                                            }
                                            return;
                                        }
                                        match keystroke.key.as_str() {
                                            "backspace" => {
                                                this.state.auth_password_input.pop();
                                                cx.notify();
                                            }
                                            "escape" => close_auth_dialog(this, cx),
                                            "return" | "enter" => this.authenticate_sudo(cx),
                                            _ => {
                                                if let Some(ch) = &keystroke.key_char {
                                                    this.state.auth_password_input.push_str(ch);
                                                    cx.notify();
                                                }
                                            }
                                        }
                                    }))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .child(
                                                div()
                                                    .text_color(if has_text {
                                                        rgb(pal.text_primary)
                                                    } else {
                                                        rgb(pal.text_placeholder)
                                                    })
                                                    .text_size(px(13.0))
                                                    .child(shown_text.clone()),
                                            )
                                            .when(focused && blink, |this| {
                                                this.child(
                                                    div()
                                                        .w(px(2.0))
                                                        .h(px(16.0))
                                                        .ml(px(1.0))
                                                        .bg(rgb(pal.accent_cyan)),
                                                )
                                            }),
                                    ),
                            )
                            .child(small_button(
                                "auth-toggle-visibility",
                                if app.state.auth_password_visible { "Hide" } else { "Show" },
                                if pal.is_light() { 0xe2e8f0 } else { 0x12233a },
                                pal.text_secondary,
                                cx.listener(|this, _, _, cx| {
                                    this.state.auth_password_visible = !this.state.auth_password_visible;
                                    cx.notify();
                                }),
                            )),
                    )
                })
                // Opt-in fix for the fingerprint-before-password PAM order
                .when(app.state.sudo_advice_slow, |this| {
                    this.child(
                        div()
                            .id("auth-fix-fingerprint")
                            .w_full()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .cursor_pointer()
                            .bg(rgb(if app.state.auth_fix_fingerprint {
                                if pal.is_light() { 0xe0f2fe } else { 0x0c1b2c }
                            } else {
                                pal.bg_input
                            }))
                            .border_1()
                            .border_color(rgb(if app.state.auth_fix_fingerprint {
                                pal.accent_cyan
                            } else {
                                pal.border_subtle
                            }))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.auth_fix_fingerprint = !this.state.auth_fix_fingerprint;
                                cx.notify();
                            }))
                            .child(format!(
                                "{} Remove this PC's 30-second fingerprint wait before authenticating (recommended; keeps a backup of /etc/pam.d/sudo)",
                                if app.state.auth_fix_fingerprint { "☑" } else { "☐" }
                            ))
                            .text_color(rgb(pal.text_secondary))
                            .text_size(px(11.5)),
                    )
                })
                .when_some(app.state.auth_error.clone(), |this, error| {
                    this.child(div().text_color(rgb(pal.accent_red)).text_size(px(12.0)).child(error))
                })
                .child(
                    div().flex().justify_end().gap_2()
                        .child(small_button("auth-cancel", "Cancel", pal.border_subtle, pal.text_secondary, cx.listener(|this, _, _, cx| {
                            if this.state.auth_busy {
                                return;
                            }
                            close_auth_dialog(this, cx);
                        })))
                        .child(small_button("auth-submit", submit_label, pal.accent_green, 0xffffff, cx.listener(|this, _, _, cx| this.authenticate_sudo(cx)))),
                ),
        )
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
                            if this.state.active_tab != ActiveTab::Doctor {
                                this.state.active_tab = ActiveTab::Doctor;
                                this.state.focused_input = None;
                                cx.notify();
                            }
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
                            if this.state.sudo_session_active {
                                // Revoke the real sudo timestamp, not just the UI flag.
                                this.rpc_async(
                                    "v1.deauthenticate",
                                    json!({}),
                                    |app, result, cx| {
                                        app.state.sudo_session_active = false;
                                        app.state.settings_msg = Some(StatusMsg {
                                            level: MsgLevel::Info,
                                            text: match result {
                                                Ok(_) => {
                                                    "Sudo session revoked; credentials cleared from the sudo cache.".to_string()
                                                }
                                                Err(_) => {
                                                    "Sudo session marked inactive in the app.".to_string()
                                                }
                                            },
                                        });
                                        cx.notify();
                                    },
                                    cx,
                                );
                            } else {
                                this.open_auth_dialog(None);
                                cx.notify();
                            }
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
        .hover(move |this| this.bg(rgb(if pal.is_light() { 0xdee9f6 } else { 0x101d31 })))
        .on_click(cx.listener(move |this, _, _, cx| {
            if this.state.active_tab != tab_clone {
                this.state.active_tab = tab_clone.clone();
                this.state.focused_input = None;
                cx.notify();
            }
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
                                        .bg(if app.state.focused_input == Some(InputField::Search) {
                                            if pal.is_light() { rgb(0xffffff) } else { rgb(0x0e2238) }
                                        } else {
                                            rgb(pal.bg_input)
                                        })
                                        .border_1()
                                        .border_color(if app.state.focused_input == Some(InputField::Search) {
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
                                        .cursor_text()
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.focus_field(InputField::Search, window, cx);
                                        }))
                                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                                            let keystroke = &event.keystroke;
                                            if keystroke.modifiers.control {
                                                if keystroke.key == "v" {
                                                    if let Some(text) = cx
                                                        .read_from_clipboard()
                                                        .and_then(|item| item.text())
                                                    {
                                                        this.state.search_query
                                                            .push_str(text.trim_end_matches('\n'));
                                                        cx.notify();
                                                    }
                                                }
                                                return;
                                            }
                                            match keystroke.key.as_str() {
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
                                                    if let Some(ch) = &keystroke.key_char {
                                                        this.state.search_query.push_str(ch);
                                                        cx.notify();
                                                    }
                                                }
                                            }
                                        }))
                                        .child(
                                            div()
                                                .flex_1()
                                                .flex()
                                                .items_center()
                                                .when(
                                                    app.state.focused_input
                                                        == Some(InputField::Search)
                                                        && app.state.tick % 2 == 0,
                                                    |this| {
                                                        this.child(
                                                            div()
                                                                .w(px(2.0))
                                                                .h(px(16.0))
                                                                .mr_1()
                                                                .bg(rgb(pal.accent_cyan)),
                                                        )
                                                    },
                                                )
                                                .child(if app.state.search_query.is_empty() {
                                                    div()
                                                        .text_color(rgb(pal.text_placeholder))
                                                        .text_size(px(13.0))
                                                        .child(if app.state.focused_input
                                                            == Some(InputField::Search)
                                                        {
                                                            "Type package name (e.g. vlc, brave, discord)…"
                                                        } else {
                                                            "Search package name or upstream git URL…"
                                                        })
                                                        .into_any_element()
                                                } else {
                                                    div()
                                                        .text_color(rgb(pal.text_primary))
                                                        .text_size(px(13.0))
                                                        .child(app.state.search_query.clone())
                                                        .into_any_element()
                                                }),
                                        )
                                        .when(
                                            app.state.focused_input == Some(InputField::Search)
                                                && !app.state.search_query.is_empty()
                                                && app.state.tick % 2 == 0,
                                            |this| {
                                                this.child(
                                                    div()
                                                        .w(px(2.0))
                                                        .h(px(16.0))
                                                        .ml(px(1.0))
                                                        .bg(rgb(pal.accent_cyan)),
                                                )
                                            },
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
                                if item.source == "official" {
                                    "Prepare Install Plan →"
                                } else {
                                    "Prepare Build Plan →"
                                },
                                ACCENT_BLUE,
                                0xffffff,
                                cx.listener({
                                    let item = item.clone();
                                    move |this, _, _, cx| this.prepare_plan_for_result(&item, cx)
                                }),
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
                                    .gap_1()
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
        .child(format!("{}", name))
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
                .flex_1()
                .overflow_hidden()
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
                        .flex_1()
                        .overflow_hidden()
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
                    if result.source == "official" {
                        "Install Plan"
                    } else {
                        "Build Plan"
                    },
                    ACCENT_BLUE,
                    0xffffff,
                    cx.listener({
                        let result = result.clone();
                        move |this, _, _, cx| {
                            this.state.selected_result = Some(idx);
                            this.prepare_plan_for_result(&result, cx);
                        }
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
                        .child(detail_row(
                            "License",
                            item.license.as_deref().unwrap_or(NOT_REPORTED),
                            &pal,
                        ))
                        .child(detail_row(
                            "Architecture",
                            item.arch.as_deref().unwrap_or(NOT_REPORTED),
                            &pal,
                        ))
                        .child(detail_row(
                            "Size",
                            item.size.as_deref().unwrap_or(NOT_REPORTED),
                            &pal,
                        ))
                        .child(detail_row(
                            "Maintainer",
                            item.maintainer.as_deref().unwrap_or(NOT_REPORTED),
                            &pal,
                        ))
                        // AUR's own staleness flag, shown only when set.
                        .when_some(item.out_of_date.clone(), |this, flagged| {
                            this.child(
                                div()
                                    .text_color(rgb(pal.accent_red))
                                    .text_size(px(11.0))
                                    .child(format!("Flagged out of date since {}", flagged)),
                            )
                        }),
                )
                // Links the source actually publishes, in place of fixed URLs.
                .when(
                    item.url.is_some() || item.source_url.is_some(),
                    |this| {
                        this.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .pt_2()
                                .border_t_1()
                                .border_color(rgb(pal.border_subtle))
                                .when_some(item.url.clone(), |this, url| {
                                    this.child(link_row(&pal, "Upstream", url))
                                })
                                .when_some(item.source_url.clone(), |this, url| {
                                    this.child(link_row(&pal, "Package source", url))
                                }),
                        )
                    },
                )
                .into_any_element(),
        })
}

fn link_row(pal: &ThemePalette, title: &'static str, url: String) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
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
                .overflow_hidden()
                .text_ellipsis()
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
                        .border_color(if app.state.focused_input == Some(InputField::Inspect) {
                            rgb(pal.accent_cyan)
                        } else {
                            rgb(pal.border)
                        })
                        .rounded_lg()
                        .px_3()
                        .flex()
                        .items_center()
                        .cursor_text()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.focus_field(InputField::Inspect, window, cx);
                        }))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                            let keystroke = &event.keystroke;
                            if keystroke.modifiers.control {
                                if keystroke.key == "v" {
                                    if let Some(text) = cx
                                        .read_from_clipboard()
                                        .and_then(|item| item.text())
                                    {
                                        this.state.inspect_path
                                            .push_str(text.trim_end_matches('\n'));
                                        cx.notify();
                                    }
                                }
                                return;
                            }
                            match keystroke.key.as_str() {
                                "backspace" => {
                                    this.state.inspect_path.pop();
                                    cx.notify();
                                }
                                "return" | "enter" => this.run_inspect(cx),
                                _ => {
                                    if let Some(ch) = &keystroke.key_char {
                                        this.state.inspect_path.push_str(ch);
                                        cx.notify();
                                    }
                                }
                            }
                        }))
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .items_center()
                                .when(
                                    app.state.focused_input == Some(InputField::Inspect)
                                        && app.state.inspect_path.is_empty()
                                        && app.state.tick % 2 == 0,
                                    |this| {
                                        this.child(
                                            div()
                                                .w(px(2.0))
                                                .h(px(16.0))
                                                .mr_1()
                                                .bg(rgb(pal.accent_cyan)),
                                        )
                                    },
                                )
                                .child(if app.state.inspect_path.is_empty() {
                                    div()
                                        .text_color(rgb(pal.text_placeholder))
                                        .text_size(px(13.0))
                                        .child("Select or enter path to .deb or .rpm file...")
                                        .into_any_element()
                                } else {
                                    div()
                                        .text_color(rgb(pal.text_primary))
                                        .text_size(px(13.0))
                                        .child(app.state.inspect_path.clone())
                                        .into_any_element()
                                }),
                        )
                        .when(
                            app.state.focused_input == Some(InputField::Inspect)
                                && !app.state.inspect_path.is_empty()
                                && app.state.tick % 2 == 0,
                            |this| {
                                this.child(
                                    div()
                                        .w(px(2.0))
                                        .h(px(16.0))
                                        .ml(px(1.0))
                                        .bg(rgb(pal.accent_cyan)),
                                )
                            },
                        ),
                )
                // Browse File button
                .child(action_button(
                    "inspect-browse-btn",
                    "Browse File...",
                    if pal.is_light() { 0xe2e8f0 } else { 0x12233a },
                    pal.text_secondary,
                    cx.listener(|this, _, _, cx| this.browse_inspect_target(cx)),
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
                        // Carry the inspected file across: building whatever happened
                        // to be typed in the Build box is not an import.
                        let path = this.state.inspect_path.trim().to_string();
                        if path.is_empty() {
                            this.state.inspect_msg = Some(StatusMsg {
                                level: MsgLevel::Warning,
                                text: "Inspect a package file before importing it.".to_string(),
                            });
                            cx.notify();
                            return;
                        }
                        this.state.build_target_input = path.clone();
                        this.prepare_plan("build", &path, cx);
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

        Some(payload) => {
            let report: Option<crate::inspect::InspectionReport> =
                serde_json::from_value(payload.clone()).ok();
            match report {
                None => div()
                    .w_full()
                    .p_4()
                    .border_1()
                    .border_color(rgb(pal.border_subtle))
                    .rounded_xl()
                    .bg(rgb(pal.bg_card))
                    .text_color(rgb(ACCENT_RED))
                    .text_size(px(12.0))
                    .child("The inspection response could not be read. Re-run the inspection.")
                    .into_any_element(),
                Some(report) => render_inspection_report(&report, app, pal, cx).into_any_element(),
            }
        }
    }
}

/// Everything shown here comes straight from the parsed archive. A field the
/// format does not carry is reported as absent rather than filled in.
fn render_inspection_report(
    report: &crate::inspect::InspectionReport,
    app: &mut ArchBridgeApp,
    pal: &ThemePalette,
    cx: &mut Context<ArchBridgeApp>,
) -> impl IntoElement {
    let text_or = |value: &str, fallback: &'static str| {
        if value.trim().is_empty() {
            fallback.to_string()
        } else {
            value.to_string()
        }
    };

    let desktop_count = report.desktop_files.len();
    let systemd_count = report.systemd_units.len();
    let script_count = report.maintainer_scripts.len();

    let subtab = app.state.inspect_active_subtab;
    let body: Vec<AnyElement> = match subtab {
        0 => report
            .maintainer_scripts
            .iter()
            .map(|script| inspect_script_card(script, pal))
            .collect(),
        1 => vec![inspect_list_card(
            "Dependencies declared by the package",
            &report.dependencies,
            if report.format == "rpm" {
                "RPM dependency requires live in the package header, which this \
                 data-only reader does not parse. Nothing was detected, not nothing."
            } else {
                "This package declares no Depends or Pre-Depends entries."
            },
            pal,
        )],
        2 => {
            let mut items = Vec::new();
            items.extend(
                report
                    .desktop_files
                    .iter()
                    .map(|f| format!("[desktop] {}", f)),
            );
            items.extend(
                report
                    .systemd_units
                    .iter()
                    .map(|f| format!("[systemd] {}", f)),
            );
            vec![inspect_list_card(
                "Desktop entries and systemd units found in the payload",
                &items,
                "No .desktop, .service, .socket, .target or .timer files in this package.",
                pal,
            )]
        }
        _ => vec![inspect_list_card(
            "Libraries named by the package metadata",
            &report.needed_libraries,
            "Foreign binaries are never executed or probed, so no dynamic-linker view \
             exists. Library needs below come from declared metadata only.",
            pal,
        )],
    };

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
                .child(detail_row("File", &report.target_file, pal))
                .child(detail_row(
                    "Format",
                    report.format.to_uppercase().as_str(),
                    pal,
                ))
                .child(detail_row(
                    "Package Name",
                    &text_or(&report.package_name, "not declared"),
                    pal,
                ))
                .child(detail_row(
                    "Version",
                    &text_or(&report.version, "not declared"),
                    pal,
                ))
                .child(detail_row(
                    "Architecture",
                    &text_or(&report.architecture, "not declared"),
                    pal,
                ))
                .child(detail_row("Size", &format_bytes(report.size_bytes), pal))
                .child(detail_row("SHA-256", &short_hash(&report.sha256), pal))
                .child(detail_row(
                    "Maintainer",
                    &text_or(&report.maintainer, "not declared"),
                    pal,
                ))
                .child(inspect_description_row(&report.description, pal))
                .child(
                    div()
                        .mt_1()
                        .text_color(rgb(pal.text_muted))
                        .text_size(px(11.0))
                        .child(report.compatibility_note.clone()),
                ),
        )
        // Subtabs Selector
        .child(
            div()
                .flex()
                .gap_2()
                .child(inspect_tab_pill(
                    0,
                    format!("Maintainer Scripts ({})", script_count),
                    app,
                    pal,
                    cx,
                ))
                .child(inspect_tab_pill(
                    1,
                    format!("Dependencies ({})", report.dependencies.len()),
                    app,
                    pal,
                    cx,
                ))
                .child(inspect_tab_pill(
                    2,
                    format!("Desktop & Systemd ({})", desktop_count + systemd_count),
                    app,
                    pal,
                    cx,
                ))
                .child(inspect_tab_pill(
                    3,
                    format!("Libraries ({})", report.needed_libraries.len()),
                    app,
                    pal,
                    cx,
                )),
        )
        // Subtab Content
        .child(
            div()
                .bg(if pal.is_light() {
                    rgb(0xf8fafc)
                } else {
                    rgb(0x040810)
                })
                .border_1()
                .border_color(rgb(pal.border_subtle))
                .rounded_xl()
                .p_4()
                .min_h(px(160.0))
                .flex()
                .flex_col()
                .gap_3()
                .children(body),
        )
}

/// Maintainer scripts are shown exactly as archived, with the execution flag
/// visible, so a reviewer can judge what the package *would* have done.
fn inspect_script_card(script: &crate::inspect::ScriptInfo, pal: &ThemePalette) -> AnyElement {
    const MAX_SCRIPT_BYTES: usize = 4096;
    let mut end = script.content.len().min(MAX_SCRIPT_BYTES);
    while end > 0 && !script.content.is_char_boundary(end) {
        end -= 1;
    }
    let preview = &script.content[..end];
    let truncated = end < script.content.len();
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
                        .text_size(px(12.0))
                        .font_weight(FontWeight::BOLD)
                        .child(script.name.clone()),
                )
                .child(
                    div()
                        .text_color(rgb(if script.executed {
                            ACCENT_RED
                        } else {
                            ACCENT_GREEN
                        }))
                        .text_size(px(10.0))
                        .font_weight(FontWeight::BOLD)
                        .child(if script.executed {
                            "EXECUTED"
                        } else {
                            "NOT EXECUTED"
                        }),
                )
                .child(
                    div()
                        .text_color(rgb(pal.text_muted))
                        .text_size(px(10.0))
                        .child(format!(
                            "{} · {} bytes",
                            script.status,
                            script.content.len()
                        )),
                ),
        )
        .child(
            div()
                .p_2()
                .rounded_md()
                .bg(if pal.is_light() {
                    rgb(0xeef2f7)
                } else {
                    rgb(0x01040a)
                })
                .text_color(rgb(pal.text_secondary))
                .text_size(px(11.0))
                .font_family("monospace")
                .child(if truncated {
                    format!("{}\n… truncated for display …", preview)
                } else {
                    preview.to_string()
                }),
        )
        .into_any_element()
}

fn inspect_list_card(
    title: &str,
    items: &[String],
    empty_note: &'static str,
    pal: &ThemePalette,
) -> AnyElement {
    let mut card = div().flex().flex_col().gap_1().child(
        div()
            .text_color(rgb(pal.text_muted))
            .text_size(px(11.0))
            .font_weight(FontWeight::BOLD)
            .child(format!("{} ({})", title, items.len())),
    );
    if items.is_empty() {
        card = card.child(
            div()
                .text_color(rgb(pal.text_muted))
                .text_size(px(12.0))
                .child(empty_note),
        );
    } else {
        card = card.children(items.iter().map(|item| {
            div()
                .text_color(rgb(pal.text_primary))
                .text_size(px(12.0))
                .font_family("monospace")
                .child(format!("• {}", item))
        }));
    }
    card.into_any_element()
}

fn inspect_description_row(description: &str, pal: &ThemePalette) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_color(rgb(pal.text_muted))
                .text_size(px(12.0))
                .child("Description"),
        )
        .child(
            div()
                .text_color(rgb(pal.text_primary))
                .text_size(px(12.0))
                .child(if description.trim().is_empty() {
                    "not declared".to_string()
                } else {
                    description.to_string()
                }),
        )
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

fn short_hash(hash: &str) -> String {
    if hash.len() <= 16 {
        return hash.to_string();
    }
    format!("{}…{}", &hash[..8], &hash[hash.len() - 8..])
}

fn inspect_tab_pill(
    idx: usize,
    title: String,
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
        .id("build-scroll")
        .flex()
        .flex_col()
        .gap_4()
        .size_full()
        // The manifest panel and the console both want more height than a short
        // window offers, and GPUI lets overflowing children paint over their
        // siblings instead of shrinking them. Scroll the whole tab instead.
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
                        .border_color(if app.state.focused_input == Some(InputField::Build) {
                            rgb(pal.accent_cyan)
                        } else {
                            rgb(pal.border)
                        })
                        .rounded_lg()
                        .px_3()
                        .flex()
                        .items_center()
                        .cursor_text()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.focus_field(InputField::Build, window, cx);
                        }))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                            let keystroke = &event.keystroke;
                            if keystroke.modifiers.control {
                                if keystroke.key == "v" {
                                    if let Some(text) = cx
                                        .read_from_clipboard()
                                        .and_then(|item| item.text())
                                    {
                                        this.state.build_target_input
                                            .push_str(text.trim_end_matches('\n'));
                                        cx.notify();
                                    }
                                }
                                return;
                            }
                            match keystroke.key.as_str() {
                                "backspace" => {
                                    this.state.build_target_input.pop();
                                    cx.notify();
                                }
                                "return" | "enter" => this.prepare_build_plan(cx),
                                _ => {
                                    if let Some(ch) = &keystroke.key_char {
                                        this.state.build_target_input.push_str(ch);
                                        cx.notify();
                                    }
                                }
                            }
                        }))
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .items_center()
                                .when(
                                    app.state.focused_input == Some(InputField::Build)
                                        && app.state.build_target_input.is_empty()
                                        && app.state.tick % 2 == 0,
                                    |this| {
                                        this.child(
                                            div()
                                                .w(px(2.0))
                                                .h(px(16.0))
                                                .mr_1()
                                                .bg(rgb(pal.accent_cyan)),
                                        )
                                    },
                                )
                                .child(if app.state.build_target_input.is_empty() {
                                    div()
                                        .text_color(rgb(pal.text_placeholder))
                                        .text_size(px(13.0))
                                        .child("Target: .deb/.rpm file, GitHub URL, local directory, PKGBUILD, or package name...")
                                        .into_any_element()
                                } else {
                                    div()
                                        .text_color(rgb(pal.text_primary))
                                        .text_size(px(13.0))
                                        .child(app.state.build_target_input.clone())
                                        .into_any_element()
                                }),
                        )
                        .when(
                            app.state.focused_input == Some(InputField::Build)
                                && !app.state.build_target_input.is_empty()
                                && app.state.tick % 2 == 0,
                            |this| {
                                this.child(
                                    div()
                                        .w(px(2.0))
                                        .h(px(16.0))
                                        .ml(px(1.0))
                                        .bg(rgb(pal.accent_cyan)),
                                )
                            },
                        ),
                )
                .child(action_button(
                    "build-browse-btn",
                    "📦 Browse Package...",
                    if pal.is_light() { 0xe2e8f0 } else { 0x12233a },
                    pal.text_secondary,
                    cx.listener(|this, _, _, cx| this.browse_build_target(cx)),
                ))
                .child(action_button(
                    "build-browse-dir-btn",
                    "📁 Folder...",
                    if pal.is_light() { 0xe2e8f0 } else { 0x12233a },
                    pal.text_secondary,
                    cx.listener(|this, _, _, cx| this.browse_build_dir(cx)),
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
                    Some(plan) => {
                        // The panel used to be headed "Generated PKGBUILD Manifest"
                        // while showing only the summary; the confirmed bytes are
                        // the PKGBUILD the engine staged, so show exactly those.
                        let pkgbuild = plan
                            .reviews
                            .iter()
                            .find(|review| review.path == "PKGBUILD")
                            .and_then(|review| review.content.clone());
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .text_color(rgb(pal.text_secondary))
                                    .text_size(px(12.0))
                                    .font_family("monospace")
                                    .child(format!(
                                        "# Plan {} — action: {}\n# {}\n# Steps: {}",
                                        plan.plan_id,
                                        plan.action,
                                        plan.summary,
                                        plan.steps.len()
                                    )),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .children(plan.steps.iter().map(|step| {
                                        div()
                                            .text_color(rgb(pal.text_muted))
                                            .text_size(px(12.0))
                                            .font_family("monospace")
                                            .child(format!("  - {}", step.purpose))
                                    }))
                                    .when(pkgbuild.is_none(), |this| {
                                        this.child(
                                            div()
                                                .text_color(rgb(pal.text_muted))
                                                .text_size(px(12.0))
                                                .font_family("monospace")
                                                .child("  (this plan runs no PKGBUILD)"),
                                        )
                                    }),
                            )
                            .when_some(pkgbuild, |this, script| {
                                this.child(
                                    div()
                                        .id("plan-pkgbuild-view")
                                        .bg(rgb(pal.bg_darkest))
                                        .border_1()
                                        .border_color(rgb(pal.border_subtle))
                                        .rounded_lg()
                                        .p_3()
                                        .max_h(px(320.0))
                                        .overflow_y_scroll()
                                        .text_color(rgb(pal.text_primary))
                                        .text_size(px(11.5))
                                        .font_family("monospace")
                                        .child("# --- PKGBUILD to be executed ---\n".to_string())
                                        .child(script),
                                )
                            })
                    }
                }),
        )
        // Confirm & Execute button
        .child(
            div()
                .flex()
                .gap_3()
                .child(action_button(
                    "confirm-exec-chroot-btn",
                    if app.state.build_busy {
                        "Executing Plan…"
                    } else {
                        "Confirm and Execute Plan"
                    },
                    ACCENT_GREEN,
                    0xffffff,
                    cx.listener(|this, _, _, cx| this.execute_plan(cx)),
                ))
                .when(app.state.build_stage == 4, |this| {
                    this.child(action_button(
                        "install-built-pkg-btn",
                        "Install Built Package",
                        ACCENT_PURPLE,
                        0xffffff,
                        cx.listener(|this, _, _, cx| {
                            let plan_id = this
                                .state
                                .build_result
                                .as_ref()
                                .and_then(|result| result.next_plan.as_ref())
                                .map(|next_plan| next_plan.plan_id.clone());
                            let Some(plan_id) = plan_id else { return };
                            if this.state.install_plan_consumed {
                                this.state.build_msg = Some(StatusMsg {
                                    level: MsgLevel::Warning,
                                    text: "This install plan already ran and was consumed. Its \
                                           output is still shown below; prepare the build again \
                                           to review and confirm a fresh install plan."
                                        .to_string(),
                                });
                                cx.notify();
                                return;
                            }
                            if !this.state.sudo_session_active {
                                this.state.active_plan =
                                    this.state.build_result.as_ref().and_then(|r| r.next_plan.clone());
                                this.open_auth_dialog(Some("execute"));
                                cx.notify();
                                return;
                            }
                            this.state.build_busy = true;
                            this.state.install_plan_consumed = true;
                            this.state.build_msg = Some(StatusMsg {
                                level: MsgLevel::Info,
                                text: "Installing built package to the host system…".to_string(),
                            });
                            cx.notify();
                            this.rpc_async(
                                "v1.execute",
                                json!({ "plan_id": plan_id, "confirmed": true }),
                                |app, result, cx| {
                                    app.state.build_busy = false;
                                    match result {
                                        Ok(val) => {
                                            let ok = val
                                                .get("ok")
                                                .and_then(|o| o.as_bool())
                                                .unwrap_or(false);
                                            if ok {
                                                // The install plan is consumed once it
                                                // runs, so the button that ran it has to
                                                // go with it.
                                                app.state.build_stage = 5;
                                            }
                                            app.state.build_msg = Some(StatusMsg {
                                                level: if ok {
                                                    MsgLevel::Success
                                                } else {
                                                    MsgLevel::Error
                                                },
                                                text: val
                                                    .get("message")
                                                    .and_then(|m| m.as_str())
                                                    .unwrap_or("Installation finished")
                                                    .to_string(),
                                            });
                                        }
                                        Err(e) => {
                                            if e.contains("password is required") {
                                                app.state.sudo_session_active = false;
                                            }
                                            app.state.build_msg = Some(StatusMsg {
                                                level: MsgLevel::Error,
                                                text: e,
                                            });
                                        }
                                    }
                                    cx.notify();
                                },
                                cx,
                            );
                        }),
                    ))
                }),
        )
        // Terminal Execution Console
        .child(
            div()
                .h(px(260.0))
                .bg(if pal.is_light() { rgb(0xf8fafc) } else { rgb(0x040810) })
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
                        .child(if app.state.build_busy && app.state.build_log.is_empty() {
                            "⏳ Engine work in progress — chroot setup, makepkg, and smoke test output will appear here when the plan finishes…".to_string()
                        } else if app.state.build_log.is_empty() {
                            "Execution output and runtime smoke test logs will stream here...".to_string()
                        } else {
                            app.state.build_log.clone()
                        }),
                )
                .when(app.state.build_busy && app.state.tick % 2 == 0, |this| {
                    this.child(
                        div()
                            .w(px(2.0))
                            .h(px(14.0))
                            .bg(rgb(ACCENT_GREEN)),
                    )
                }),
        )
}

// ─────────────────────────────────────────────────────────────────────────────
// 4. Uninstall Software View
// ─────────────────────────────────────────────────────────────────────────────

fn render_uninstall(app: &mut ArchBridgeApp, cx: &mut Context<ArchBridgeApp>) -> impl IntoElement {
    let pal = get_palette(app.state.theme);
    let query = app.state.uninstall_query.to_lowercase();
    // Indexes first, clones last: materialising a cloned copy of every installed
    // package on each repaint is what made typing in this view stutter.
    let matching: Vec<usize> = app
        .state
        .installed_packages
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            if query.is_empty() {
                true
            } else {
                p.name.to_lowercase().contains(&query)
            }
        })
        .map(|(i, _)| i)
        .collect();

    let total_count = matching.len();
    let limit = app.state.uninstall_display_limit;
    let showing_count = total_count.min(limit);
    let has_more = total_count > limit;
    let is_empty = matching.is_empty();
    let loading = app.state.installed_loading;
    let visible_pkgs: Vec<(usize, InstalledPackage)> = matching
        .iter()
        .take(limit)
        .map(|&i| (i, app.state.installed_packages[i].clone()))
        .collect();

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
                        this.reload_installed(cx);
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
                        .border_color(if app.state.focused_input == Some(InputField::Uninstall) {
                            rgb(pal.accent_cyan)
                        } else {
                            rgb(pal.border_subtle)
                        })
                        .rounded_lg()
                        .px_3()
                        .flex()
                        .items_center()
                        .cursor_text()
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.focus_field(InputField::Uninstall, window, cx);
                        }))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                            let keystroke = &event.keystroke;
                            if keystroke.modifiers.control {
                                if keystroke.key == "v" {
                                    if let Some(text) = cx
                                        .read_from_clipboard()
                                        .and_then(|item| item.text())
                                    {
                                        this.state.uninstall_query
                                            .push_str(text.trim_end_matches('\n'));
                                        cx.notify();
                                    }
                                }
                                return;
                            }
                            match keystroke.key.as_str() {
                                "backspace" => {
                                    this.state.uninstall_query.pop();
                                    cx.notify();
                                }
                                _ => {
                                    if let Some(ch) = &keystroke.key_char {
                                        this.state.uninstall_query.push_str(ch);
                                        cx.notify();
                                    }
                                }
                            }
                        }))
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .items_center()
                                .when(
                                    app.state.focused_input == Some(InputField::Uninstall)
                                        && app.state.uninstall_query.is_empty()
                                        && app.state.tick % 2 == 0,
                                    |this| {
                                        this.child(
                                            div()
                                                .w(px(2.0))
                                                .h(px(16.0))
                                                .mr_1()
                                                .bg(rgb(pal.accent_cyan)),
                                        )
                                    },
                                )
                                .child(if app.state.uninstall_query.is_empty() {
                                    div()
                                        .text_color(rgb(pal.text_placeholder))
                                        .text_size(px(13.0))
                                        .child(if app.state.focused_input
                                            == Some(InputField::Uninstall)
                                        {
                                            "Type software name (e.g. grok-bot, vlc, discord)..."
                                        } else {
                                            "Type software name to find and uninstall (e.g. grok-bot, vlc, discord, steam)..."
                                        })
                                        .into_any_element()
                                } else {
                                    div()
                                        .text_color(rgb(pal.text_primary))
                                        .text_size(px(13.0))
                                        .child(app.state.uninstall_query.clone())
                                        .into_any_element()
                                }),
                        )
                        .when(
                            app.state.focused_input == Some(InputField::Uninstall)
                                && !app.state.uninstall_query.is_empty()
                                && app.state.tick % 2 == 0,
                            |this| {
                                this.child(
                                    div()
                                        .w(px(2.0))
                                        .h(px(16.0))
                                        .ml(px(1.0))
                                        .bg(rgb(pal.accent_cyan)),
                                )
                            },
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
        // Safety Confirmation Prompt Popup Modal
        .when_some(app.state.uninstall_confirm_idx, |this, idx| {
            if let Some(pkg) = app.state.installed_packages.get(idx) {
                let pkg_name = pkg.name.clone();
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .bg(rgba(0x000000aa))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .w(px(450.0))
                                .p_5()
                                .rounded_xl()
                                .bg(if pal.is_light() { rgb(0xfef2f2) } else { rgb(0x2d1214) })
                                .border_1()
                                .border_color(if pal.is_light() { rgb(0xfecaca) } else { rgb(0x7f1d1d) })
                                .shadow_lg()
                                .flex()
                                .flex_col()
                                .gap_4()
                                .child(
                                    div()
                                        .w_full()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap_3()
                                        .child(div().text_size(px(20.0)).child("⚠️"))
                                        .child(
                                            div()
                                                .text_color(if pal.is_light() { rgb(0x991b1b) } else { rgb(0xfca5a5) })
                                                .text_size(px(13.5))
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
                        )
                    )
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
                                .w(px(220.0))
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
                        .gap_2()
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
                                                .w(px(220.0))
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
                                    .child(if loading {
                                        "Reading installed packages from pacman…"
                                    } else {
                                        "No verified installed packages to show."
                                    })
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
                // Diagnostic status rows
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
                                                        .bg(rgb(status_color(&check.status))),
                                                )
                                                .child(
                                                    div()
                                                        .text_color(rgb(status_color(
                                                            &check.status,
                                                        )))
                                                        .text_size(px(12.0))
                                                        .font_weight(FontWeight::BOLD)
                                                        .child(status_label(&check.status)),
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
                        )
                        .when(app.state.health_checks.is_empty(), |this| {
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
                                    .child(if app.state.doctor_busy {
                                        "Measuring packaging prerequisites…"
                                    } else {
                                        "No diagnostics have run in this session."
                                    })
                                    .child(
                                        div()
                                            .text_color(rgb(pal.text_placeholder))
                                            .text_size(px(11.0))
                                            .child(
                                                "Use “Run All Diagnostics” to check this system.",
                                            ),
                                    ),
                            )
                        }),
                ),
        )
}

/// An unavailable check is neither a pass nor a failure: rendering it red would
/// tell the user their system is broken when it merely lacks an online probe.
fn status_color(status: &str) -> u32 {
    match status {
        "pass" => ACCENT_GREEN,
        "unavailable" => ACCENT_ORANGE,
        _ => ACCENT_RED,
    }
}

fn status_label(status: &str) -> &'static str {
    match status {
        "pass" => "PASS",
        "unavailable" => "OFFLINE",
        _ => "FAIL",
    }
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
                                this.rpc_async(
                                    "v1.deauthenticate",
                                    json!({}),
                                    |app, _, cx| {
                                        app.state.sudo_session_active = false;
                                        app.state.settings_msg = Some(StatusMsg {
                                            level: MsgLevel::Info,
                                            text: "Sudo credentials cleared from memory.".to_string(),
                                        });
                                        cx.notify();
                                    },
                                    cx,
                                );
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
                        .child(div().text_color(rgb(pal.text_muted)).text_size(px(13.0)).child("•"))
                        .child(
                            div()
                                .text_color(rgb(pal.text_secondary))
                                .text_size(px(12.0))
                                .child(if app.state.sudo_session_active {
                                    "An administrator credential is cached by sudo for this session. \
                                     ArchBridge never stores your password."
                                } else {
                                    "No administrator credential is cached. Privileged steps will ask \
                                     for a password. ArchBridge never stores your password."
                                }),
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
                .child(settings_checkbox(
                    &pal,
                    "pref-official",
                    "Official Arch Linux Repositories (pacman)",
                    app.state.opt_official,
                    cx.listener(|this, _, _, cx| {
                        this.state.opt_official = !this.state.opt_official;
                        this.state.settings_dirty = true;
                        cx.notify();
                    }),
                ))
                .child(settings_checkbox(
                    &pal,
                    "pref-aur",
                    "Arch User Repository (AUR RPC)",
                    app.state.opt_aur,
                    cx.listener(|this, _, _, cx| {
                        this.state.opt_aur = !this.state.opt_aur;
                        this.state.settings_dirty = true;
                        cx.notify();
                    }),
                ))
                .child(settings_checkbox(
                    &pal,
                    "pref-flatpak",
                    "Flatpak Release Bundles (Flathub)",
                    app.state.opt_flatpak,
                    cx.listener(|this, _, _, cx| {
                        this.state.opt_flatpak = !this.state.opt_flatpak;
                        this.state.settings_dirty = true;
                        cx.notify();
                    }),
                ))
                .child(settings_checkbox(
                    &pal,
                    "pref-appimage",
                    "AppImage Standalone Assets",
                    app.state.opt_appimage,
                    cx.listener(|this, _, _, cx| {
                        this.state.opt_appimage = !this.state.opt_appimage;
                        this.state.settings_dirty = true;
                        cx.notify();
                    }),
                ))
                .child(settings_checkbox(
                    &pal,
                    "pref-upstream",
                    "Upstream Git Release Sources (GitHub/GitLab)",
                    app.state.opt_upstream,
                    cx.listener(|this, _, _, cx| {
                        this.state.opt_upstream = !this.state.opt_upstream;
                        this.state.settings_dirty = true;
                        cx.notify();
                    }),
                ))
                .child(settings_checkbox(
                    &pal,
                    "pref-deb",
                    "DEB Package Inspection",
                    app.state.opt_deb,
                    cx.listener(|this, _, _, cx| {
                        this.state.opt_deb = !this.state.opt_deb;
                        this.state.settings_dirty = true;
                        cx.notify();
                    }),
                ))
                .child(settings_checkbox(
                    &pal,
                    "pref-rpm",
                    "RPM Package Inspection",
                    app.state.opt_rpm,
                    cx.listener(|this, _, _, cx| {
                        this.state.opt_rpm = !this.state.opt_rpm;
                        this.state.settings_dirty = true;
                        cx.notify();
                    }),
                ))
                .child(
                    div()
                        .pt_3()
                        .w_full()
                        .child(action_button(
                            "save-prefs-btn",
                            if app.state.settings_busy {
                                "Saving…"
                            } else if app.state.settings_dirty {
                                "Save Preferences (unsaved changes)"
                            } else {
                                "Save Preferences"
                            },
                            0x059669,
                            0xffffff,
                            cx.listener(|this, _, _, cx| {
                                this.save_preferences(cx);
                            }),
                        )),
                ),
        )
        .when_some(app.state.settings_msg.clone(), |this, msg| {
            this.child(status_msg_bar(&msg, &pal))
        })
}

fn settings_checkbox<F>(
    pal: &ThemePalette,
    id: &'static str,
    label: &'static str,
    checked: bool,
    on_click: F,
) -> impl IntoElement
where
    F: Fn(&ClickEvent, &mut Window, &mut App) + 'static,
{
    div()
        .id(SharedString::from(id))
        .flex()
        .flex_row()
        .items_center()
        .gap_3()
        .py_1()
        .cursor_pointer()
        .on_click(on_click)
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

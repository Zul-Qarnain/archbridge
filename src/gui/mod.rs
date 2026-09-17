pub mod app;
pub mod state;
pub mod theme;

use app::ArchBridgeApp;
use gpui::*;
use state::SharedEngine;
use std::sync::{Arc, Mutex};

/// Launch the native GPUI window for ArchBridge.
/// This replaces the old Python archbridge-gui.py launcher.
pub fn run_gui() {
    let engine: SharedEngine = Arc::new(Mutex::new(crate::rpc::RpcServer::new()));

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1280.0), px(820.0)), cx);
        let engine_clone = Arc::clone(&engine);

        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("ArchBridge — Software Discovery & Packaging".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |_, cx| {
                cx.new(|cx| ArchBridgeApp::new(Arc::clone(&engine_clone), cx))
            },
        )
        .unwrap();

        cx.activate(true);
    });
}

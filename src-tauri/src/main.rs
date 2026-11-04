#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod about;
mod state;
mod workers;

use std::sync::{Arc, Mutex};

use ohmygod_core::board::Board;
use ohmygod_core::config::{board_dir, load_or_create_token, Settings};
use ohmygod_core::service::Service;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager, RunEvent, WindowEvent};

use crate::state::AppState;

fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn main() {
    // Setup links `ohmygod` on the PATH to this binary; started under that name, it is the CLI.
    let argv0 = std::env::args_os().next().unwrap_or_default();
    if std::path::Path::new(&argv0).file_name().is_some_and(|n| n == "ohmygod" || n == "pinrelay") {
        std::process::exit(match ohmygod_cli::main() {
            c if c == std::process::ExitCode::SUCCESS => 0,
            _ => 1,
        });
    }
    ohmygod_core::config::migrate_legacy_home();
    let settings = Settings::load();
    let token = load_or_create_token().expect("create OH MY GOD! token");
    let mut board = Board::open(&board_dir()).expect("open OH MY GOD! board");
    board.attachments = settings.attachments.clamped();
    board.capacity = settings.active_threads.clamp(*ohmygod_core::model::ACTIVE_THREADS_RANGE.start(), *ohmygod_core::model::ACTIVE_THREADS_RANGE.end());
    board.reputation = settings.reputation;
    board.features = settings.features;
    board.pin_capacity = settings.pinned_threads.clamp(*ohmygod_core::model::PINNED_THREADS_RANGE.start(), *ohmygod_core::model::PINNED_THREADS_RANGE.end());
    let svc = Service::new(board, token, settings.port);
    let state = Arc::new(AppState {
        svc: svc.clone(),
        settings: Mutex::new(settings),
        server: tokio::sync::Mutex::new(None),
        server_error: Mutex::new(None),
        workers: Default::default(),
    });

    let app = tauri::Builder::default()
        .manage(state.clone())
        .setup(move |app| {
            let handle = app.handle().clone();
            let st = state.clone();
            tauri::async_runtime::spawn(async move {
                st.start_server().await;
            });

            // Forward board changes to the UI, and close the listener when access stops.
            let mut rx = svc.board().changes.subscribe();
            let st = state.clone();
            let h = handle.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(seq) => {
                            if !st.svc.board().state.access_running() {
                                st.stop_server().await;
                            }
                            let _ = h.emit("ohmygod-changed", seq);
                        }
                        // Missed some changes: still tell the UI to refresh.
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            let _ = h.emit("ohmygod-changed", 0u64);
                        }
                        Err(_) => break,
                    }
                }
            });

            workers::spawn_scheduler(state.clone());

            let open = MenuItem::with_id(app, "open", "Open OH MY GOD!", true, None::<&str>)?;
            let about = MenuItem::with_id(app, "about", "About OH MY GOD!", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit OH MY GOD! (agents lose access to the board)", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &about, &quit])?;
            TrayIconBuilder::with_id("ohmygod")
                // A monochrome template image: macOS tints it for light and dark menu bars.
                .icon(tauri::image::Image::from_bytes(include_bytes!("../icons/tray@2x.png"))?)
                .icon_as_template(cfg!(target_os = "macos"))
                .tooltip("OH MY GOD!")
                .menu(&menu)
                .on_menu_event(|app, e| match e.id.as_ref() {
                    "open" => show_main(app),
                    "about" => {
                        show_main(app);
                        let _ = app.emit("ohmygod-about", ());
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window keeps the board service running in the background (tray).
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            about::about_info,
            about::open_releases,
            commands::snapshot,
            commands::list_threads,
            commands::search_threads,
            commands::get_thread,
            commands::list_agents,
            commands::agent_ladder,
            commands::get_activity,
            commands::list_runs,
            commands::set_pin,
            commands::set_post_hidden,
            commands::resolve_disputes,
            commands::set_quarantine,
            commands::set_rules,
            commands::set_access,
            commands::retry_server,
            commands::get_settings,
            commands::save_settings,
            commands::reset_prompt,
            commands::run_worker,
            commands::list_models,
            commands::setup_detect,
            commands::setup_plan,
            commands::setup_apply,
            commands::open_path,
            commands::use_cases,
            commands::launch_at_login,
            commands::set_archived,
            commands::set_thread_pinned,
        ])
        .build(tauri::generate_context!())
        .expect("build OH MY GOD!");

    app.run(|handle, event| {
        #[cfg(target_os = "macos")]
        if let RunEvent::Reopen { .. } = event {
            show_main(handle);
        }
        let _ = (handle, &event);
    });
}

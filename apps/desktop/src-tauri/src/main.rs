// Prevents an extra console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use rowglow_lib::error::AppError;
use rowglow_lib::state::AppState;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::menu::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Emitter, Manager, RunEvent, WindowEvent};

/// Set by the UI while any table has staged (uncommitted) edits, so quitting
/// or closing the window asks first instead of silently discarding them.
struct Unsaved(AtomicBool);

fn has_unsaved(app: &AppHandle) -> bool {
    app.state::<Unsaved>().0.load(Ordering::Relaxed)
}

/// Quit, unless there are staged edits: then ask the UI to confirm first.
fn request_quit(app: &AppHandle) {
    if has_unsaved(app) {
        let _ = app.emit("confirm-quit", ());
    } else {
        app.exit(0);
    }
}

#[tauri::command]
fn set_unsaved(state: tauri::State<'_, Unsaved>, dirty: bool) {
    state.0.store(dirty, Ordering::Relaxed);
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

#[tauri::command]
async fn rpc(
    state: tauri::State<'_, Arc<AppState>>,
    method: String,
    params: Value,
) -> Result<Value, AppError> {
    rowglow_lib::rpc::dispatch(state.inner(), &method, params).await
}

#[cfg(target_os = "macos")]
fn build_menu(app: &tauri::AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let about = AboutMetadata {
        name: Some("Rowglow".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        copyright: Some("© 2026 Rowglow".into()),
        comments: Some("Ask your data anything.".into()),
        ..Default::default()
    };
    let app_menu = Submenu::with_items(
        app,
        "Rowglow",
        true,
        &[
            &PredefinedMenuItem::about(app, Some("About Rowglow"), Some(about))?,
            &MenuItem::with_id(app, "updates", "Check for Updates…", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "settings", "Settings…", true, Some("CmdOrCtrl+,"))?,
            &MenuItem::with_id(app, "license", "License…", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            // Custom Quit (⌘Q) so staged edits can be confirmed before exiting.
            &MenuItem::with_id(app, "quit", "Quit Rowglow", true, Some("CmdOrCtrl+Q"))?,
        ],
    )?;
    // App actions have no native accelerators: shortcuts are handled by the
    // web UI so they work identically on every platform (⌘T, ⌘W, ⌘K, …).
    let file_menu = Submenu::with_items(
        app,
        "File",
        true,
        &[
            &MenuItem::with_id(app, "new-connection", "New Connection…", true, None::<&str>)?,
            &MenuItem::with_id(app, "new-query", "New Query Tab", true, None::<&str>)?,
            &MenuItem::with_id(app, "open-sql", "Open SQL File…", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "radar", "Scan for Local Databases", true, None::<&str>)?,
        ],
    )?;
    let edit_menu = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;
    let view_menu = Submenu::with_items(
        app,
        "View",
        true,
        &[
            &MenuItem::with_id(app, "palette", "Open Anything…", true, None::<&str>)?,
            &MenuItem::with_id(app, "toggle-sidebar", "Toggle Sidebar", true, None::<&str>)?,
            &MenuItem::with_id(app, "toggle-inspector", "Toggle Inspector", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::fullscreen(app, None)?,
        ],
    )?;
    let window_menu = Submenu::with_items(
        app,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::maximize(app, None)?,
        ],
    )?;
    let help_menu = Submenu::with_items(
        app,
        "Help",
        true,
        &[
            &MenuItem::with_id(app, "shortcuts", "Keyboard Shortcuts", true, None::<&str>)?,
            &MenuItem::with_id(app, "website", "Rowglow Website", true, None::<&str>)?,
        ],
    )?;
    Menu::with_items(
        app,
        &[&app_menu, &file_menu, &edit_menu, &view_menu, &window_menu, &help_menu],
    )
}

/// Windows / Linux: a conventional File / View / Help menu bar under the native
/// title bar. No Edit menu and no accelerators: the webview handles clipboard
/// keys itself and the web UI handles every shortcut, and native accelerators
/// would swallow those keys before they reach the page.
#[cfg(not(target_os = "macos"))]
fn build_menu(app: &tauri::AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let about = AboutMetadata {
        name: Some("Rowglow".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        copyright: Some("© 2026 Rowglow".into()),
        comments: Some("Ask your data anything.".into()),
        ..Default::default()
    };
    let item = |id: &str, text: &str| MenuItem::with_id(app, id, text, true, None::<&str>);
    let file_menu = Submenu::with_items(
        app,
        "&File",
        true,
        &[
            &item("new-connection", "&New Connection…")?,
            &item("new-query", "New &Query Tab")?,
            &item("open-sql", "&Open SQL File…")?,
            &PredefinedMenuItem::separator(app)?,
            &item("radar", "Scan for &Local Databases")?,
            &PredefinedMenuItem::separator(app)?,
            &item("settings", "&Settings…")?,
            &item("license", "&License…")?,
            &PredefinedMenuItem::separator(app)?,
            // A custom item: the predefined Quit isn't supported on GTK.
            &item("quit", "E&xit")?,
        ],
    )?;
    let view_menu = Submenu::with_items(
        app,
        "&View",
        true,
        &[
            &item("palette", "&Open Anything…")?,
            &item("toggle-sidebar", "Toggle &Sidebar")?,
            &item("toggle-inspector", "Toggle &Inspector")?,
        ],
    )?;
    let help_menu = Submenu::with_items(
        app,
        "&Help",
        true,
        &[
            &item("shortcuts", "&Keyboard Shortcuts")?,
            &item("website", "Rowglow &Website")?,
            &PredefinedMenuItem::separator(app)?,
            &item("updates", "Check for &Updates…")?,
            &PredefinedMenuItem::about(app, Some("&About Rowglow"), Some(about))?,
        ],
    )?;
    Menu::with_items(app, &[&file_menu, &view_menu, &help_menu])
}

fn main() {
    rowglow_lib::init_crypto();
    let app = tauri::Builder::default()
        // Must come first: a second launch just brings the running window forward,
        // so two copies never overwrite each other's settings or lock the same DuckDB file.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
        }))
        .manage(Unsaved(AtomicBool::new(false)))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let _ = std::fs::create_dir_all(&data_dir);
            rowglow_lib::crashlog::install_panic_hook(&data_dir);
            rowglow_lib::state::migrate_legacy_data(&data_dir);
            let state = match AppState::new(data_dir.clone(), false) {
                Ok(s) => Arc::new(s),
                Err(e) => {
                    rowglow_lib::crashlog::append(&data_dir, &format!("startup failed: {e}"));
                    return Err(Box::new(e) as Box<dyn std::error::Error>);
                }
            };
            if let Some(moved) = &state.recovered_db {
                rowglow_lib::crashlog::append(&data_dir, &format!("app.db was damaged; moved to {}", moved.display()));
            }
            // setup() runs on the main thread outside Tokio: spawn on Tauri's runtime.
            tauri::async_runtime::spawn(state.clone().background_tasks());
            app.manage(state);
            let menu = build_menu(app.handle())?;
            app.set_menu(menu)?;
            app.on_menu_event(|app, event| match event.id().0.as_str() {
                "quit" => request_quit(app),
                id => {
                    let _ = app.emit("menu", id);
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if has_unsaved(window.app_handle()) {
                    api.prevent_close();
                    let _ = window.app_handle().emit("confirm-quit", ());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![rpc, set_unsaved, quit_app])
        .build(tauri::generate_context!())
        .expect("error while building Rowglow");
    app.run(|app, event| {
        // A quit not initiated by us (e.g. the Dock's Quit) while edits are staged.
        if let RunEvent::ExitRequested { code: None, api, .. } = event {
            if has_unsaved(app) {
                api.prevent_exit();
                let _ = app.emit("confirm-quit", ());
            }
        }
    });
}

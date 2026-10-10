use std::{fs, path::PathBuf};

use crate::{
    models::Snapshot,
    state::{RefreshMode, Store},
    storage, token,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VisibilityAction {
    None,
    Hide,
    Show,
}

pub(crate) fn fullscreen_visibility_transition(
    is_fullscreen: bool,
    user_hidden: bool,
    hidden_by_fullscreen: bool,
) -> (bool, VisibilityAction) {
    if is_fullscreen && !user_hidden && !hidden_by_fullscreen {
        (true, VisibilityAction::Hide)
    } else if !is_fullscreen && hidden_by_fullscreen {
        if user_hidden {
            (false, VisibilityAction::None)
        } else {
            (false, VisibilityAction::Show)
        }
    } else {
        (hidden_by_fullscreen, VisibilityAction::None)
    }
}

#[tauri::command]
pub(crate) fn get_dashboard_snapshot(state: tauri::State<Store>) -> Snapshot {
    crate::dashboard_snapshot(&state, RefreshMode::Cached)
}

#[tauri::command]
pub(crate) fn refresh_dashboard_snapshot(state: tauri::State<Store>) -> Snapshot {
    crate::dashboard_snapshot(&state, RefreshMode::Force)
}

#[tauri::command]
pub(crate) fn clear_token_history(state: tauri::State<Store>) -> Result<(), String> {
    token::clear_history(&state)
}

#[tauri::command]
pub(crate) fn save_window_position(
    state: tauri::State<Store>,
    x: i32,
    y: i32,
) -> Result<(), String> {
    let db = state.db.lock().map_err(|_| "database lock")?;
    let expanded = *state.expanded.lock().map_err(|_| "lock")?;
    storage::save_window_position(&db, x, y, expanded).map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
pub(crate) fn set_window_expanded(
    app: tauri::AppHandle,
    state: tauri::State<Store>,
    expanded: bool,
) -> Result<(), String> {
    *state.expanded.lock().map_err(|_| "lock")? = expanded;
    {
        let db = state.db.lock().map_err(|_| "database lock")?;
        storage::save_window_expanded(&db, expanded).map_err(|error| error.to_string())?;
    }
    use tauri::Manager;
    if let Some(window) = app.get_webview_window("float") {
        window
            .set_size(tauri::LogicalSize::new(
                if expanded { 320.0 } else { 250.0 },
                if expanded { 280.0 } else { 64.0 },
            ))
            .map_err(|error| error.to_string())?;
        if let (Some(monitor), Ok(position), Ok(size)) = (
            window
                .current_monitor()
                .map_err(|error| error.to_string())?,
            window.outer_position(),
            window.outer_size(),
        ) {
            let area = monitor.work_area();
            let min_x = area.position.x;
            let min_y = area.position.y;
            let max_x = (area.position.x + area.size.width as i32 - size.width as i32).max(min_x);
            let max_y = (area.position.y + area.size.height as i32 - size.height as i32).max(min_y);
            let x = position.x.clamp(min_x, max_x);
            let y = position.y.clamp(min_y, max_y);
            if x != position.x || y != position.y {
                window
                    .set_position(tauri::PhysicalPosition::new(x, y))
                    .map_err(|error| error.to_string())?;
            }
            let db = state.db.lock().map_err(|_| "database lock")?;
            storage::save_window_position(&db, x, y, expanded)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

#[cfg(windows)]
fn start_fullscreen_monitor(app: tauri::AppHandle) {
    use std::{thread, time::Duration};
    use tauri::Manager;
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible,
    };

    thread::spawn(move || {
        let own_pid = unsafe { GetCurrentProcessId() };
        let mut hidden = false;
        loop {
            let foreground = unsafe { GetForegroundWindow() };
            let mut pid = 0;
            let foreground_pid = unsafe { GetWindowThreadProcessId(foreground, Some(&mut pid)) };
            let mut fullscreen = false;
            if foreground_pid != 0
                && pid != own_pid
                && unsafe { IsWindowVisible(foreground).as_bool() }
            {
                let monitor = unsafe { MonitorFromWindow(foreground, MONITOR_DEFAULTTONEAREST) };
                if !monitor.is_invalid() {
                    let mut info = MONITORINFO {
                        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                        rcMonitor: RECT::default(),
                        rcWork: RECT::default(),
                        dwFlags: 0,
                    };
                    let mut window_rect = RECT::default();
                    let got_monitor = unsafe { GetMonitorInfoW(monitor, &mut info).as_bool() };
                    let got_window = unsafe { GetWindowRect(foreground, &mut window_rect).is_ok() };
                    fullscreen = got_monitor && got_window && window_rect == info.rcMonitor;
                }
            }
            if let Some(window) = app.get_webview_window("float") {
                let user_hidden = app
                    .state::<Store>()
                    .user_hidden
                    .lock()
                    .map(|value| *value)
                    .unwrap_or(true);
                let (next_hidden, action) =
                    fullscreen_visibility_transition(fullscreen, user_hidden, hidden);
                hidden = next_hidden;
                match action {
                    VisibilityAction::Hide => {
                        let _ = window.hide();
                    }
                    VisibilityAction::Show => {
                        let _ = window.show();
                    }
                    VisibilityAction::None => {}
                }
            }
            thread::sleep(Duration::from_millis(500));
        }
    });
}

#[cfg(not(windows))]
fn start_fullscreen_monitor(_app: tauri::AppHandle) {}

pub(crate) fn run() {
    let data = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let db = data.join("CodexFloat").join("codex-float.sqlite");
    let _ = fs::create_dir_all(db.parent().unwrap());
    let store = Store::new(&db).expect("database");
    tauri::Builder::default()
        .manage(store)
        .setup(|app| {
            use tauri::{Emitter, Manager};
            #[cfg(desktop)]
            {
                use tauri_plugin_global_shortcut::{Code, Modifiers, ShortcutState};
                app.handle().plugin(
                    tauri_plugin_global_shortcut::Builder::new()
                        .with_shortcuts(["ctrl+shift+space"])?
                        .with_handler(|app, shortcut, event| {
                            if event.state == ShortcutState::Pressed
                                && shortcut
                                    .matches(Modifiers::CONTROL | Modifiers::SHIFT, Code::Space)
                            {
                                let state = app.state::<Store>();
                                let next = state.click_through.lock().ok().map(|mut enabled| {
                                    *enabled = !*enabled;
                                    *enabled
                                });
                                if let Some(enabled) = next {
                                    if let Some(window) = app.get_webview_window("float") {
                                        let _ = window.set_ignore_cursor_events(enabled);
                                    }
                                    let _ = app.emit("click-through-changed", enabled);
                                }
                            }
                        })
                        .build(),
                )?;
            }
            start_fullscreen_monitor(app.handle().clone());
            {
                use tauri::menu::{Menu, MenuItem};
                use tauri::tray::TrayIconBuilder;
                let show_main =
                    MenuItem::with_id(app, "show-main", "打开主窗口", true, None::<&str>)?;
                let show_float =
                    MenuItem::with_id(app, "show-float", "显示悬浮窗", true, None::<&str>)?;
                let quit = MenuItem::with_id(app, "quit", "退出 Codex Float", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&show_main, &show_float, &quit])?;
                TrayIconBuilder::new()
                    .menu(&menu)
                    .on_menu_event(|app, event| match event.id.as_ref() {
                        "show-main" => {
                            if let Some(window) = app.get_webview_window("main") {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                        }
                        "show-float" => {
                            let state = app.state::<Store>();
                            if let Ok(mut hidden) = state.user_hidden.lock() {
                                *hidden = false;
                            }
                            if let Ok(mut enabled) = state.click_through.lock() {
                                *enabled = false;
                            }
                            if let Some(window) = app.get_webview_window("float") {
                                let _ = window.set_ignore_cursor_events(false);
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                            let _ = app.emit("click-through-changed", false);
                        }
                        "quit" => app.exit(0),
                        _ => {}
                    })
                    .build(app)?;
            }
            let state = app.state::<Store>();
            let saved = state
                .db
                .lock()
                .ok()
                .and_then(|db| storage::load_window_state(&db));
            if let Some(window) = app.get_webview_window("float") {
                let expanded = saved.as_ref().map(|value| value.2).unwrap_or(false);
                if expanded {
                    window.set_size(tauri::LogicalSize::new(320.0, 300.0))?;
                }
                let valid_saved_position = saved.as_ref().is_some_and(|(x, y, _)| {
                    app.available_monitors().ok().is_some_and(|monitors| {
                        monitors.iter().any(|monitor| {
                            let area = monitor.work_area();
                            *x >= area.position.x
                                && *y >= area.position.y
                                && *x < area.position.x + area.size.width as i32
                                && *y < area.position.y + area.size.height as i32
                        })
                    })
                });
                if valid_saved_position {
                    let (x, y, _) = saved.unwrap();
                    window.set_position(tauri::PhysicalPosition::new(x, y))?;
                } else if let Some(monitor) = window.current_monitor()? {
                    let work_area = monitor.work_area();
                    let size = window.outer_size()?;
                    let margin = (16.0 * monitor.scale_factor()).round() as i32;
                    let x = work_area.position.x + work_area.size.width as i32
                        - size.width as i32
                        - margin;
                    let y = work_area.position.y + work_area.size.height as i32
                        - size.height as i32
                        - margin;
                    window.set_position(tauri::PhysicalPosition::new(x, y))?;
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if window.label() == "float" {
                    use tauri::Manager;
                    let state = window.app_handle().state::<Store>();
                    if let Ok(mut hidden) = state.user_hidden.lock() {
                        *hidden = true;
                    };
                }
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_dashboard_snapshot,
            refresh_dashboard_snapshot,
            set_window_expanded,
            save_window_position,
            clear_token_history
        ])
        .run(tauri::generate_context!())
        .expect("error running app");
}

#[cfg(test)]
mod tests {
    use super::{fullscreen_visibility_transition, VisibilityAction};

    #[test]
    fn fullscreen_visibility_preserves_user_visibility_choice() {
        assert_eq!(
            fullscreen_visibility_transition(true, false, false),
            (true, VisibilityAction::Hide)
        );
        assert_eq!(
            fullscreen_visibility_transition(false, true, true),
            (false, VisibilityAction::None)
        );
        assert_eq!(
            fullscreen_visibility_transition(false, false, true),
            (false, VisibilityAction::Show)
        );
        assert_eq!(
            fullscreen_visibility_transition(true, true, false),
            (false, VisibilityAction::None)
        );
    }
}

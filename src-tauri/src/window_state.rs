use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::Duration,
};
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, Window};

const WINDOW_STATE_FILE: &str = "window-state.json";
static DELAYED_SAVE_TASK: OnceLock<Mutex<Option<tauri::async_runtime::JoinHandle<()>>>> =
    OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedWindowState {
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    #[serde(default)]
    maximized: bool,
    #[serde(default)]
    fullscreen: bool,
}

fn state_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Window settings folder could not be found: {e}"))?;
    fs::create_dir_all(&dir).map_err(|e| format!("Window settings folder could not be created: {e}"))?;
    Ok(dir.join(WINDOW_STATE_FILE))
}

fn read_window_state(app: &AppHandle) -> Option<PersistedWindowState> {
    let path = state_path(app).ok()?;
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str::<PersistedWindowState>(&text).ok()
}

pub fn restore_window_state(app: &AppHandle) -> Result<(), String> {
    let Some(window) = app.get_webview_window("main") else {
        return Ok(());
    };

    let Some(state) = read_window_state(app) else {
        return Ok(());
    };

    if state.width < 600 || state.height < 600 {
        return Ok(());
    }

    window
        .set_size(PhysicalSize::new(state.width, state.height))
        .map_err(|e| format!("Window size could not be restored: {e}"))?;
    if saved_position_is_visible(app, &state) {
        window
            .set_position(PhysicalPosition::new(state.x, state.y))
            .map_err(|e| format!("Window position could not be restored: {e}"))?;
    } else {
        window
            .center()
            .map_err(|e| format!("Window could not be centered: {e}"))?;
    }

    if state.fullscreen {
        window
            .set_fullscreen(true)
            .map_err(|e| format!("Fullscreen state could not be restored: {e}"))?;
    } else if state.maximized {
        window
            .maximize()
            .map_err(|e| format!("Maximized state could not be restored: {e}"))?;
    }
    Ok(())
}

pub fn save_window_state(window: &Window) -> Result<(), String> {
    if window.label() != "main" {
        return Ok(());
    }

    let is_minimized = window
        .is_minimized()
        .map_err(|e| format!("Window state could not be read: {e}"))?;
    if is_minimized {
        return Ok(());
    }

    let maximized = window.is_maximized().unwrap_or(false);
    let fullscreen = window.is_fullscreen().unwrap_or(false);
    let previous_state = read_window_state(&window.app_handle());

    let size = window
        .outer_size()
        .map_err(|e| format!("Window size could not be read: {e}"))?;
    let position = window
        .outer_position()
        .map_err(|e| format!("Window position could not be read: {e}"))?;

    if size.width < 600 || size.height < 600 {
        return Ok(());
    }

    let (width, height, x, y) = if maximized || fullscreen {
        previous_state
            .map(|state| (state.width, state.height, state.x, state.y))
            .unwrap_or((size.width, size.height, position.x, position.y))
    } else {
        (size.width, size.height, position.x, position.y)
    };

    let state = PersistedWindowState {
        width,
        height,
        x,
        y,
        maximized,
        fullscreen,
    };

    let app = window.app_handle();
    let path = state_path(&app)?;
    let json = serde_json::to_string_pretty(&state)
        .map_err(|e| format!("Window state could not be prepared: {e}"))?;

    crate::safe_fs::atomic_write(&path, json.as_bytes())
        .map_err(|e| format!("Window state could not be saved: {e}"))
}

/// Captures the normal window bounds after Windows finishes a
/// fullscreen/maximize transition. The first resize event may still report the
/// previous state, which would otherwise leave stale bounds on the next launch.
pub fn save_window_state_after_transition(window: Window) {
    if !window.is_fullscreen().unwrap_or(false) && !window.is_maximized().unwrap_or(false) {
        return;
    }

    let task = tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if let Err(error) = save_window_state(&window) {
            eprintln!("[WINDOW_STATE] {error}");
        }
    });
    match DELAYED_SAVE_TASK.get_or_init(|| Mutex::new(None)).lock() {
        Ok(mut pending) => {
            if let Some(previous) = pending.replace(task) {
                previous.abort();
            }
        }
        Err(_) => task.abort(),
    }
}

fn saved_position_is_visible(app: &AppHandle, state: &PersistedWindowState) -> bool {
    let Ok(monitors) = app.available_monitors() else {
        return true;
    };

    if monitors.is_empty() {
        return true;
    }

    let center_x = state.x + (state.width as i32 / 2);
    let center_y = state.y + (state.height as i32 / 2);

    monitors.iter().any(|monitor| {
        let position = monitor.position();
        let size = monitor.size();
        let left = position.x;
        let top = position.y;
        let right = left + size.width as i32;
        let bottom = top + size.height as i32;
        center_x >= left && center_x <= right && center_y >= top && center_y <= bottom
    })
}

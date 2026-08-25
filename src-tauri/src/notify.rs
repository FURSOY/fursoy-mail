use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager};
use windows::Win32::Foundation::RECT;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetDesktopWindow, GetForegroundWindow, GetWindowRect,
};

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct NotificationPayload {
    pub title: String,
    pub body: String,
    pub kind: Option<String>,
    pub code: Option<String>,
    #[serde(rename = "emailId")]
    pub email_id: Option<String>,
    pub duration: Option<u32>,
    #[serde(rename = "accountId")]
    pub account_id: Option<String>,
    #[serde(rename = "accountPicture")]
    pub account_picture: Option<String>,
    #[serde(rename = "multiAccount")]
    pub multi_account: Option<bool>,
    #[serde(rename = "copyLabel")]
    pub copy_label: Option<String>,
    #[serde(rename = "copiedLabel")]
    pub copied_label: Option<String>,
    #[serde(rename = "copyFailedLabel")]
    pub copy_failed_label: Option<String>,
    #[serde(rename = "dismissAllLabel")]
    pub dismiss_all_label: Option<String>,
}

pub struct PendingNotification {
    lifecycle: tokio::sync::Mutex<()>,
    queue: Mutex<NotificationQueue>,
}

#[derive(Default)]
struct NotificationQueue {
    pending: VecDeque<NotificationPayload>,
    ready: bool,
    created_at: Option<Instant>,
}

const MAX_PENDING_NOTIFICATIONS: usize = 50;
const NOTIFICATION_READY_TIMEOUT: Duration = Duration::from_secs(5);

impl NotificationQueue {
    fn push(&mut self, payload: NotificationPayload) {
        if self.pending.len() >= MAX_PENDING_NOTIFICATIONS {
            self.pending.pop_front();
        }
        self.pending.push_back(payload);
    }
}

impl Default for PendingNotification {
    fn default() -> Self {
        Self {
            lifecycle: tokio::sync::Mutex::new(()),
            queue: Mutex::new(NotificationQueue::default()),
        }
    }
}

#[tauri::command]
pub fn is_system_fullscreen(window: tauri::WebviewWindow) -> Result<bool, String> {
    crate::require_command_window(&window, &["main"])?;
    Ok(is_fullscreen())
}

pub fn is_fullscreen() -> bool {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0 == std::ptr::null_mut() {
            return false;
        }

        let desktop = GetDesktopWindow();
        let mut desktop_rect = RECT::default();
        let mut window_rect = RECT::default();

        if GetWindowRect(desktop, &mut desktop_rect).is_err()
            || GetWindowRect(hwnd, &mut window_rect).is_err()
        {
            return false;
        }

        let is_covering = window_rect.left <= 0
            && window_rect.top <= 0
            && window_rect.right >= desktop_rect.right
            && window_rect.bottom >= desktop_rect.bottom;

        if !is_covering {
            return false;
        }

        let mut class_name = [0u16; 256];
        let len = GetClassNameW(hwnd, &mut class_name) as usize;
        if len > 0 {
            let class_string = String::from_utf16_lossy(&class_name[..len]);
            if class_string == "WorkerW"
                || class_string == "Progman"
                || class_string == "Shell_TrayWnd"
            {
                return false;
            }
        }

        true
    }
}

const NOTIF_W: f64 = 340.0;
const MARGIN: f64 = 16.0;
const TASKBAR_H: f64 = 48.0;

#[tauri::command]
pub async fn show_custom_notification(
    window: tauri::WebviewWindow,
    app: AppHandle,
    title: String,
    body: String,
    kind: Option<String>,
    code: Option<String>,
    email_id: Option<String>,
    duration: Option<u32>,
    account_id: Option<String>,
    account_picture: Option<String>,
    multi_account: Option<bool>,
    copy_label: Option<String>,
    copied_label: Option<String>,
    copy_failed_label: Option<String>,
    dismiss_all_label: Option<String>,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    if kind.as_deref() == Some("mail") {
        let email_id = email_id
            .as_deref()
            .ok_or_else(|| "Mail notification is missing an email ID.".to_string())?;
        let account_id = account_id
            .as_deref()
            .ok_or_else(|| "Mail notification is missing an account ID.".to_string())?;
        if !crate::db::email_belongs_to_account(&app, email_id, account_id)? {
            return Err("Mail notification does not belong to the selected account.".to_string());
        }
    }
    let controls = crate::settings::read_app_controls(&app);
    if !controls.allows_notification(
        kind.as_deref(),
        code.as_ref().is_some_and(|value| !value.is_empty()),
    ) {
        return Ok(());
    }

    if is_fullscreen() {
        return Ok(());
    }

    let payload = NotificationPayload {
        title,
        body,
        kind,
        code,
        email_id,
        duration,
        account_id,
        account_picture,
        multi_account,
        copy_label,
        copied_label,
        copy_failed_label,
        dismiss_all_label,
    };
    let state = app.state::<PendingNotification>();
    let _lifecycle_guard = state.lifecycle.lock().await;
    let mut payload_queued = false;

    // If window already exists (hidden or visible), just send new notification
    if let Some(window) = app.get_webview_window("notification") {
        let (ready, recreate) = {
            let mut queue = state
                .queue
                .lock()
                .map_err(|_| "Notification queue is unavailable.".to_string())?;
            if !queue.ready {
                queue.push(payload.clone());
                payload_queued = true;
            }
            let recreate = !queue.ready
                && queue
                    .created_at
                    .is_some_and(|created_at| created_at.elapsed() >= NOTIFICATION_READY_TIMEOUT);
            if recreate {
                queue.created_at = Some(Instant::now());
            }
            (queue.ready, recreate)
        };
        if !ready {
            if !recreate {
                return Ok(());
            }
            window.destroy().map_err(|error| error.to_string())?;
        } else {
            window
                .emit("new-notification", payload)
                .map_err(|error| error.to_string())?;
            window.show().map_err(|error| error.to_string())?;
            return Ok(());
        }
    }

    // Store payload for first load
    {
        let mut queue = state
            .queue
            .lock()
            .map_err(|_| "Notification queue is unavailable.".to_string())?;
        queue.ready = false;
        queue.created_at = Some(Instant::now());
        if !payload_queued {
            queue.push(payload);
        }
    }

    let monitor_result = app.primary_monitor();
    let (screen_w, screen_h) = if let Ok(Some(monitor)) = monitor_result {
        let size = monitor.size();
        let scale = monitor.scale_factor();
        (size.width as f64 / scale, size.height as f64 / scale)
    } else {
        (1920.0, 1080.0)
    };

    let initial_h = 90.0; // fits one card; JS will resize after mount
    let x = screen_w - NOTIF_W - MARGIN;
    let y = screen_h - initial_h - MARGIN - TASKBAR_H;

    let app_clone = app.clone();
    let (result_sender, result_receiver) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let result = tauri::WebviewWindowBuilder::new(
            &app_clone,
            "notification",
            tauri::WebviewUrl::App("notification.html".into()),
        )
        .title("Notification")
        .inner_size(NOTIF_W, initial_h)
        .position(x, y)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .focused(false)
        .visible(false)
        .build()
        .map(|_| ())
        .map_err(|e| format!("Notification window could not be created: {e}"));

        if result.is_err() {
            if let Some(state) = app_clone.try_state::<PendingNotification>() {
                if let Ok(mut queue) = state.queue.lock() {
                    queue.ready = false;
                    queue.created_at = None;
                    queue.pending.clear();
                }
            }
        }
        let _ = result_sender.send(result);
    })
    .map_err(|e| format!("Notification window could not be started: {e}"))?;
    tokio::time::timeout(std::time::Duration::from_secs(5), result_receiver)
        .await
        .map_err(|_| "Notification window did not start in time.".to_string())?
        .map_err(|_| "Notification window could not be started.".to_string())?
}

/// Called by notification window to get the initial payload
#[tauri::command]
pub fn get_pending_notification(
    window: tauri::WebviewWindow,
    app: AppHandle,
) -> Result<Vec<NotificationPayload>, String> {
    crate::require_command_window(&window, &["notification"])?;
    let state = app.state::<PendingNotification>();
    let mut queue = state
        .queue
        .lock()
        .map_err(|_| "Notification queue is unavailable.".to_string())?;
    let pending = queue.pending.drain(..).collect();
    queue.ready = true;
    queue.created_at = None;
    Ok(pending)
}

#[cfg(test)]
mod tests {
    use super::{NotificationPayload, NotificationQueue, MAX_PENDING_NOTIFICATIONS};

    fn payload(index: usize) -> NotificationPayload {
        NotificationPayload {
            title: index.to_string(),
            body: String::new(),
            kind: None,
            code: None,
            email_id: None,
            duration: None,
            account_id: None,
            account_picture: None,
            multi_account: None,
            copy_label: None,
            copied_label: None,
            copy_failed_label: None,
            dismiss_all_label: None,
        }
    }

    #[test]
    fn pending_queue_keeps_only_the_newest_notifications() {
        let mut queue = NotificationQueue::default();
        for index in 0..MAX_PENDING_NOTIFICATIONS + 3 {
            queue.push(payload(index));
        }
        assert_eq!(queue.pending.len(), MAX_PENDING_NOTIFICATIONS);
        assert_eq!(queue.pending.front().unwrap().title, "3");
    }
}

/// Called by notification window to get screen info for repositioning
#[tauri::command]
pub fn get_screen_info(window: tauri::WebviewWindow, app: AppHandle) -> Result<(f64, f64), String> {
    crate::require_command_window(&window, &["notification"])?;
    if let Ok(Some(monitor)) = app.primary_monitor() {
        let size = monitor.size();
        let scale = monitor.scale_factor();
        Ok((size.width as f64 / scale, size.height as f64 / scale))
    } else {
        Ok((1920.0, 1080.0))
    }
}

/// Show the main window and force WebView2 to re-render at the correct DPI.
/// On Windows, hiding a WebView2 window puts it in a degraded render mode;
/// setting the size again after show() triggers a proper recompose.
pub fn show_main_window(window: &tauri::WebviewWindow) -> Result<(), String> {
    window
        .show()
        .map_err(|e| format!("Main window could not be shown: {e}"))?;
    window
        .unminimize()
        .map_err(|e| format!("Main window could not be restored: {e}"))?;
    // Force WebView2 to re-render at correct DPI after being hidden.
    // Skip if maximized/fullscreen — set_size would un-maximize the window.
    let is_maximized = window.is_maximized().unwrap_or(false);
    let is_fullscreen = window.is_fullscreen().unwrap_or(false);
    if !is_maximized && !is_fullscreen {
        if let Ok(size) = window.inner_size() {
            window
                .set_size(tauri::PhysicalSize::new(size.width + 1, size.height))
                .map_err(|e| format!("Main window could not be redrawn: {e}"))?;
            window
                .set_size(tauri::PhysicalSize::new(size.width, size.height))
                .map_err(|e| format!("Main window size could not be restored: {e}"))?;
        }
    }
    window
        .set_focus()
        .map_err(|e| format!("Main window could not be focused: {e}"))
}

/// Called by notification window to focus the main window reliably via Rust
#[tauri::command]
pub fn focus_main_window(window: tauri::WebviewWindow, app: AppHandle) -> Result<(), String> {
    crate::require_command_window(&window, &["notification"])?;
    if let Some(window) = app.get_webview_window("main") {
        show_main_window(&window)?;
    }
    Ok(())
}

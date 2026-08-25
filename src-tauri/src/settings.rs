use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf, sync::Mutex};
use tauri::{AppHandle, Manager};

#[cfg(target_os = "windows")]
use windows::{
    core::{w, PCWSTR},
    Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS},
    Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
        RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE,
        REG_SZ, REG_VALUE_TYPE,
    },
    Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
};

#[cfg(target_os = "windows")]
const STARTUP_VALUE_NAME: &str = "FURSOY Mail";
#[cfg(target_os = "windows")]
const DEFAULT_MAIL_PROG_ID: &str = "FURSOYMail.Url.Mailto";
#[cfg(target_os = "windows")]
const DEFAULT_MAIL_REGISTERED_APP: &str = "FURSOY Mail";
#[cfg(target_os = "windows")]
const DEFAULT_MAIL_CAPABILITIES_PATH: &str = r"Software\FURSOY\FURSOY Mail\Capabilities";

const APP_CONTROLS_FILE: &str = "app-controls.json";

fn default_app_language() -> String {
    "en".into()
}

fn default_notification_mode() -> String {
    "all".into()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppControls {
    #[serde(default = "default_notification_mode")]
    pub notification_mode: String,
    #[serde(default, rename = "notificationsMuted", skip_serializing)]
    legacy_notifications_muted: bool,
    pub mail_sync_paused: bool,
    pub quiet_hours_enabled: bool,
    pub quiet_hours_start: String,
    pub quiet_hours_end: String,
    #[serde(default = "default_app_language")]
    pub app_language: String,
}

#[derive(Default)]
pub struct AppControlsState(pub Mutex<()>);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppControlsPatch {
    notification_mode: Option<String>,
    mail_sync_paused: Option<bool>,
    quiet_hours_enabled: Option<bool>,
    quiet_hours_start: Option<String>,
    quiet_hours_end: Option<String>,
    app_language: Option<String>,
}

impl AppControlsPatch {
    fn apply(self, controls: &mut AppControls) {
        if let Some(value) = self.notification_mode {
            controls.notification_mode = value;
        }
        if let Some(value) = self.mail_sync_paused {
            controls.mail_sync_paused = value;
        }
        if let Some(value) = self.quiet_hours_enabled {
            controls.quiet_hours_enabled = value;
        }
        if let Some(value) = self.quiet_hours_start {
            controls.quiet_hours_start = value;
        }
        if let Some(value) = self.quiet_hours_end {
            controls.quiet_hours_end = value;
        }
        if let Some(value) = self.app_language {
            controls.app_language = value;
        }
    }
}

impl Default for AppControls {
    fn default() -> Self {
        Self {
            notification_mode: default_notification_mode(),
            legacy_notifications_muted: false,
            mail_sync_paused: false,
            quiet_hours_enabled: false,
            quiet_hours_start: "22:00".into(),
            quiet_hours_end: "08:00".into(),
            app_language: "en".into(),
        }
    }
}

impl AppControls {
    pub fn notifications_disabled(&self) -> bool {
        self.notification_mode == "off"
    }

    pub fn allows_notification(&self, kind: Option<&str>, has_code: bool) -> bool {
        match self.notification_mode.as_str() {
            "off" => false,
            "otpOnly" => kind == Some("mail") && has_code,
            _ => true,
        }
    }
}

fn is_valid_clock_time(value: &str) -> bool {
    if value.len() != 5 || value.as_bytes().get(2) != Some(&b':') {
        return false;
    }
    let Ok(hours) = value[..2].parse::<u8>() else {
        return false;
    };
    let Ok(minutes) = value[3..].parse::<u8>() else {
        return false;
    };
    hours < 24 && minutes < 60
}

fn normalize_app_controls(mut controls: AppControls) -> AppControls {
    if !matches!(
        controls.notification_mode.as_str(),
        "all" | "otpOnly" | "off"
    ) {
        controls.notification_mode = default_notification_mode();
    }
    if !matches!(controls.app_language.as_str(), "en" | "tr") {
        controls.app_language = default_app_language();
    }
    if !is_valid_clock_time(&controls.quiet_hours_start) {
        controls.quiet_hours_start = AppControls::default().quiet_hours_start;
    }
    if !is_valid_clock_time(&controls.quiet_hours_end) {
        controls.quiet_hours_end = AppControls::default().quiet_hours_end;
    }
    controls
}

fn validate_app_controls(controls: &AppControls) -> Result<(), String> {
    if !matches!(
        controls.notification_mode.as_str(),
        "all" | "otpOnly" | "off"
    ) {
        return Err("Unsupported notification mode".into());
    }
    if !matches!(controls.app_language.as_str(), "en" | "tr") {
        return Err("Unsupported app language".into());
    }
    if !is_valid_clock_time(&controls.quiet_hours_start)
        || !is_valid_clock_time(&controls.quiet_hours_end)
    {
        return Err("Quiet hours must use HH:MM format".into());
    }
    Ok(())
}

fn controls_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Ayar klasoru bulunamadi: {e}"))?;
    fs::create_dir_all(&dir).map_err(|e| format!("Ayar klasoru olusturulamadi: {e}"))?;
    Ok(dir.join(APP_CONTROLS_FILE))
}

pub fn read_app_controls(app: &AppHandle) -> AppControls {
    let Ok(path) = controls_path(app) else {
        return AppControls::default();
    };
    let Ok(text) = fs::read_to_string(path) else {
        return AppControls::default();
    };
    let mut controls: AppControls = serde_json::from_str(&text).unwrap_or_default();
    if controls.legacy_notifications_muted {
        controls.notification_mode = "off".into();
        controls.legacy_notifications_muted = false;
    }
    normalize_app_controls(controls)
}

pub fn write_app_controls(app: &AppHandle, controls: &AppControls) -> Result<(), String> {
    let path = controls_path(app)?;
    let json = serde_json::to_string_pretty(controls).map_err(|e| e.to_string())?;
    crate::safe_fs::atomic_write(&path, json.as_bytes())
        .map_err(|e| format!("Ayarlar kaydedilemedi: {e}"))
}

#[tauri::command]
pub fn get_app_controls(
    window: tauri::WebviewWindow,
    app: AppHandle,
) -> Result<AppControls, String> {
    crate::require_command_window(&window, &["main"])?;
    Ok(read_app_controls(&app))
}

#[tauri::command]
pub fn set_app_controls(
    window: tauri::WebviewWindow,
    app: AppHandle,
    state: tauri::State<'_, AppControlsState>,
    controls: AppControlsPatch,
) -> Result<AppControls, String> {
    crate::require_command_window(&window, &["main"])?;
    let _guard = state
        .0
        .lock()
        .map_err(|_| "App controls lock is unavailable.")?;
    let mut controls_current = read_app_controls(&app);
    controls.apply(&mut controls_current);
    validate_app_controls(&controls_current)?;
    write_app_controls(&app, &controls_current)?;
    Ok(controls_current)
}

#[tauri::command]
pub fn set_notifications_muted(
    window: tauri::WebviewWindow,
    app: AppHandle,
    state: tauri::State<'_, AppControlsState>,
    muted: bool,
) -> Result<AppControls, String> {
    crate::require_command_window(&window, &["main"])?;
    let _guard = state
        .0
        .lock()
        .map_err(|_| "App controls lock is unavailable.")?;
    let mut controls = read_app_controls(&app);
    controls.notification_mode = if muted { "off" } else { "all" }.into();
    write_app_controls(&app, &controls)?;
    Ok(controls)
}

#[tauri::command]
pub fn set_mail_sync_paused(
    window: tauri::WebviewWindow,
    app: AppHandle,
    state: tauri::State<'_, AppControlsState>,
    paused: bool,
) -> Result<AppControls, String> {
    crate::require_command_window(&window, &["main"])?;
    let _guard = state
        .0
        .lock()
        .map_err(|_| "App controls lock is unavailable.")?;
    let mut controls = read_app_controls(&app);
    controls.mail_sync_paused = paused;
    write_app_controls(&app, &controls)?;
    Ok(controls)
}

#[tauri::command]
pub fn set_app_language(
    window: tauri::WebviewWindow,
    app: AppHandle,
    state: tauri::State<'_, AppControlsState>,
    language: String,
) -> Result<AppControls, String> {
    crate::require_command_window(&window, &["main"])?;
    let _guard = state
        .0
        .lock()
        .map_err(|_| "App controls lock is unavailable.")?;
    if language != "en" && language != "tr" {
        return Err("Unsupported app language".into());
    }
    let mut controls = read_app_controls(&app);
    controls.app_language = language;
    write_app_controls(&app, &controls)?;
    if let Some(handles) = app.try_state::<crate::TrayMenuHandles>() {
        let (mute_label, pause_label, quit_label) = crate::tray_labels(&controls.app_language);
        let _ = handles.mute.set_text(mute_label);
        let _ = handles.pause.set_text(pause_label);
        let _ = handles.quit.set_text(quit_label);
    }
    Ok(controls)
}

#[cfg(target_os = "windows")]
fn app_exe_path() -> Result<String, String> {
    std::env::current_exe()
        .map_err(|e| format!("Uygulama yolu okunamadi: {e}"))
        .map(|path| path.to_string_lossy().to_string())
}

#[cfg(target_os = "windows")]
fn startup_command() -> Result<String, String> {
    Ok(format!("\"{}\" --background", app_exe_path()?))
}

#[cfg(target_os = "windows")]
fn startup_value_has_background_arg(value: &str) -> bool {
    value.contains("--background") || value.contains("--hidden") || value.contains("--minimized")
}

#[cfg(target_os = "windows")]
fn startup_value_matches_current_app(value: &str) -> Result<bool, String> {
    let exe = app_exe_path()?;
    let trimmed = value.trim();
    Ok(trimmed.trim_matches('"') == exe || trimmed.starts_with(&format!("\"{exe}\"")))
}

#[cfg(target_os = "windows")]
fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(target_os = "windows")]
fn reg_open(access: windows::Win32::System::Registry::REG_SAM_FLAGS) -> Result<HKEY, String> {
    let subkey = to_wide_null(r"Software\Microsoft\Windows\CurrentVersion\Run");
    let mut hkey = HKEY::default();
    let err = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            Some(0),
            access,
            &mut hkey,
        )
    };
    if err == ERROR_SUCCESS {
        Ok(hkey)
    } else {
        Err(format!("Registry acilamadi: {err:?}"))
    }
}

#[cfg(target_os = "windows")]
fn write_startup_value(command: &str) -> Result<(), String> {
    let hkey = reg_open(KEY_SET_VALUE)?;
    let value_name = to_wide_null(STARTUP_VALUE_NAME);
    let data: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 2) };
    let err =
        unsafe { RegSetValueExW(hkey, PCWSTR(value_name.as_ptr()), None, REG_SZ, Some(bytes)) };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    if err == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("Baslangic kaydi eklenemedi: {err:?}"))
    }
}

#[cfg(target_os = "windows")]
fn write_registry_string(
    subkey: &str,
    value_name: Option<&str>,
    value: &str,
) -> Result<(), String> {
    let subkey = to_wide_null(subkey);
    let mut hkey = HKEY::default();
    let err = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut hkey,
            None,
        )
    };
    if err != ERROR_SUCCESS {
        return Err(format!("Registry anahtari olusturulamadi: {err:?}"));
    }

    let value_name = value_name.map(to_wide_null);
    let value_name_ptr = value_name
        .as_ref()
        .map_or(PCWSTR::null(), |name| PCWSTR(name.as_ptr()));
    let data = to_wide_null(value);
    let bytes = unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 2) };
    let err = unsafe { RegSetValueExW(hkey, value_name_ptr, None, REG_SZ, Some(bytes)) };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    if err == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("Registry degeri yazilamadi: {err:?}"))
    }
}

#[cfg(target_os = "windows")]
pub fn ensure_default_mail_registration() -> Result<(), String> {
    let exe = app_exe_path()?;
    let prog_id_path = format!(r"Software\Classes\{DEFAULT_MAIL_PROG_ID}");

    write_registry_string(&prog_id_path, None, "URL:FURSOY Mail mailto")?;
    write_registry_string(&prog_id_path, Some("URL Protocol"), "")?;
    write_registry_string(
        &format!(r"{prog_id_path}\DefaultIcon"),
        None,
        &format!("\"{exe}\",0"),
    )?;
    write_registry_string(
        &format!(r"{prog_id_path}\shell\open\command"),
        None,
        &format!("\"{exe}\" \"%1\""),
    )?;
    write_registry_string(
        DEFAULT_MAIL_CAPABILITIES_PATH,
        Some("ApplicationName"),
        DEFAULT_MAIL_REGISTERED_APP,
    )?;
    write_registry_string(
        DEFAULT_MAIL_CAPABILITIES_PATH,
        Some("ApplicationDescription"),
        "Windows Gmail client with fast verification-code notifications.",
    )?;
    write_registry_string(
        &format!(r"{DEFAULT_MAIL_CAPABILITIES_PATH}\UrlAssociations"),
        Some("mailto"),
        DEFAULT_MAIL_PROG_ID,
    )?;
    write_registry_string(
        r"Software\RegisteredApplications",
        Some(DEFAULT_MAIL_REGISTERED_APP),
        DEFAULT_MAIL_CAPABILITIES_PATH,
    )
}

#[tauri::command]
pub fn open_default_mail_settings(window: tauri::WebviewWindow) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    #[cfg(target_os = "windows")]
    {
        ensure_default_mail_registration()?;
        let result = unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                w!("ms-settings:defaultapps?registeredAppUser=FURSOY%20Mail"),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        if result.0 as isize > 32 {
            Ok(())
        } else {
            Err(format!("Windows ayarlari acilamadi: {}", result.0 as isize))
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        Err("Default mail app integration is only supported on Windows.".into())
    }
}

#[cfg(target_os = "windows")]
fn delete_startup_value() -> Result<(), String> {
    let hkey = match reg_open(KEY_SET_VALUE) {
        Ok(h) => h,
        Err(_) => return Ok(()), // key doesn't exist → already not set
    };
    let value_name = to_wide_null(STARTUP_VALUE_NAME);
    let err = unsafe { RegDeleteValueW(hkey, PCWSTR(value_name.as_ptr())) };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    if err == ERROR_SUCCESS || err == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(format!("Baslangic kaydi silinemedi: {err:?}"))
    }
}

#[cfg(target_os = "windows")]
fn read_startup_value() -> Result<Option<String>, String> {
    let hkey = match reg_open(KEY_READ) {
        Ok(h) => h,
        Err(_) => return Ok(None),
    };
    let value_name = to_wide_null(STARTUP_VALUE_NAME);
    let mut data_type = REG_VALUE_TYPE::default();
    let mut size: u32 = 0;
    // First call: get required buffer size
    let size_err = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(value_name.as_ptr()),
            None,
            Some(&mut data_type),
            None,
            Some(&mut size),
        )
    };
    if size_err != ERROR_SUCCESS {
        unsafe {
            let _ = RegCloseKey(hkey);
        }
        return if size_err == ERROR_FILE_NOT_FOUND {
            Ok(None)
        } else {
            Err(format!("Baslangic kaydi okunamadi: {size_err:?}"))
        };
    }
    if size == 0 {
        unsafe {
            let _ = RegCloseKey(hkey);
        }
        return Ok(None);
    }
    let mut buffer = vec![0u8; size as usize];
    let err = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(value_name.as_ptr()),
            None,
            Some(&mut data_type),
            Some(buffer.as_mut_ptr()),
            Some(&mut size),
        )
    };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    if err != ERROR_SUCCESS {
        return Ok(None);
    }
    let words: Vec<u16> = buffer
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let s = String::from_utf16_lossy(&words)
        .trim_end_matches('\0')
        .to_string();
    Ok(if s.is_empty() { None } else { Some(s) })
}

fn get_launch_at_startup_impl() -> Result<bool, String> {
    #[cfg(target_os = "windows")]
    {
        let Some(registered) = read_startup_value()? else {
            return Ok(false);
        };
        let matches = startup_value_matches_current_app(&registered)?;
        if matches && !startup_value_has_background_arg(&registered) {
            let _ = write_startup_value(&startup_command()?);
        }
        Ok(matches)
    }

    #[cfg(not(target_os = "windows"))]
    {
        Ok(false)
    }
}

#[tauri::command]
pub fn get_launch_at_startup(window: tauri::WebviewWindow) -> Result<bool, String> {
    crate::require_command_window(&window, &["main"])?;
    get_launch_at_startup_impl()
}

#[tauri::command]
pub fn set_launch_at_startup(window: tauri::WebviewWindow, enabled: bool) -> Result<bool, String> {
    crate::require_command_window(&window, &["main"])?;
    #[cfg(target_os = "windows")]
    {
        if enabled {
            let command = startup_command()?;
            write_startup_value(&command)?;
        } else {
            delete_startup_value()?;
        }

        get_launch_at_startup_impl()
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = enabled;
        Err("Otomatik baslatma bu platformda desteklenmiyor.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::{is_valid_clock_time, validate_app_controls, AppControls, AppControlsPatch};

    #[test]
    fn app_controls_patch_preserves_unmentioned_fields() {
        let mut controls = AppControls::default();
        controls.mail_sync_paused = true;
        AppControlsPatch {
            notification_mode: Some("off".into()),
            mail_sync_paused: None,
            quiet_hours_enabled: None,
            quiet_hours_start: None,
            quiet_hours_end: None,
            app_language: None,
        }
        .apply(&mut controls);

        assert_eq!(controls.notification_mode, "off");
        assert!(controls.mail_sync_paused);
    }

    #[test]
    fn notification_modes_filter_expected_payloads() {
        let mut controls = AppControls::default();
        assert!(controls.allows_notification(Some("mail"), false));
        assert!(controls.allows_notification(Some("update"), false));

        controls.notification_mode = "otpOnly".into();
        assert!(controls.allows_notification(Some("mail"), true));
        assert!(!controls.allows_notification(Some("mail"), false));
        assert!(!controls.allows_notification(Some("update"), true));

        controls.notification_mode = "off".into();
        assert!(controls.notifications_disabled());
        assert!(!controls.allows_notification(Some("mail"), true));
    }

    #[test]
    fn app_controls_reject_invalid_enum_and_time_values() {
        assert!(is_valid_clock_time("08:30"));
        assert!(!is_valid_clock_time("8:30"));
        assert!(!is_valid_clock_time("25:00"));

        let mut controls = AppControls::default();
        controls.app_language = "invalid".into();
        assert!(validate_app_controls(&controls).is_err());

        controls.app_language = "en".into();
        controls.quiet_hours_start = "99:00".into();
        assert!(validate_app_controls(&controls).is_err());
    }
}

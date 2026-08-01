mod auth;
mod db;
mod gmail;
mod img_proxy;
mod mail_account;
mod mail_oauth;
mod notify;
mod safe_fs;
mod settings;
mod window_state;

use std::collections::HashMap;
use std::sync::Mutex;
use tauri::{Emitter, Manager};
use tokio::sync::oneshot;

fn window_label_allowed(label: &str, allowed: &[&str]) -> bool {
    allowed.contains(&label)
}

pub(crate) fn require_command_window(
    window: &tauri::WebviewWindow,
    allowed: &[&str],
) -> Result<(), String> {
    if window_label_allowed(window.label(), allowed) {
        Ok(())
    } else {
        Err("This window is not authorized to perform this action.".to_string())
    }
}

/// Per-account sync lock — prevents concurrent syncs for the same account
#[derive(Default)]
pub struct SyncWorkers {
    active: HashMap<String, i64>,
    resync_requested: HashMap<String, i64>,
    backfilling: HashMap<String, i64>,
    backfill_tasks: HashMap<String, (i64, tokio::sync::oneshot::Sender<()>)>,
}

impl SyncWorkers {
    /// Returns true only for the caller that owns this account's worker.
    pub fn claim_or_request_resync(&mut self, account_id: &str, account_generation: i64) -> bool {
        match self.active.get(account_id) {
            Some(active_generation) if *active_generation == account_generation => {
                self.resync_requested
                    .insert(account_id.to_string(), account_generation);
                false
            }
            _ => {
                self.active
                    .insert(account_id.to_string(), account_generation);
                true
            }
        }
    }

    pub fn take_resync_request(&mut self, account_id: &str, account_generation: i64) -> bool {
        if self.active.get(account_id) != Some(&account_generation) {
            return false;
        }
        self.resync_requested.remove(account_id) == Some(account_generation)
    }

    /// Atomically either keeps the worker for a queued refresh or releases it.
    pub fn take_resync_or_release(&mut self, account_id: &str, account_generation: i64) -> bool {
        if self.active.get(account_id) != Some(&account_generation) {
            return false;
        }
        self.backfilling.remove(account_id);
        if self.resync_requested.remove(account_id) == Some(account_generation) {
            true
        } else {
            self.active.remove(account_id);
            false
        }
    }

    pub fn set_backfilling(&mut self, account_id: &str, account_generation: i64) -> bool {
        if self.active.get(account_id) != Some(&account_generation) {
            return false;
        }
        self.backfilling
            .insert(account_id.to_string(), account_generation);
        true
    }

    pub fn is_backfilling(&self, account_id: Option<&str>) -> bool {
        match account_id {
            Some(account_id) => self.backfilling.contains_key(account_id),
            None => !self.backfilling.is_empty(),
        }
    }

    pub fn release(&mut self, account_id: &str, account_generation: i64) {
        if self.active.get(account_id) == Some(&account_generation) {
            self.active.remove(account_id);
            self.resync_requested.remove(account_id);
            self.backfilling.remove(account_id);
        }
    }

    pub fn invalidate_account(&mut self, account_id: &str) {
        if let Some((_, task)) = self.backfill_tasks.remove(account_id) {
            let _ = task.send(());
        }
        self.active.remove(account_id);
        self.resync_requested.remove(account_id);
        self.backfilling.remove(account_id);
    }

    pub fn register_backfill_task(
        &mut self,
        account_id: &str,
        account_generation: i64,
        cancellation: tokio::sync::oneshot::Sender<()>,
    ) {
        if let Some((_, previous)) = self
            .backfill_tasks
            .insert(account_id.to_string(), (account_generation, cancellation))
        {
            let _ = previous.send(());
        }
    }

    pub fn finish_backfill_task(&mut self, account_id: &str, account_generation: i64) {
        if self
            .backfill_tasks
            .get(account_id)
            .is_some_and(|(generation, _)| *generation == account_generation)
        {
            self.backfill_tasks.remove(account_id);
        }
    }
}

/// Single per-account worker state for initial sync, incremental sync, and backfill.
pub struct SyncState {
    pub workers: Mutex<SyncWorkers>,
}

type TokenRefreshResult = Result<db::AuthInfo, String>;

/// Shares one in-flight OAuth refresh result with every caller for an account.
/// Different accounts remain independent and can refresh in parallel.
#[derive(Default)]
pub struct TokenRefreshFlights {
    waiters: Mutex<HashMap<String, Vec<oneshot::Sender<TokenRefreshResult>>>>,
}

impl TokenRefreshFlights {
    /// Returns a receiver when another caller already owns this account's
    /// refresh. `None` means the caller must perform the refresh itself.
    pub fn join_or_start(&self, account_id: &str) -> Option<oneshot::Receiver<TokenRefreshResult>> {
        let mut waiters = self.waiters.lock().ok()?;
        if let Some(account_waiters) = waiters.get_mut(account_id) {
            let (sender, receiver) = oneshot::channel();
            account_waiters.push(sender);
            Some(receiver)
        } else {
            waiters.insert(account_id.to_string(), Vec::new());
            None
        }
    }

    pub fn finish(&self, account_id: &str, result: TokenRefreshResult) {
        let waiters = self
            .waiters
            .lock()
            .ok()
            .and_then(|mut all| all.remove(account_id))
            .unwrap_or_default();
        for waiter in waiters {
            let _ = waiter.send(result.clone());
        }
    }
}

fn is_background_launch() -> bool {
    std::env::args().any(|arg| arg == "--background" || arg == "--hidden" || arg == "--minimized")
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();

    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
        if let Some(window) = app.get_webview_window("main") {
            if let Err(error) = notify::show_main_window(&window) {
                eprintln!("[WINDOW] {error}");
            }
        }
    }));

    builder
        .register_asynchronous_uri_scheme_protocol("mailimg", |app, request, responder| {
            let uri = request.uri().to_string();
            let proxy = app
                .app_handle()
                .state::<img_proxy::ImageProxyState>()
                .inner()
                .clone();
            let Some(request_slot) = proxy.try_reserve_request() else {
                let response = tauri::http::Response::builder()
                    .status(429)
                    .header("Retry-After", "1")
                    .body(Vec::new())
                    .unwrap();
                responder.respond(response);
                return;
            };
            tauri::async_runtime::spawn(async move {
                let _request_slot = request_slot;
                let _fetch_permit = proxy.acquire_fetch().await;
                let response = match img_proxy::fetch_remote_image(uri).await {
                    Ok((bytes, content_type)) => tauri::http::Response::builder()
                        .status(200)
                        .header("Content-Type", content_type)
                        .header("Access-Control-Allow-Origin", "*")
                        .header("Cross-Origin-Resource-Policy", "cross-origin")
                        .header("Cache-Control", "max-age=86400")
                        .body(bytes)
                        .unwrap_or_else(|_| {
                            tauri::http::Response::builder()
                                .status(500)
                                .body(Vec::new())
                                .unwrap()
                        }),
                    Err(_) => tauri::http::Response::builder()
                        .status(404)
                        .body(Vec::new())
                        .unwrap(),
                };
                responder.respond(response);
            });
        })
        .manage(SyncState {
            workers: Mutex::new(SyncWorkers::default()),
        })
        .manage(TokenRefreshFlights::default())
        .manage(img_proxy::ImageProxyState::default())
        .manage(auth::OAuthFlowState::default())
        .manage(mail_oauth::MailOAuthFlowState::default())
        .manage(notify::PendingNotification::default())
        .manage(settings::AppControlsState::default())
        .manage(db::SearchCoordinator::default())
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_) => {
                if let Err(error) = window_state::save_window_state(window) {
                    eprintln!("[WINDOW_STATE] {error}");
                }
                window_state::save_window_state_after_transition(window.clone());
            }
            tauri::WindowEvent::CloseRequested { api, .. } => {
                if window.label() == "main" {
                    if let Err(error) = window_state::save_window_state(window) {
                        eprintln!("[WINDOW_STATE] {error}");
                    }
                    match window.hide() {
                        Ok(()) => api.prevent_close(),
                        Err(error) => {
                            eprintln!("[WINDOW] main window could not be hidden: {error}")
                        }
                    }
                }
            }
            _ => {}
        })
        .setup(|app| {
            let background_launch = is_background_launch();

            let _ = dotenvy::dotenv();

            db::init_db(app.handle()).expect("Failed to initialize database");
            let search_index_app = app.handle().clone();
            tauri::async_runtime::spawn_blocking(move || {
                if let Err(error) = db::backfill_email_search_text(&search_index_app) {
                    eprintln!("[MAIL_SEARCH] body index backfill failed: {error}");
                } else if let Err(error) = search_index_app.emit("mail-search-index-ready", ()) {
                    eprintln!("[MAIL_SEARCH] index-ready event failed: {error}");
                }
            });
            #[cfg(all(target_os = "windows", not(debug_assertions)))]
            if let Err(error) = settings::ensure_default_mail_registration() {
                eprintln!("Default mail registration failed: {error}");
            }
            if let Err(error) = window_state::restore_window_state(app.handle()) {
                eprintln!("[WINDOW_STATE] {error}");
            }
            if let Some(window) = app.get_webview_window("main") {
                if background_launch {
                    if let Err(error) = window.hide() {
                        eprintln!("[WINDOW] background launch could not hide main window: {error}");
                    }
                } else {
                    if let Err(error) = window.show() {
                        eprintln!("[WINDOW] main window could not be shown: {error}");
                    } else if let Err(error) = window.set_focus() {
                        eprintln!("[WINDOW] main window could not be focused: {error}");
                    }
                }
            }

            use tauri::{
                menu::{CheckMenuItem, Menu, MenuItem},
                tray::TrayIconBuilder,
                Manager,
            };
            let controls = settings::read_app_controls(app.handle());
            let mute_i = CheckMenuItem::with_id(
                app,
                "toggle_mute_notifications",
                "Bildirimleri sessize al",
                true,
                controls.notifications_disabled(),
                None::<&str>,
            )?;
            let pause_i = CheckMenuItem::with_id(
                app,
                "toggle_pause_sync",
                "Mail çekmeyi durdur",
                true,
                controls.mail_sync_paused,
                None::<&str>,
            )?;
            let quit_i = MenuItem::with_id(app, "quit", "Kapat", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&mute_i, &pause_i, &quit_i])?;
            let mute_item = mute_i.clone();
            let pause_item = pause_i.clone();

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |app, event| match event.id.as_ref() {
                    "toggle_mute_notifications" => {
                        let state = app.state::<settings::AppControlsState>();
                        let _guard = match state.0.lock() {
                            Ok(guard) => guard,
                            Err(_) => {
                                eprintln!("[TRAY] app controls lock is unavailable");
                                return;
                            }
                        };
                        let previous = settings::read_app_controls(app);
                        let mut controls = previous.clone();
                        controls.notification_mode = if controls.notifications_disabled() {
                            "all".into()
                        } else {
                            "off".into()
                        };
                        if let Err(error) = settings::write_app_controls(app, &controls) {
                            eprintln!("[TRAY] notification setting could not be saved: {error}");
                            return;
                        }
                        if let Err(error) = mute_item.set_checked(controls.notifications_disabled())
                        {
                            if settings::write_app_controls(app, &previous).is_err() {
                                eprintln!("[TRAY] notification menu update and rollback failed");
                            }
                            eprintln!("[TRAY] notification menu could not be updated: {error}");
                            return;
                        }
                        if let Err(error) = app.emit("app-controls-changed", controls) {
                            let rollback = settings::write_app_controls(app, &previous);
                            let menu_rollback =
                                mute_item.set_checked(previous.notifications_disabled());
                            if rollback.is_err() || menu_rollback.is_err() {
                                eprintln!("[TRAY] notification setting event and rollback failed");
                            } else {
                                eprintln!("[TRAY] notification setting event failed: {error}");
                            }
                        }
                    }
                    "toggle_pause_sync" => {
                        let state = app.state::<settings::AppControlsState>();
                        let _guard = match state.0.lock() {
                            Ok(guard) => guard,
                            Err(_) => {
                                eprintln!("[TRAY] app controls lock is unavailable");
                                return;
                            }
                        };
                        let previous = settings::read_app_controls(app);
                        let mut controls = previous.clone();
                        controls.mail_sync_paused = !controls.mail_sync_paused;
                        if let Err(error) = settings::write_app_controls(app, &controls) {
                            eprintln!("[TRAY] sync setting could not be saved: {error}");
                            return;
                        }
                        if let Err(error) = pause_item.set_checked(controls.mail_sync_paused) {
                            if settings::write_app_controls(app, &previous).is_err() {
                                eprintln!("[TRAY] sync menu update and rollback failed");
                            }
                            eprintln!("[TRAY] sync menu could not be updated: {error}");
                            return;
                        }
                        if let Err(error) = app.emit("app-controls-changed", controls) {
                            let rollback = settings::write_app_controls(app, &previous);
                            let menu_rollback = pause_item.set_checked(previous.mail_sync_paused);
                            if rollback.is_err() || menu_rollback.is_err() {
                                eprintln!("[TRAY] sync setting event and rollback failed");
                            } else {
                                eprintln!("[TRAY] sync setting event failed: {error}");
                            }
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::Click {
                        button: tauri::tray::MouseButton::Left,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(window) = app.get_webview_window("main") {
                            if let Err(error) = notify::show_main_window(&window) {
                                eprintln!("[WINDOW] {error}");
                            }
                        }
                    }
                })
                .build(app)?;

            Ok(())
        })
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            auth::start_google_oauth,
            auth::cancel_google_oauth,
            auth::refresh_access_token,
            mail_oauth::discover_mail_provider,
            mail_oauth::start_mail_oauth,
            mail_oauth::cancel_mail_oauth,
            mail_account::test_mail_account,
            mail_account::add_mail_account,
            mail_account::sync_imap_emails,
            mail_account::wait_for_imap_change,
            db::get_accounts,
            db::get_account_auth,
            auth::remove_account,
            db::reorder_accounts,
            db::search_contacts,
            db::get_local_emails,
            db::search_local_emails,
            db::get_emails_by_label,
            db::get_thread_groups_by_label,
            db::get_gmail_labels,
            db::search_local_thread_groups,
            db::cancel_local_search,
            db::get_orphaned_cache_counts,
            db::reset_local_mail_cache,
            db::get_email_body,
            db::get_inbox_unread_count,
            db::get_thread_emails,
            gmail::sync_emails,
            gmail::refresh_email_from_gmail,
            gmail::get_mailbox_download_status,
            gmail::mark_as_read,
            gmail::mark_thread_as_read,
            gmail::mark_as_unread,
            gmail::archive_email,
            gmail::create_gmail_label,
            gmail::rename_gmail_label,
            gmail::set_gmail_label_color,
            gmail::delete_gmail_label,
            gmail::set_thread_gmail_label,
            gmail::report_spam,
            gmail::trash_email,
            gmail::move_to_inbox,
            gmail::send_reply,
            gmail::send_email,
            gmail::list_drafts,
            gmail::get_draft,
            gmail::save_draft,
            gmail::send_draft,
            gmail::delete_draft,
            gmail::verify_sent_message,
            gmail::fetch_attachment_data,
            gmail::save_and_reveal_attachment,
            db::get_email_attachments,
            notify::show_custom_notification,
            notify::get_pending_notification,
            notify::get_screen_info,
            notify::is_system_fullscreen,
            notify::focus_main_window,
            settings::get_launch_at_startup,
            settings::set_launch_at_startup,
            settings::get_app_controls,
            settings::set_app_controls,
            settings::set_notifications_muted,
            settings::set_mail_sync_paused,
            settings::set_app_language,
            settings::open_default_mail_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::{window_label_allowed, SyncWorkers, TokenRefreshFlights};

    #[test]
    fn command_window_allowlist_is_exact() {
        assert!(window_label_allowed("main", &["main"]));
        assert!(window_label_allowed("notification", &["notification"]));
        assert!(!window_label_allowed("notification", &["main"]));
        assert!(!window_label_allowed("main-preview", &["main"]));
    }

    #[test]
    fn worker_keeps_ownership_until_the_queued_resync_is_consumed() {
        let mut workers = SyncWorkers::default();

        assert!(workers.claim_or_request_resync("account-a", 1));
        assert!(!workers.claim_or_request_resync("account-a", 1));
        assert!(workers.set_backfilling("account-a", 1));

        assert!(workers.take_resync_or_release("account-a", 1));
        assert!(!workers.claim_or_request_resync("account-a", 1));
        assert!(workers.take_resync_or_release("account-a", 1));
        assert!(!workers.take_resync_or_release("account-a", 1));
        assert!(workers.claim_or_request_resync("account-a", 1));

        workers.invalidate_account("account-a");
        assert!(workers.claim_or_request_resync("account-a", 2));
        workers.release("account-a", 1);
        assert!(!workers.claim_or_request_resync("account-a", 2));
    }

    #[tokio::test]
    async fn token_refresh_singleflight_shares_the_leader_result_per_account() {
        let flights = TokenRefreshFlights::default();

        assert!(flights.join_or_start("account-a").is_none());
        let waiting_a = flights
            .join_or_start("account-a")
            .expect("join account-a refresh");
        assert!(flights.join_or_start("account-b").is_none());

        flights.finish("account-a", Err("refresh failed".to_string()));
        assert_eq!(
            waiting_a
                .await
                .expect("receive account-a result")
                .expect_err("account-a refresh should fail"),
            "refresh failed"
        );

        // Completing account-a does not affect account-b's independent flight.
        let waiting_b = flights
            .join_or_start("account-b")
            .expect("join account-b refresh");
        flights.finish("account-b", Err("other account failed".to_string()));
        assert_eq!(
            waiting_b
                .await
                .expect("receive account-b result")
                .expect_err("account-b refresh should fail"),
            "other account failed"
        );

        assert!(flights.join_or_start("account-a").is_none());
    }
}

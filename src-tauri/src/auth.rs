use tauri::Manager;

/// Renews the session for one account. OAuth providers refresh through
/// `mail_oauth`; password accounts hold no renewable session because the stored
/// credential is validated by the IMAP/SMTP connection itself.
async fn refresh_access_token_once(
    app: tauri::AppHandle,
    account_id: &str,
) -> Result<crate::db::AuthInfo, String> {
    if crate::db::load_tokens(account_id).is_some() {
        return crate::mail_oauth::refresh_mail_oauth_token(app, account_id).await;
    }
    if crate::mail_account::has_stored_password(account_id) {
        return Ok(crate::db::AuthInfo {
            authenticated: true,
            expires_at: None,
            email: account_id.to_string(),
            picture: crate::db::get_account_picture(&app, account_id),
        });
    }
    Err("No session found. Please sign in again.".to_string())
}

#[tauri::command]
pub async fn remove_account(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    account_id: String,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    // The watcher owns a live connection for this account, so it has to go
    // before the credential it authenticated with does.
    crate::mail_account::stop_all_imap_watchers_for_account(&account_id);
    crate::db::remove_account_data(&app, &account_id)?;
    crate::mail_account::delete_stored_password(&account_id)?;
    crate::db::delete_tokens(&account_id)
}

#[tauri::command]
pub async fn refresh_access_token(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    account_id: String,
) -> Result<crate::db::AuthInfo, String> {
    crate::require_command_window(&window, &["main"])?;
    refresh_session(app, &account_id).await
}

/// The one way to renew a session, for the frontend command and for the sync,
/// send, and watcher paths alike. Everything goes through the single-flight so
/// an account is never refreshed twice at once: a provider that rotates its
/// refresh token hands out one replacement, and a second refresh racing the
/// first would store the one the server has already retired.
pub(crate) async fn refresh_session(
    app: tauri::AppHandle,
    account_id: &str,
) -> Result<crate::db::AuthInfo, String> {
    let account_id = account_id.to_string();
    let flights = app.state::<crate::TokenRefreshFlights>();
    if let Some(waiter) = flights.join_or_start(&account_id) {
        return waiter
            .await
            .map_err(|_| "Token refresh was interrupted".to_string())?;
    }

    struct RefreshLeaderGuard<'a> {
        flights: &'a crate::TokenRefreshFlights,
        account_id: String,
        completed: bool,
    }

    impl RefreshLeaderGuard<'_> {
        fn complete(&mut self, result: Result<crate::db::AuthInfo, String>) {
            self.flights.finish(&self.account_id, result);
            self.completed = true;
        }
    }

    impl Drop for RefreshLeaderGuard<'_> {
        fn drop(&mut self) {
            if !self.completed {
                self.flights.finish(
                    &self.account_id,
                    Err("Token refresh was interrupted".to_string()),
                );
            }
        }
    }

    let mut leader = RefreshLeaderGuard {
        flights: &flights,
        account_id: account_id.clone(),
        completed: false,
    };
    let result = refresh_access_token_once(app.clone(), &account_id).await;
    leader.complete(result.clone());
    result
}

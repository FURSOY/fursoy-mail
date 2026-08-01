use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::Manager;
use tauri_plugin_opener::OpenerExt;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::{timeout, Duration};

const REDIRECT_URI: &str = "http://127.0.0.1:8123/callback";
const MAX_CALLBACK_REQUEST_LINE: usize = 8 * 1024;
const CALLBACK_READ_TIMEOUT: Duration = Duration::from_secs(5);
const OAUTH_SCOPES: &str = "https://www.googleapis.com/auth/gmail.modify \
                           https://www.googleapis.com/auth/userinfo.profile \
                           https://www.googleapis.com/auth/userinfo.email";

#[derive(Default)]
pub struct OAuthFlowState {
    cancel_sender: std::sync::Mutex<Option<oneshot::Sender<()>>>,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn generate_random_string(len: usize) -> String {
    use rand::{distributions::Alphanumeric, Rng};
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

fn generate_pkce_pair() -> (String, String) {
    let verifier = generate_random_string(64);
    let hash = Sha256::digest(verifier.as_bytes());
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash);
    (verifier, challenge)
}

// ── Credentials ──────────────────────────────────────────────────────────────

fn read_credential(name: &str, embedded: Option<&str>) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .or_else(|| embedded.map(str::to_string))
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{name} bulunamadi. Release build almadan once src-tauri/.env dosyasini kontrol edin."))
}

fn get_client_id() -> Result<String, String> {
    read_credential("GOOGLE_CLIENT_ID", option_env!("GOOGLE_CLIENT_ID"))
}

fn get_client_secret() -> Result<String, String> {
    read_credential("GOOGLE_CLIENT_SECRET", option_env!("GOOGLE_CLIENT_SECRET"))
}

// ── OAuth URL ─────────────────────────────────────────────────────────────────

fn build_auth_url(client_id: &str, state: &str, code_challenge: &str) -> Result<String, String> {
    let mut url = reqwest::Url::parse("https://accounts.google.com/o/oauth2/v2/auth")
        .map_err(|e| e.to_string())?;
    url.query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("response_type", "code")
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent")
        .append_pair("state", state)
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("scope", OAUTH_SCOPES);
    Ok(url.to_string())
}

fn open_auth_url(app: &tauri::AppHandle, auth_url: String) -> Result<(), String> {
    match app.opener().open_url(auth_url.clone(), None::<&str>) {
        Ok(_) => Ok(()),
        Err(plugin_error) => {
            #[cfg(target_os = "windows")]
            {
                std::process::Command::new("cmd")
                    .args(["/C", "start", "", &auth_url])
                    .spawn()
                    .map_err(|fallback_error| {
                        format!("Tarayici acilamadi. Opener: {plugin_error}. Fallback: {fallback_error}")
                    })?;
                return Ok(());
            }

            #[cfg(not(target_os = "windows"))]
            {
                Err(format!("Tarayici acilamadi: {plugin_error}"))
            }
        }
    }
}

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
pub struct AuthResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: i64,
    pub token_type: Option<String>,
    pub scope: Option<String>,
    pub refresh_token_expires_in: Option<i64>,
}

#[derive(Deserialize)]
struct UserInfo {
    email: String,
    picture: Option<String>,
}

fn token_expiry(expires_in: i64) -> Result<i64, String> {
    if expires_in <= 0 {
        return Err("Google geçersiz bir token süresi döndürdü.".to_string());
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "Sistem saati geçersiz.".to_string())?
        .as_secs() as i64;
    now.checked_add(expires_in)
        .ok_or_else(|| "Token süresi hesaplanamadı.".to_string())
}

fn validate_token_response(response: &AuthResponse, require_scopes: bool) -> Result<(), String> {
    if response.access_token.trim().is_empty() {
        return Err("Google boş bir access token döndürdü.".to_string());
    }
    if response
        .token_type
        .as_deref()
        .is_some_and(|token_type| !token_type.eq_ignore_ascii_case("Bearer"))
    {
        return Err("Google desteklenmeyen bir token türü döndürdü.".to_string());
    }
    if require_scopes {
        let granted = response
            .scope
            .as_deref()
            .ok_or_else(|| "Google verilen izin kapsamlarını döndürmedi.".to_string())?;
        for required in OAUTH_SCOPES.split_whitespace() {
            if !granted.split_whitespace().any(|scope| scope == required) {
                return Err(format!("Gerekli Google izni verilmedi: {required}"));
            }
        }
    }
    Ok(())
}

// ── Callback HTML ─────────────────────────────────────────────────────────────

const SUCCESS_HTML: &str = "\
<html><body style='display:flex;justify-content:center;align-items:center;\
height:100vh;background:#09090b;color:#fff;font-family:sans-serif;'>\
<h2>Sign-in successful! You can close this tab.</h2>\
<script>window.close();</script></body></html>";

const CSRF_HTML: &str = "\
<html><body style='display:flex;justify-content:center;align-items:center;\
height:100vh;background:#09090b;color:#f87171;font-family:sans-serif;'>\
<h2>A security error was detected. Please close this tab and try again.</h2></body></html>";

const CANCELLED_HTML: &str = "\
<html><body style='display:flex;justify-content:center;align-items:center;\
height:100vh;background:#09090b;color:#fff;font-family:sans-serif;'>\
<h2>Sign-in cancelled. You can close this tab.</h2>\
<script>window.close();</script></body></html>";

// ── Commands ──────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn start_google_oauth(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<crate::db::AuthInfo, String> {
    crate::require_command_window(&window, &["main"])?;
    let client_id = get_client_id()?;

    let expected_state = generate_random_string(32);
    let (code_verifier, code_challenge) = generate_pkce_pair();
    let auth_url = build_auth_url(&client_id, &expected_state, &code_challenge)?;

    let listener = TcpListener::bind("127.0.0.1:8123")
        .await
        .map_err(|_| "Port 8123 kullanimda. Lutfen arkada acik kalan uygulamalari kapatin.")?;

    let (cancel_sender, mut cancel_receiver) = oneshot::channel();
    if let Some(previous) = app
        .state::<OAuthFlowState>()
        .cancel_sender
        .lock()
        .map_err(|_| "OAuth cancellation state is unavailable.")?
        .replace(cancel_sender)
    {
        let _ = previous.send(());
    }

    if let Err(error) = open_auth_url(&app, auth_url) {
        let _ = app
            .state::<OAuthFlowState>()
            .cancel_sender
            .lock()
            .map(|mut sender| sender.take());
        return Err(error);
    }

    let callback_result = timeout(Duration::from_secs(120), async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                continue;
            };

            let reader = BufReader::new(&mut stream);
            let mut request_line = String::new();
            let mut limited_reader =
                reader.take((MAX_CALLBACK_REQUEST_LINE.saturating_add(1)) as u64);
            let read_result = timeout(
                CALLBACK_READ_TIMEOUT,
                limited_reader.read_line(&mut request_line),
            )
            .await;
            drop(limited_reader);

            if !matches!(read_result, Ok(Ok(bytes)) if bytes > 0)
                || request_line.len() > MAX_CALLBACK_REQUEST_LINE
                || !request_line.ends_with('\n')
            {
                let _ = stream
                    .write_all(b"HTTP/1.1 414 URI Too Long\r\nConnection: close\r\n\r\n")
                    .await;
                continue;
            }

            if !request_line.starts_with("GET ") {
                let _ = stream
                    .write_all(b"HTTP/1.1 404 Not Found\r\nConnection: close\r\n\r\n")
                    .await;
                continue;
            }

            let after_get = &request_line[4..];
            let path_end = after_get.find(' ').unwrap_or(after_get.len());
            let full_url = format!("http://localhost:8123{}", &after_get[..path_end]);

            let url = match reqwest::Url::parse(&full_url) {
                Ok(u) => u,
                Err(_) => continue,
            };
            if url.path() != "/callback" {
                let _ = stream
                    .write_all(b"HTTP/1.1 404 Not Found\r\nConnection: close\r\n\r\n")
                    .await;
                continue;
            }

            let mut code_val = String::new();
            let mut state_val = String::new();
            let mut oauth_error = String::new();
            for (k, v) in url.query_pairs() {
                match k.as_ref() {
                    "code" => code_val = v.into_owned(),
                    "state" => state_val = v.into_owned(),
                    "error" => oauth_error = v.into_owned(),
                    _ => {}
                }
            }

            if (!code_val.is_empty() || !oauth_error.is_empty()) && state_val != expected_state {
                let resp = format!(
                    "HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\
                         Content-Type: text/html; charset=utf-8\r\n\r\n{}",
                    CSRF_HTML
                );
                let _ = stream.write_all(resp.as_bytes()).await;
                let _ = stream.flush().await;
                drop(listener);
                return Err(
                    "Güvenlik hatası: Oturum doğrulaması başarısız. Lütfen tekrar deneyin."
                        .to_string(),
                );
            }

            if !oauth_error.is_empty() {
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nConnection: close\r\n\
                         Content-Type: text/html; charset=utf-8\r\n\r\n{}",
                    CANCELLED_HTML
                );
                let _ = stream.write_all(resp.as_bytes()).await;
                let _ = stream.flush().await;
                drop(listener);
                return Err(if oauth_error == "access_denied" {
                    "oauth_cancelled".to_string()
                } else {
                    format!("Google OAuth error: {oauth_error}")
                });
            }

            if code_val.is_empty() {
                continue;
            }

            let resp = format!(
                "HTTP/1.1 200 OK\r\nConnection: close\r\n\
                     Content-Type: text/html; charset=utf-8\r\n\r\n{}",
                SUCCESS_HTML
            );
            let _ = stream.write_all(resp.as_bytes()).await;
            let _ = stream.flush().await;
            drop(listener);
            return Ok(code_val);
        }
    });

    let code_result = tokio::select! {
        result = callback_result => result,
        _ = &mut cancel_receiver => {
            let _ = app.state::<OAuthFlowState>().cancel_sender.lock().map(|mut sender| sender.take());
            return Err("oauth_cancelled".into());
        }
    };
    let _ = app
        .state::<OAuthFlowState>()
        .cancel_sender
        .lock()
        .map(|mut sender| sender.take());

    let code = match code_result {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => return Err(e),
        _ => return Err("Giris islemi zaman asimina ugradi.".into()),
    };

    if code.is_empty() {
        return Err("Auth code bulunamadi".into());
    }

    let auth_resp = exchange_code_for_token(&code, &code_verifier).await?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    let user_res = client
        .get("https://www.googleapis.com/oauth2/v2/userinfo")
        .bearer_auth(&auth_resp.access_token)
        .send()
        .await
        .map_err(|_| "Google kullanici bilgisi istegi basarisiz oldu.".to_string())?;

    if !user_res.status().is_success() {
        return Err(format!(
            "Google kullanıcı bilgisi alınamadı: {}",
            user_res.status()
        ));
    }
    let user_info: UserInfo = user_res
        .json()
        .await
        .map_err(|_| "Google kullanici bilgisi yaniti okunamadi.".to_string())?;
    if user_info.email.trim().is_empty() {
        return Err("Google hesabı e-posta adresi döndürmedi.".to_string());
    }
    let picture = user_info.picture.unwrap_or_default();

    // Keep the previous credential so a later database failure can restore it.
    let previous_tokens = crate::db::load_tokens(&user_info.email);
    let existing_refresh = previous_tokens
        .as_ref()
        .map(|tokens| tokens.refresh_token.clone())
        .unwrap_or_default();
    let refresh_token = auth_resp.refresh_token.unwrap_or(existing_refresh);
    if refresh_token.is_empty() {
        return Err(
            "Google refresh token döndürmedi. Lütfen erişimi yeniden onaylayın.".to_string(),
        );
    }
    let expires_at = token_expiry(auth_resp.expires_in)?;

    // Persist tokens to keyring
    let new_tokens = crate::db::StoredTokens {
        access_token: auth_resp.access_token.clone(),
        refresh_token,
        expires_at: Some(expires_at),
    };
    crate::db::save_tokens(&user_info.email, &new_tokens)?;

    // Create or update account record
    let account = match crate::db::upsert_account(&app, &user_info.email, &picture) {
        Ok(account) => account,
        Err(error) => {
            if let Some(tokens) = previous_tokens {
                let _ = crate::db::save_tokens(&user_info.email, &tokens);
            } else {
                let _ = crate::db::delete_tokens(&user_info.email);
            }
            return Err(error);
        }
    };

    Ok(crate::db::AuthInfo {
        authenticated: true,
        expires_at: Some(expires_at),
        email: account.email,
        picture: account.picture,
    })
}

async fn exchange_code_for_token(code: &str, code_verifier: &str) -> Result<AuthResponse, String> {
    let client_id = get_client_id()?;
    let client_secret = get_client_secret()?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    let params = [
        ("client_id", client_id.as_str()),
        ("client_secret", client_secret.as_str()),
        ("code", code),
        ("grant_type", "authorization_code"),
        ("redirect_uri", REDIRECT_URI),
        ("code_verifier", code_verifier),
    ];

    let res = client
        .post("https://oauth2.googleapis.com/token")
        .form(&params)
        .send()
        .await
        .map_err(|_| "Google token istegi basarisiz oldu.".to_string())?;

    if res.status().is_success() {
        let auth_resp: AuthResponse = res
            .json()
            .await
            .map_err(|_| "Google token yaniti okunamadi.".to_string())?;
        validate_token_response(&auth_resp, true)?;
        Ok(auth_resp)
    } else {
        Err(format!("Token alinamadi (HTTP {}).", res.status()))
    }
}

async fn refresh_access_token_once(
    app: tauri::AppHandle,
    account_id: &str,
) -> Result<crate::db::AuthInfo, String> {
    if crate::db::get_account_provider(&app, account_id)? == "imap"
        && crate::db::load_tokens(account_id).is_some()
    {
        return crate::mail_oauth::refresh_mail_oauth_token(app, account_id).await;
    }
    let stored_tokens = crate::db::load_tokens(account_id)
        .ok_or_else(|| "Oturum bilgisi bulunamadı. Lütfen tekrar giriş yapın.".to_string())?;
    let refresh_token = stored_tokens.refresh_token;

    if refresh_token.is_empty() {
        return Err("No refresh token available. Please login again.".into());
    }

    let client_id = get_client_id()?;
    let client_secret = get_client_secret()?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    let params = [
        ("client_id", client_id.as_str()),
        ("client_secret", client_secret.as_str()),
        ("refresh_token", refresh_token.as_str()),
        ("grant_type", "refresh_token"),
    ];

    let res = client
        .post("https://oauth2.googleapis.com/token")
        .form(&params)
        .send()
        .await
        .map_err(|_| "Google token yenileme istegi basarisiz oldu.".to_string())?;

    if !res.status().is_success() {
        return Err(format!("Token refresh failed (HTTP {}).", res.status()));
    }

    let token_resp: AuthResponse = res
        .json()
        .await
        .map_err(|_| "Google token yenileme yaniti okunamadi.".to_string())?;
    validate_token_response(&token_resp, false)?;
    let new_refresh = token_resp.refresh_token.unwrap_or(refresh_token);
    let expires_at = token_expiry(token_resp.expires_in)?;

    crate::db::save_tokens(
        account_id,
        &crate::db::StoredTokens {
            access_token: token_resp.access_token.clone(),
            refresh_token: new_refresh,
            expires_at: Some(expires_at),
        },
    )?;

    let picture = crate::db::get_account_picture(&app, account_id);

    Ok(crate::db::AuthInfo {
        authenticated: true,
        expires_at: Some(expires_at),
        email: account_id.to_string(),
        picture,
    })
}

async fn revoke_google_token(token: &str) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    let response = client
        .post("https://oauth2.googleapis.com/revoke")
        .form(&[("token", token)])
        .send()
        .await
        .map_err(|_| "Google oturumu iptal edilemedi.".to_string())?;
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        let detail = response.text().await.unwrap_or_default();
        let already_invalid = serde_json::from_str::<serde_json::Value>(&detail)
            .ok()
            .and_then(|value| value["error"].as_str().map(str::to_string))
            .is_some_and(|error| error == "invalid_token");
        if already_invalid {
            Ok(())
        } else {
            Err(format!("Google oturumu iptal edilemedi ({status})."))
        }
    }
}

#[tauri::command]
pub async fn remove_account(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    account_id: String,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    if crate::db::get_account_provider(&app, &account_id)? == "imap" {
        crate::db::remove_account_data(&app, &account_id)?;
        crate::mail_account::delete_stored_password(&account_id)?;
        return crate::db::delete_tokens(&account_id);
    }
    if let Some(tokens) = crate::db::load_tokens(&account_id) {
        let token_to_revoke = if tokens.refresh_token.is_empty() {
            tokens.access_token.as_str()
        } else {
            tokens.refresh_token.as_str()
        };
        revoke_google_token(token_to_revoke).await?;
    }
    crate::db::remove_account_data(&app, &account_id)
}

#[tauri::command]
pub async fn refresh_access_token(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    account_id: String,
) -> Result<crate::db::AuthInfo, String> {
    crate::require_command_window(&window, &["main"])?;
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

#[tauri::command]
pub fn cancel_google_oauth(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, OAuthFlowState>,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    if let Some(sender) = state
        .cancel_sender
        .lock()
        .map_err(|_| "OAuth cancellation state is unavailable.")?
        .take()
    {
        let _ = sender.send(());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_url_requests_only_the_required_scopes() {
        let url = build_auth_url("client-id", "state", "challenge").expect("auth URL");
        let parsed = reqwest::Url::parse(&url).expect("valid auth URL");
        let scope = parsed
            .query_pairs()
            .find_map(|(key, value)| (key == "scope").then(|| value.into_owned()))
            .expect("scope query parameter");
        let scopes: Vec<_> = scope.split_whitespace().collect();

        assert_eq!(
            scopes,
            vec![
                "https://www.googleapis.com/auth/gmail.modify",
                "https://www.googleapis.com/auth/userinfo.profile",
                "https://www.googleapis.com/auth/userinfo.email",
            ]
        );
        assert!(!scope.contains("gmail.send"));
    }

    #[test]
    fn token_response_requires_bearer_and_all_requested_scopes() {
        let valid = AuthResponse {
            access_token: "access".to_string(),
            refresh_token: Some("refresh".to_string()),
            expires_in: 3600,
            token_type: Some("Bearer".to_string()),
            scope: Some(OAUTH_SCOPES.to_string()),
            refresh_token_expires_in: None,
        };
        assert!(validate_token_response(&valid, true).is_ok());

        let wrong_type = AuthResponse {
            token_type: Some("MAC".to_string()),
            ..valid
        };
        assert!(validate_token_response(&wrong_type, true).is_err());

        let missing_scope = AuthResponse {
            token_type: Some("Bearer".to_string()),
            scope: Some("https://www.googleapis.com/auth/userinfo.email".to_string()),
            ..wrong_type
        };
        assert!(validate_token_response(&missing_scope, true).is_err());
    }
}

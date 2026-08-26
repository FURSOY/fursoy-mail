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
const GOOGLE_SCOPES: &str = "openid email profile https://mail.google.com/";
const MICROSOFT_SCOPES: &str = "openid email profile offline_access https://outlook.office.com/IMAP.AccessAsUser.All https://outlook.office.com/SMTP.Send";

#[derive(Default)]
pub struct MailOAuthFlowState {
    cancel_sender: std::sync::Mutex<Option<oneshot::Sender<()>>>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveredProvider {
    Google,
    Microsoft,
    Yahoo,
    Icloud,
    Manual,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDiscovery {
    pub email: String,
    pub provider: DiscoveredProvider,
    pub auth_type: &'static str,
}

#[derive(Debug, Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: i64,
    id_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleUserInfo {
    email: String,
    picture: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MicrosoftIdClaims {
    nonce: Option<String>,
    preferred_username: Option<String>,
    email: Option<String>,
}

fn normalize_email(value: &str) -> Result<String, String> {
    let email = value.trim().to_lowercase();
    let Some((local, domain)) = email.rsplit_once('@') else {
        return Err("mail_account_invalid_email".to_string());
    };
    let domain_valid = !domain.is_empty()
        && domain.len() <= 253
        && domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        });
    if local.is_empty() || local.len() > 64 || !domain_valid {
        return Err("mail_account_invalid_email".to_string());
    }
    Ok(email)
}

fn provider_for_domain(domain: &str) -> DiscoveredProvider {
    if matches!(domain, "gmail.com" | "googlemail.com") {
        DiscoveredProvider::Google
    } else if matches!(
        domain,
        "outlook.com" | "hotmail.com" | "live.com" | "msn.com"
    ) || domain.starts_with("outlook.")
        || domain.starts_with("hotmail.")
    {
        DiscoveredProvider::Microsoft
    } else if matches!(domain, "ymail.com" | "rocketmail.com") || domain.starts_with("yahoo.") {
        DiscoveredProvider::Yahoo
    } else if matches!(domain, "icloud.com" | "me.com" | "mac.com") {
        DiscoveredProvider::Icloud
    } else {
        DiscoveredProvider::Manual
    }
}

fn is_personal_microsoft_domain(domain: &str) -> bool {
    matches!(
        domain,
        "outlook.com" | "hotmail.com" | "live.com" | "msn.com"
    ) || domain.starts_with("outlook.")
        || domain.starts_with("hotmail.")
}

#[tauri::command]
pub fn discover_mail_provider(
    window: tauri::WebviewWindow,
    email: String,
) -> Result<ProviderDiscovery, String> {
    crate::require_command_window(&window, &["main"])?;
    let email = normalize_email(&email)?;
    let domain = email
        .rsplit_once('@')
        .map(|(_, value)| value)
        .unwrap_or_default();
    let provider = provider_for_domain(domain);
    let auth_type = match provider {
        DiscoveredProvider::Google | DiscoveredProvider::Microsoft => "oauth",
        DiscoveredProvider::Yahoo | DiscoveredProvider::Icloud => "app_password",
        DiscoveredProvider::Manual => "manual",
    };
    Ok(ProviderDiscovery {
        email,
        provider,
        auth_type,
    })
}

fn random_string(len: usize) -> String {
    use rand::{distributions::Alphanumeric, Rng};
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

fn pkce_pair() -> (String, String) {
    let verifier = random_string(64);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

fn credential(name: &str, embedded: Option<&str>) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .or_else(|| embedded.map(str::to_string))
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "mail_oauth_missing_credential".to_string())
}

fn optional_credential(name: &str, embedded: Option<&str>) -> Option<String> {
    std::env::var(name)
        .ok()
        .or_else(|| embedded.map(str::to_string))
        .filter(|value| !value.trim().is_empty())
}

/// Microsoft sign-in needs this build's own application registration, the same
/// way Google does. There used to be a hard-coded fallback here, which could
/// only ever fail: an application id belonging to somebody else does not list
/// this app's redirect address, so Microsoft refuses the request in the browser
/// and the sign-in waits three minutes for a reply that never comes. Saying so
/// straight away is the whole improvement.
fn microsoft_client_id() -> Result<String, String> {
    credential("MICROSOFT_CLIENT_ID", option_env!("MICROSOFT_CLIENT_ID"))
}

/// `force_consent` decides whether Google is asked for the consent screen. It
/// is the only way to be handed a refresh token, but it also spends one of the
/// hundred a client may hold per account, so it is asked for exactly when this
/// app has no working refresh token left to fall back on.
fn build_authorization_url(
    provider: DiscoveredProvider,
    email: &str,
    state: &str,
    nonce: &str,
    challenge: &str,
    force_consent: bool,
) -> Result<String, String> {
    let (endpoint, client_id, scopes) = match provider {
        DiscoveredProvider::Google => (
            "https://accounts.google.com/o/oauth2/v2/auth",
            credential("GOOGLE_CLIENT_ID", option_env!("GOOGLE_CLIENT_ID"))?,
            GOOGLE_SCOPES,
        ),
        DiscoveredProvider::Microsoft => (
            "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
            microsoft_client_id()?,
            MICROSOFT_SCOPES,
        ),
        _ => return Err("mail_oauth_provider_not_supported".to_string()),
    };
    let mut url =
        reqwest::Url::parse(endpoint).map_err(|_| "mail_oauth_invalid_url".to_string())?;
    url.query_pairs_mut()
        .append_pair("client_id", &client_id)
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("response_type", "code")
        .append_pair("scope", scopes)
        .append_pair("state", state)
        .append_pair("nonce", nonce)
        .append_pair("login_hint", email)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair(
            "prompt",
            if provider == DiscoveredProvider::Google && force_consent {
                "consent"
            } else {
                "select_account"
            },
        );
    if provider == DiscoveredProvider::Google {
        url.query_pairs_mut().append_pair("access_type", "offline");
    }
    Ok(url.to_string())
}

fn open_authorization_url(app: &tauri::AppHandle, url: String) -> Result<(), String> {
    app.opener()
        .open_url(url, None::<&str>)
        .map(|_| ())
        .map_err(|_| "mail_oauth_browser_failed".to_string())
}

async fn wait_for_callback(
    app: &tauri::AppHandle,
    expected_state: String,
    authorization_url: String,
) -> Result<String, String> {
    let listener = TcpListener::bind("127.0.0.1:8123")
        .await
        .map_err(|_| "mail_oauth_callback_port_busy".to_string())?;
    let (cancel_sender, mut cancel_receiver) = oneshot::channel();
    if let Some(previous) = app
        .state::<MailOAuthFlowState>()
        .cancel_sender
        .lock()
        .map_err(|_| "mail_oauth_state_failed".to_string())?
        .replace(cancel_sender)
    {
        let _ = previous.send(());
    }
    open_authorization_url(app, authorization_url)?;

    let callback = timeout(Duration::from_secs(180), async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                continue;
            };
            let reader = BufReader::new(&mut stream);
            let mut request_line = String::new();
            let mut limited = reader.take((MAX_CALLBACK_REQUEST_LINE + 1) as u64);
            let read = timeout(CALLBACK_READ_TIMEOUT, limited.read_line(&mut request_line)).await;
            drop(limited);
            if !matches!(read, Ok(Ok(bytes)) if bytes > 0)
                || request_line.len() > MAX_CALLBACK_REQUEST_LINE
                || !request_line.starts_with("GET ")
            {
                continue;
            }
            let target = request_line[4..]
                .split_whitespace()
                .next()
                .unwrap_or_default();
            let Ok(url) = reqwest::Url::parse(&format!("http://localhost:8123{target}")) else {
                continue;
            };
            if url.path() != "/callback" {
                continue;
            }
            let mut code = String::new();
            let mut state = String::new();
            let mut oauth_error = String::new();
            for (key, value) in url.query_pairs() {
                match key.as_ref() {
                    "code" => code = value.into_owned(),
                    "state" => state = value.into_owned(),
                    "error" => oauth_error = value.into_owned(),
                    _ => {}
                }
            }
            if state != expected_state {
                let _ = stream
                    .write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n")
                    .await;
                return Err("mail_oauth_state_mismatch".to_string());
            }
            if !oauth_error.is_empty() {
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<script>window.close()</script>")
                    .await;
                return Err(if oauth_error == "access_denied" {
                    "oauth_cancelled".to_string()
                } else {
                    "mail_oauth_authorization_failed".to_string()
                });
            }
            if code.is_empty() {
                continue;
            }
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<script>window.close()</script>")
                .await;
            return Ok(code);
        }
    });

    let result = tokio::select! {
        result = callback => result,
        _ = &mut cancel_receiver => return Err("oauth_cancelled".to_string()),
    };
    let _ = app
        .state::<MailOAuthFlowState>()
        .cancel_sender
        .lock()
        .map(|mut sender| sender.take());
    match result {
        Ok(result) => result,
        Err(_) => Err("mail_oauth_timeout".to_string()),
    }
}

/// What a token endpoint's refusal says about the credential that was sent.
/// Only `Revoked` is worth a new sign-in for; the other two say nothing about
/// the stored refresh token, so they must not cost the user their session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenRequestError {
    /// The grant will never work again: revoked, expired, or superseded.
    Revoked,
    /// Nothing was learned — a network failure, throttling, or a server fault.
    Unavailable,
    /// A response arrived, but not one carrying a usable token.
    Invalid,
}

impl TokenRequestError {
    fn code(self) -> &'static str {
        match self {
            TokenRequestError::Revoked => "mail_oauth_refresh_revoked",
            TokenRequestError::Unavailable => "mail_oauth_token_unavailable",
            TokenRequestError::Invalid => "mail_oauth_token_invalid",
        }
    }
}

/// Both Google and Microsoft answer a dead grant with a 4xx carrying
/// `error: invalid_grant`. Everything else — 429, a 5xx, a captive portal's
/// HTML — describes the moment rather than the credential, and the stored
/// refresh token is still the right thing to try again with.
fn classify_token_failure(status: reqwest::StatusCode, body: &str) -> TokenRequestError {
    if !matches!(status.as_u16(), 400 | 401) {
        return TokenRequestError::Unavailable;
    }
    let error = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value["error"].as_str().map(str::to_string))
        .unwrap_or_default();
    if error == "invalid_grant" {
        TokenRequestError::Revoked
    } else {
        TokenRequestError::Unavailable
    }
}

async fn request_token(
    endpoint: &str,
    params: &[(&str, String)],
) -> Result<OAuthTokenResponse, TokenRequestError> {
    let response = reqwest::Client::new()
        .post(endpoint)
        .form(params)
        .send()
        .await
        .map_err(|_| TokenRequestError::Unavailable)?;
    let status = response.status();
    if !status.is_success() {
        // An OAuth error response is a code and a description; the request
        // carried the secrets, the failure body does not.
        let body = response.text().await.unwrap_or_default();
        return Err(classify_token_failure(status, &body));
    }
    let token = response
        .json::<OAuthTokenResponse>()
        .await
        .map_err(|_| TokenRequestError::Invalid)?;
    if token.access_token.is_empty() || token.expires_in <= 0 {
        return Err(TokenRequestError::Invalid);
    }
    Ok(token)
}

async fn exchange_code(
    provider: DiscoveredProvider,
    code: &str,
    verifier: &str,
) -> Result<OAuthTokenResponse, String> {
    let (endpoint, client_id) = match provider {
        DiscoveredProvider::Google => (
            "https://oauth2.googleapis.com/token",
            credential("GOOGLE_CLIENT_ID", option_env!("GOOGLE_CLIENT_ID"))?,
        ),
        DiscoveredProvider::Microsoft => (
            "https://login.microsoftonline.com/common/oauth2/v2.0/token",
            microsoft_client_id()?,
        ),
        _ => return Err("mail_oauth_provider_not_supported".to_string()),
    };
    let mut params = vec![
        ("client_id", client_id),
        ("code", code.to_string()),
        ("grant_type", "authorization_code".to_string()),
        ("redirect_uri", REDIRECT_URI.to_string()),
        ("code_verifier", verifier.to_string()),
    ];
    if provider == DiscoveredProvider::Google {
        if let Some(secret) =
            optional_credential("GOOGLE_CLIENT_SECRET", option_env!("GOOGLE_CLIENT_SECRET"))
        {
            params.push(("client_secret", secret));
        }
    }
    request_token(endpoint, &params)
        .await
        .map_err(|error| match error {
            TokenRequestError::Invalid => "mail_oauth_token_invalid".to_string(),
            // The user is standing in front of an interactive sign-in here, so
            // every other reason ends the same way: the attempt did not work.
            _ => "mail_oauth_token_failed".to_string(),
        })
}

pub(crate) async fn refresh_mail_oauth_token(
    app: tauri::AppHandle,
    account_id: &str,
) -> Result<crate::db::AuthInfo, String> {
    let settings = crate::db::get_imap_account_settings(&app, account_id)?;
    let provider = if settings.imap_host == "imap.gmail.com" {
        DiscoveredProvider::Google
    } else if settings.imap_host == "outlook.office365.com" {
        DiscoveredProvider::Microsoft
    } else {
        return Err("mail_oauth_provider_not_supported".to_string());
    };
    let stored = crate::db::load_tokens(account_id)
        .ok_or_else(|| "mail_oauth_refresh_token_missing".to_string())?;
    if stored.refresh_token.is_empty() {
        // The marker a revoked grant leaves behind. Asking the server again
        // would only repeat the same answer, so fail without the round trip.
        return Err("mail_oauth_refresh_revoked".to_string());
    }
    let (endpoint, client_id, scopes) = match provider {
        DiscoveredProvider::Google => (
            "https://oauth2.googleapis.com/token",
            credential("GOOGLE_CLIENT_ID", option_env!("GOOGLE_CLIENT_ID"))?,
            GOOGLE_SCOPES,
        ),
        DiscoveredProvider::Microsoft => (
            "https://login.microsoftonline.com/common/oauth2/v2.0/token",
            microsoft_client_id()?,
            MICROSOFT_SCOPES,
        ),
        _ => return Err("mail_oauth_provider_not_supported".to_string()),
    };
    let mut params = vec![
        ("client_id", client_id),
        ("refresh_token", stored.refresh_token.clone()),
        ("grant_type", "refresh_token".to_string()),
        ("scope", scopes.to_string()),
    ];
    if provider == DiscoveredProvider::Google {
        if let Some(secret) =
            optional_credential("GOOGLE_CLIENT_SECRET", option_env!("GOOGLE_CLIENT_SECRET"))
        {
            params.push(("client_secret", secret));
        }
    }
    let token = request_token(endpoint, &params).await.map_err(|error| {
        if error == TokenRequestError::Revoked {
            // Record it, so every later attempt fails here instead of on the
            // network, and so the next sign-in asks for consent again.
            let _ = crate::db::mark_oauth_session_revoked(account_id);
        }
        error.code().to_string()
    })?;
    let expires_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "mail_oauth_token_invalid".to_string())?
        .as_secs() as i64
        + token.expires_in;
    crate::db::save_tokens(
        account_id,
        &crate::db::StoredTokens {
            access_token: token.access_token,
            // A provider that rotates its refresh tokens sends a new one;
            // Google sends none and keeps the old one alive. An empty string is
            // neither, and storing it would end the account's session for good.
            refresh_token: token
                .refresh_token
                .filter(|value| !value.is_empty())
                .unwrap_or(stored.refresh_token),
            expires_at: Some(expires_at),
        },
    )?;
    Ok(crate::db::AuthInfo {
        authenticated: true,
        expires_at: Some(expires_at),
        email: account_id.to_string(),
        picture: crate::db::get_account_picture(&app, account_id),
    })
}

async fn authorized_identity(
    provider: DiscoveredProvider,
    token: &OAuthTokenResponse,
    nonce: &str,
) -> Result<(String, String), String> {
    match provider {
        DiscoveredProvider::Google => {
            let response = reqwest::Client::new()
                .get("https://www.googleapis.com/oauth2/v2/userinfo")
                .bearer_auth(&token.access_token)
                .send()
                .await
                .map_err(|_| "mail_oauth_profile_failed".to_string())?;
            let profile = response
                .json::<GoogleUserInfo>()
                .await
                .map_err(|_| "mail_oauth_profile_failed".to_string())?;
            Ok((
                normalize_email(&profile.email)?,
                profile.picture.unwrap_or_default(),
            ))
        }
        DiscoveredProvider::Microsoft => {
            let id_token = token
                .id_token
                .as_deref()
                .ok_or_else(|| "mail_oauth_profile_failed".to_string())?;
            let payload = id_token
                .split('.')
                .nth(1)
                .ok_or_else(|| "mail_oauth_profile_failed".to_string())?;
            let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(payload)
                .map_err(|_| "mail_oauth_profile_failed".to_string())?;
            let claims: MicrosoftIdClaims = serde_json::from_slice(&bytes)
                .map_err(|_| "mail_oauth_profile_failed".to_string())?;
            if claims.nonce.as_deref() != Some(nonce) {
                return Err("mail_oauth_state_mismatch".to_string());
            }
            let email = normalize_email(
                claims
                    .preferred_username
                    .or(claims.email)
                    .as_deref()
                    .ok_or_else(|| "mail_oauth_profile_failed".to_string())?,
            )?;
            Ok((email, String::new()))
        }
        _ => Err("mail_oauth_provider_not_supported".to_string()),
    }
}

pub(crate) async fn refresh_google_profile_picture(
    app: &tauri::AppHandle,
    account_id: &str,
    access_token: &str,
) -> Result<(), String> {
    let response = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|_| "mail_oauth_profile_failed".to_string())?
        .get("https://www.googleapis.com/oauth2/v2/userinfo")
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|_| "mail_oauth_profile_failed".to_string())?;
    if !response.status().is_success() {
        return Err("mail_oauth_profile_failed".to_string());
    }
    let profile = response
        .json::<GoogleUserInfo>()
        .await
        .map_err(|_| "mail_oauth_profile_failed".to_string())?;
    if normalize_email(&profile.email)? != account_id {
        return Err("mail_oauth_email_mismatch".to_string());
    }
    if let Some(picture) = profile.picture.filter(|value| !value.is_empty()) {
        crate::db::set_account_picture(app, account_id, &picture)?;
    }
    Ok(())
}

fn account_input(
    provider: DiscoveredProvider,
    email: &str,
) -> Result<crate::mail_account::ImapAccountInput, String> {
    use crate::mail_account::{ImapAccountInput, MailSecurity};
    match provider {
        DiscoveredProvider::Google => Ok(ImapAccountInput {
            email: email.to_string(),
            username: email.to_string(),
            password: String::new(),
            imap_host: "imap.gmail.com".to_string(),
            imap_port: 993,
            imap_security: MailSecurity::Tls,
            smtp_host: "smtp.gmail.com".to_string(),
            smtp_port: 465,
            smtp_security: MailSecurity::Tls,
        }),
        DiscoveredProvider::Microsoft => {
            let domain = email
                .rsplit_once('@')
                .map(|(_, domain)| domain)
                .unwrap_or_default();
            Ok(ImapAccountInput {
                email: email.to_string(),
                username: email.to_string(),
                password: String::new(),
                imap_host: "outlook.office365.com".to_string(),
                imap_port: 993,
                imap_security: MailSecurity::Tls,
                smtp_host: if is_personal_microsoft_domain(domain) {
                    "smtp-mail.outlook.com".to_string()
                } else {
                    "smtp.office365.com".to_string()
                },
                smtp_port: 587,
                smtp_security: MailSecurity::Starttls,
            })
        }
        _ => Err("mail_oauth_provider_not_supported".to_string()),
    }
}

/// Says which step of a sign-in gave up. Only this app's own error codes and
/// the step's name are written — never a token, a code, or a password.
fn oauth_step_failed(step: &str, error: String) -> String {
    eprintln!("[OAUTH] {step} failed: {error}");
    error
}

#[tauri::command]
pub async fn start_mail_oauth(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    email: String,
    provider: DiscoveredProvider,
) -> Result<crate::db::AuthInfo, String> {
    crate::require_command_window(&window, &["main"])?;
    let email = normalize_email(&email)?;
    if !matches!(
        provider,
        DiscoveredProvider::Google | DiscoveredProvider::Microsoft
    ) {
        return Err("mail_oauth_provider_not_supported".to_string());
    }
    let state = random_string(32);
    let nonce = random_string(32);
    let (verifier, challenge) = pkce_pair();
    // Without a refresh token this app cannot renew anything, so the consent
    // screen has to run; with one, a silent re-authorization keeps it.
    let has_refresh_token = crate::db::load_tokens(&email)
        .is_some_and(|tokens| !tokens.refresh_token.is_empty());
    let authorization_url = build_authorization_url(
        provider,
        &email,
        &state,
        &nonce,
        &challenge,
        !has_refresh_token,
    )?;
    // A sign-in that never comes back looks exactly like one that was never
    // started, so both ends of the wait say so.
    eprintln!("[OAUTH] {provider:?}: waiting for the browser to come back");
    let code = wait_for_callback(&app, state, authorization_url)
        .await
        .map_err(|error| oauth_step_failed("callback", error))?;
    eprintln!("[OAUTH] {provider:?}: browser returned, exchanging the code");
    let token = exchange_code(provider, &code, &verifier)
        .await
        .map_err(|error| oauth_step_failed("token exchange", error))?;
    let (authorized_email, picture) = authorized_identity(provider, &token, &nonce)
        .await
        .map_err(|error| oauth_step_failed("identity", error))?;
    if authorized_email != email {
        // The address the provider signed in is the one the mailbox belongs to,
        // and it is not a secret the user does not already know.
        eprintln!("[OAUTH] signed in as {authorized_email}, but {email} was asked for");
        return Err("mail_oauth_email_mismatch".to_string());
    }

    let input = account_input(provider, &email)?;
    eprintln!(
        "[OAUTH] {provider:?}: signing in to {} and {}",
        input.imap_host, input.smtp_host
    );
    crate::mail_account::test_oauth_mail_account(input.clone(), token.access_token.clone())
        .await
        .map_err(|error| oauth_step_failed("mailbox sign-in", error))?;
    eprintln!("[OAUTH] {provider:?}: signed in");
    let previous_tokens = crate::db::load_tokens(&email);
    let previous_password = crate::mail_account::load_stored_password(&email);
    let refresh_token = token
        .refresh_token
        .or_else(|| {
            previous_tokens
                .as_ref()
                .map(|tokens| tokens.refresh_token.clone())
        })
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "mail_oauth_refresh_token_missing".to_string())?;
    let expires_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "mail_oauth_token_invalid".to_string())?
        .as_secs() as i64
        + token.expires_in;
    crate::db::save_tokens(
        &email,
        &crate::db::StoredTokens {
            access_token: token.access_token,
            refresh_token,
            expires_at: Some(expires_at),
        },
    )?;
    crate::mail_account::delete_stored_password(&email)?;
    let settings = crate::db::ImapAccountSettings {
        account_id: email.clone(),
        username: input.username,
        imap_host: input.imap_host,
        imap_port: input.imap_port,
        imap_security: "tls".to_string(),
        smtp_host: input.smtp_host,
        smtp_port: input.smtp_port,
        smtp_security: if input.smtp_security == crate::mail_account::MailSecurity::Tls {
            "tls".to_string()
        } else {
            "starttls".to_string()
        },
    };
    let account = match crate::db::upsert_imap_account(&app, &settings) {
        Ok(account) => account,
        Err(error) => {
            if let Some(tokens) = previous_tokens {
                let _ = crate::db::save_tokens(&email, &tokens);
            } else {
                let _ = crate::db::delete_tokens(&email);
            }
            if let Some(password) = previous_password {
                let _ = crate::mail_account::save_stored_password(&email, &password);
            }
            return Err(error);
        }
    };
    if !picture.is_empty() {
        crate::db::set_account_picture(&app, &email, &picture)?;
    }
    Ok(crate::db::AuthInfo {
        authenticated: true,
        expires_at: Some(expires_at),
        email: account.email,
        picture,
    })
}

#[tauri::command]
pub fn cancel_mail_oauth(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, MailOAuthFlowState>,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    if let Some(sender) = state
        .cancel_sender
        .lock()
        .map_err(|_| "mail_oauth_state_failed".to_string())?
        .take()
    {
        let _ = sender.send(());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        account_input, build_authorization_url, classify_token_failure, normalize_email,
        provider_for_domain, DiscoveredProvider, TokenRequestError,
    };

    #[test]
    fn discovers_known_consumer_providers() {
        assert_eq!(provider_for_domain("gmail.com"), DiscoveredProvider::Google);
        assert_eq!(
            provider_for_domain("outlook.com"),
            DiscoveredProvider::Microsoft
        );
        assert_eq!(provider_for_domain("yahoo.com"), DiscoveredProvider::Yahoo);
        assert_eq!(
            provider_for_domain("icloud.com"),
            DiscoveredProvider::Icloud
        );
        assert_eq!(
            provider_for_domain("example.com"),
            DiscoveredProvider::Manual
        );
    }

    #[test]
    fn normalizes_discovery_email() {
        assert_eq!(
            normalize_email(" Person@Gmail.COM ").unwrap(),
            "person@gmail.com"
        );
        assert!(normalize_email("not-an-email").is_err());
    }

    #[test]
    fn only_a_rejected_grant_ends_the_session() {
        assert_eq!(
            classify_token_failure(
                reqwest::StatusCode::BAD_REQUEST,
                r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#
            ),
            TokenRequestError::Revoked
        );
        // A throttled or broken endpoint says nothing about the credential.
        assert_eq!(
            classify_token_failure(reqwest::StatusCode::TOO_MANY_REQUESTS, ""),
            TokenRequestError::Unavailable
        );
        assert_eq!(
            classify_token_failure(reqwest::StatusCode::INTERNAL_SERVER_ERROR, "<html>"),
            TokenRequestError::Unavailable
        );
        // A misconfigured client is not something a new sign-in can repair.
        assert_eq!(
            classify_token_failure(
                reqwest::StatusCode::UNAUTHORIZED,
                r#"{"error":"invalid_client"}"#
            ),
            TokenRequestError::Unavailable
        );
        assert_eq!(
            classify_token_failure(reqwest::StatusCode::BAD_REQUEST, "not json"),
            TokenRequestError::Unavailable
        );
    }

    #[test]
    fn consent_is_requested_only_without_a_refresh_token() {
        let with_consent =
            build_authorization_url(DiscoveredProvider::Google, "person@gmail.com", "s", "n", "c", true)
                .unwrap_or_default();
        let without_consent =
            build_authorization_url(DiscoveredProvider::Google, "person@gmail.com", "s", "n", "c", false)
                .unwrap_or_default();
        // A build without client credentials configured returns an error for
        // both, which would make this assertion vacuous.
        if with_consent.is_empty() || without_consent.is_empty() {
            return;
        }
        assert!(with_consent.contains("prompt=consent"));
        assert!(with_consent.contains("access_type=offline"));
        assert!(without_consent.contains("prompt=select_account"));
        assert!(without_consent.contains("access_type=offline"));
    }

    #[test]
    fn selects_microsoft_smtp_host_by_account_type() {
        let personal = account_input(DiscoveredProvider::Microsoft, "person@hotmail.com").unwrap();
        assert_eq!(personal.smtp_host, "smtp-mail.outlook.com");

        let organization =
            account_input(DiscoveredProvider::Microsoft, "person@example.com").unwrap();
        assert_eq!(organization.smtp_host, "smtp.office365.com");
    }
}

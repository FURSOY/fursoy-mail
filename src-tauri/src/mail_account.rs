use base64::Engine;
use imap::extensions::idle::WaitOutcome;
use lettre::{
    address::{Address, Envelope},
    transport::smtp::{
        authentication::{Credentials, Mechanism},
        client::Tls,
    },
    SmtpTransport, Transport,
};
use mailparse::MailHeaderMap;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

const IO_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_HOST_LEN: usize = 253;
const MAX_USERNAME_LEN: usize = 320;
/// How long the inbox may go without a full pass. Read and starred state can
/// change on another device without moving UIDNEXT or the message count, so the
/// cheap checkpoint comparison cannot see it. IDLE is supposed to report these
/// promptly but does not do so reliably across servers, and this bound is what
/// keeps the inbox correct when it does not. A pass costs one FLAGS listing and
/// downloads no message bodies.
const INBOX_RECONCILE_INTERVAL_SECS: i64 = 30;
/// The same bound for every other mailbox. Nothing watches them, and their flags
/// matter less moment to moment, so they trade freshness for a much lower cost.
const MAILBOX_RECONCILE_INTERVAL_SECS: i64 = 300;
/// The inbox bound while a watcher is parked on IDLE. The server then reports
/// flag changes as they happen, so the timer only has to cover what a wake could
/// not describe rather than carry freshness by itself.
const WATCHED_INBOX_RECONCILE_INTERVAL_SECS: i64 = 300;
/// The bound once CONDSTORE is carrying flag state. What is left for the timer
/// is an expunge the cached message count could not reveal, which needs a
/// message this app never managed to cache, so it stays rare.
const CONDSTORE_RECONCILE_INTERVAL_SECS: i64 = 3600;
/// How long a single IDLE wait runs before it is terminated and re-issued. RFC
/// 2177 allows 29 minutes; a shorter cycle also proves the connection is still
/// alive and bounds how long a stopped watcher keeps its socket open.
const IDLE_CYCLE: Duration = Duration::from_secs(60);
/// Backoff before a watcher whose connection failed tries again, so a server
/// that is down is not reconnected in a tight loop. It doubles up to
/// `WATCH_RETRY_MAX_DELAY` while failures continue: an outage or an expired
/// credential must not turn into a permanent reconnect loop, and it must not
/// end the watcher either, because nothing else would start it again.
const WATCH_RETRY_DELAY: Duration = Duration::from_secs(30);
const WATCH_RETRY_MAX_DELAY: Duration = Duration::from_secs(300);
/// Retry and stop are both checked on this granularity, which is what lets a
/// stopped watcher leave a backoff early.
const WATCH_STOP_POLL: Duration = Duration::from_secs(1);

/// Opt-in tracing for live sync work, off unless `FURSOY_IMAP_LOG` is set to
/// something other than `0`. Read once, so flipping it needs a restart and a
/// long-running watcher cannot start logging behind the user's back.
///
/// What may go through here is deliberately narrow: mailbox roles, plan
/// decisions, UIDs and counts. Never a header, a subject, an address, a body, a
/// filename, or anything derived from a credential.
fn imap_log_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("FURSOY_IMAP_LOG")
            .map(|value| !value.is_empty() && value != "0")
            .unwrap_or(false)
    })
}

fn imap_log(message: impl FnOnce() -> String) {
    if imap_log_enabled() {
        eprintln!("[IMAP] {}", message());
    }
}

fn unix_time_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MailSecurity {
    Tls,
    Starttls,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImapAccountInput {
    pub email: String,
    pub username: String,
    pub password: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_security: MailSecurity,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_security: MailSecurity,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailConnectionReport {
    pub imap_ok: bool,
    pub smtp_ok: bool,
    pub mailbox_count: usize,
}

/// Tells the frontend that an account's cache moved because of something the
/// server reported, so the views and the unread badge have to be re-read.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImapChangePayload {
    account_id: String,
}

impl ImapAccountInput {
    fn normalized(mut self) -> Result<Self, String> {
        self.email = self.email.trim().to_lowercase();
        self.username = self.username.trim().to_string();
        self.imap_host = normalize_host(&self.imap_host)?;
        self.smtp_host = normalize_host(&self.smtp_host)?;

        if !looks_like_email(&self.email) {
            return Err("mail_account_invalid_email".to_string());
        }
        if self.username.is_empty() || self.username.len() > MAX_USERNAME_LEN {
            return Err("mail_account_invalid_username".to_string());
        }
        if self.password.is_empty() {
            return Err("mail_account_password_required".to_string());
        }
        if self.imap_port == 0 || self.smtp_port == 0 {
            return Err("mail_account_invalid_port".to_string());
        }
        Ok(self)
    }
}

fn password_key(account_id: &str) -> String {
    format!("imap-password-{account_id}")
}

pub(crate) fn load_stored_password(account_id: &str) -> Option<String> {
    keyring::Entry::new("fursoy-mail", &password_key(account_id))
        .ok()?
        .get_password()
        .ok()
        .filter(|password| !password.is_empty())
}

pub(crate) fn has_stored_password(account_id: &str) -> bool {
    load_stored_password(account_id).is_some()
}

pub(crate) fn save_stored_password(account_id: &str, password: &str) -> Result<(), String> {
    keyring::Entry::new("fursoy-mail", &password_key(account_id))
        .and_then(|entry| entry.set_password(password))
        .map_err(|_| "mail_account_credential_store_failed".to_string())
}

pub(crate) fn delete_stored_password(account_id: &str) -> Result<(), String> {
    let entry = keyring::Entry::new("fursoy-mail", &password_key(account_id))
        .map_err(|_| "mail_account_credential_store_failed".to_string())?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err("mail_account_credential_delete_failed".to_string()),
    }
}

fn normalize_host(value: &str) -> Result<String, String> {
    let host = value.trim().trim_end_matches('.').to_lowercase();
    let valid = !host.is_empty()
        && host.len() <= MAX_HOST_LEN
        && !host.contains(char::is_whitespace)
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-');
    if valid {
        Ok(host)
    } else {
        Err("mail_account_invalid_host".to_string())
    }
}

fn looks_like_email(value: &str) -> bool {
    let Some((local, domain)) = value.rsplit_once('@') else {
        return false;
    };
    !local.is_empty() && normalize_host(domain).is_ok()
}

async fn test_imap(input: &ImapAccountInput) -> Result<usize, String> {
    let input = input.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut session = connect_password_imap(&input)?;
        session
            .select("INBOX")
            .map_err(|_| "mail_account_imap_failed".to_string())?;
        session.logout().ok();
        Ok(1)
    })
    .await
    .map_err(|_| "mail_account_test_interrupted".to_string())?
}

fn test_smtp(input: &ImapAccountInput) -> Result<(), String> {
    let credentials = Credentials::new(input.username.clone(), input.password.clone());
    let tls = match input.smtp_security {
        MailSecurity::Tls => Tls::Wrapper(
            lettre::transport::smtp::client::TlsParameters::new(input.smtp_host.clone())
                .map_err(|_| "mail_account_tls_failed".to_string())?,
        ),
        MailSecurity::Starttls => Tls::Required(
            lettre::transport::smtp::client::TlsParameters::new(input.smtp_host.clone())
                .map_err(|_| "mail_account_tls_failed".to_string())?,
        ),
    };
    let transport = SmtpTransport::builder_dangerous(&input.smtp_host)
        .port(input.smtp_port)
        .tls(tls)
        .credentials(credentials)
        .authentication(vec![Mechanism::Plain, Mechanism::Login])
        .timeout(Some(IO_TIMEOUT))
        .build();
    match transport.test_connection() {
        Ok(true) => Ok(()),
        Ok(false) => Err("mail_account_smtp_failed".to_string()),
        Err(_) => Err("mail_account_smtp_failed".to_string()),
    }
}

struct XOAuth2 {
    username: String,
    access_token: String,
}

impl imap::Authenticator for XOAuth2 {
    type Response = String;

    fn process(&self, _challenge: &[u8]) -> Self::Response {
        format!(
            "user={}\x01auth=Bearer {}\x01\x01",
            self.username, self.access_token
        )
    }
}

type OAuthImapSession = imap::Session<native_tls::TlsStream<TcpStream>>;

/// Microsoft rejects IMAP logins with the same generic failure whether the
/// credential is wrong or the account simply never had IMAP turned on
/// (Outlook.com/Microsoft 365 both default IMAP access to off) — the wire
/// response does not distinguish the two. Since every Microsoft account uses
/// this exact host (`mail_oauth.rs`), a login failure against it gets a
/// separate error code so the UI can add the "turn on IMAP" hint generic
/// auth failures for other providers should not carry.
fn is_outlook_imap_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("outlook.office365.com")
}

fn imap_auth_failed_code(host: &str) -> &'static str {
    if is_outlook_imap_host(host) {
        "mail_account_outlook_auth_failed"
    } else {
        "mail_account_auth_failed"
    }
}

fn connect_oauth_imap(
    input: &ImapAccountInput,
    access_token: &str,
) -> Result<OAuthImapSession, String> {
    let tls = native_tls::TlsConnector::builder()
        .build()
        .map_err(|_| "mail_account_tls_failed".to_string())?;
    let client = match input.imap_security {
        MailSecurity::Tls => imap::connect(
            (input.imap_host.as_str(), input.imap_port),
            &input.imap_host,
            &tls,
        ),
        MailSecurity::Starttls => imap::connect_starttls(
            (input.imap_host.as_str(), input.imap_port),
            &input.imap_host,
            &tls,
        ),
    }
    .map_err(|_| "mail_account_tls_failed".to_string())?;
    client
        .authenticate(
            "XOAUTH2",
            &XOAuth2 {
                username: input.username.clone(),
                access_token: access_token.to_string(),
            },
        )
        .map_err(|_| imap_auth_failed_code(&input.imap_host).to_string())
}

fn connect_password_imap(input: &ImapAccountInput) -> Result<OAuthImapSession, String> {
    let tls = native_tls::TlsConnector::builder()
        .build()
        .map_err(|_| "mail_account_tls_failed".to_string())?;
    let client = match input.imap_security {
        MailSecurity::Tls => imap::connect(
            (input.imap_host.as_str(), input.imap_port),
            &input.imap_host,
            &tls,
        ),
        MailSecurity::Starttls => imap::connect_starttls(
            (input.imap_host.as_str(), input.imap_port),
            &input.imap_host,
            &tls,
        ),
    }
    .map_err(|_| "mail_account_tls_failed".to_string())?;
    client
        .login(&input.username, &input.password)
        .map_err(|_| imap_auth_failed_code(&input.imap_host).to_string())
}

fn connect_sync_imap(
    input: &ImapAccountInput,
    oauth_token: Option<&str>,
) -> Result<OAuthImapSession, String> {
    match oauth_token {
        Some(token) => connect_oauth_imap(input, token),
        None => connect_password_imap(input),
    }
}

/// Whether the server advertised an extension. A server that did not answers a
/// command using it with an error, so this is asked once per connection and the
/// answer decides which commands the connection may use.
fn session_supports(session: &mut OAuthImapSession, extension: &str) -> bool {
    session
        .capabilities()
        .map(|capabilities| capabilities.has_str(extension))
        .unwrap_or(false)
}

fn fallback_mailbox_role(name: &str) -> Option<&'static str> {
    let leaf = name
        .rsplit(['/', '.'])
        .next()
        .unwrap_or(name)
        .trim()
        .to_ascii_lowercase();
    match leaf.as_str() {
        "inbox" => Some("inbox"),
        "sent" | "sent items" | "sent messages" | "sent mail" => Some("sent"),
        "draft" | "drafts" => Some("drafts"),
        "trash" | "deleted" | "deleted items" | "deleted messages" => Some("trash"),
        "junk" | "junk email" | "spam" => Some("junk"),
        "archive" | "archives" => Some("archive"),
        "all" | "all mail" => Some("all"),
        _ => None,
    }
}

/// Non-selectable path segments (e.g. a `[Gmail]` container folder) cannot be
/// SELECTed at all, so they must never become a syncable role.
fn mailbox_is_selectable(mailbox: &imap::types::Name) -> bool {
    !mailbox
        .attributes()
        .iter()
        .any(|attribute| matches!(attribute, imap::types::NameAttribute::NoSelect))
}

/// Resolves one LISTed mailbox to the role it will be discovered/cached
/// under, or `None` if it should not become a mailbox role at all.
/// Special-use beats name-based guessing. A folder that matches neither is
/// not lost anymore on a non-Gmail server — it keeps its own IMAP path as a
/// synthetic role, unique by construction (only two *recognized*-role
/// folders can still collide). On Gmail specifically, every mailbox beyond
/// the fixed system ones is a virtual per-label folder (Gmail's IMAP exposes
/// each label as its own LISTed mailbox in addition to `X-GM-LABELS`); those
/// are already covered by the label system, so turning them into "custom
/// folders" too would just duplicate every label as a sidebar folder.
fn resolve_mailbox_role(special_role: Option<&str>, name: &str, gmail_account: bool) -> Option<String> {
    if let Some(role) = special_role {
        return Some(role.to_string());
    }
    if let Some(role) = fallback_mailbox_role(name) {
        return Some(role.to_string());
    }
    if gmail_account {
        return None;
    }
    Some(format!("custom:{name}"))
}

/// Gmail's own built-in views (Starred, Important) are LISTed as ordinary
/// mailboxes too, marked only by these two special-use attributes — neither
/// is a role this app caches under, and neither is a real user label.
fn is_gmail_system_view(special_use: &str) -> bool {
    matches!(special_use, "\\flagged" | "\\important")
}

/// A mailbox LIST is the only signal left for a Gmail account's label set,
/// now that the REST label API is gone (`gmail.googleapis.com/labels` is
/// never called). Every mailbox that is not a recognized system role and not
/// one of Gmail's own built-in views (`is_gmail_system_view`) is a user
/// label, LISTed by Gmail as its own virtual folder in addition to being
/// reachable through `X-GM-LABELS`.
struct DiscoveredMailboxes {
    roles: Vec<(String, String)>,
    gmail_labels: Vec<String>,
}

fn discover_imap_mailboxes(
    session: &mut OAuthImapSession,
    gmail_account: bool,
) -> Result<DiscoveredMailboxes, String> {
    let listed = session
        .list(None, Some("*"))
        .map_err(|_| "mail_account_imap_failed".to_string())?;
    let mut roles = BTreeMap::<String, String>::new();
    let mut gmail_labels = Vec::new();
    for mailbox in listed.iter() {
        if !mailbox_is_selectable(mailbox) {
            continue;
        }
        let mut special_role = None;
        let mut system_view = false;
        for attribute in mailbox.attributes() {
            if let imap::types::NameAttribute::Custom(value) = attribute {
                let lower = value.to_ascii_lowercase();
                if is_gmail_system_view(&lower) {
                    system_view = true;
                }
                special_role = match lower.as_str() {
                    "\\sent" => Some("sent"),
                    "\\drafts" => Some("drafts"),
                    "\\trash" => Some("trash"),
                    "\\junk" => Some("junk"),
                    "\\archive" => Some("archive"),
                    "\\all" => Some("all"),
                    _ => special_role,
                };
            }
        }
        if system_view {
            continue;
        }
        match resolve_mailbox_role(special_role, mailbox.name(), gmail_account) {
            Some(role) => {
                roles
                    .entry(role)
                    .or_insert_with(|| mailbox.name().to_string());
            }
            None if gmail_account => gmail_labels.push(mailbox.name().to_string()),
            None => {}
        }
    }
    roles
        .entry("inbox".to_string())
        .or_insert_with(|| "INBOX".to_string());
    Ok(DiscoveredMailboxes {
        roles: roles.into_iter().collect(),
        gmail_labels,
    })
}

fn smtp_transport(
    input: &ImapAccountInput,
    secret: String,
    oauth: bool,
) -> Result<SmtpTransport, String> {
    let credentials = Credentials::new(input.username.clone(), secret);
    let tls = match input.smtp_security {
        MailSecurity::Tls => Tls::Wrapper(
            lettre::transport::smtp::client::TlsParameters::new(input.smtp_host.clone())
                .map_err(|_| "mail_account_tls_failed".to_string())?,
        ),
        MailSecurity::Starttls => Tls::Required(
            lettre::transport::smtp::client::TlsParameters::new(input.smtp_host.clone())
                .map_err(|_| "mail_account_tls_failed".to_string())?,
        ),
    };
    Ok(SmtpTransport::builder_dangerous(&input.smtp_host)
        .port(input.smtp_port)
        .tls(tls)
        .credentials(credentials)
        .authentication(if oauth {
            vec![Mechanism::Xoauth2]
        } else {
            vec![Mechanism::Plain, Mechanism::Login]
        })
        .timeout(Some(IO_TIMEOUT))
        .build())
}

pub(crate) async fn test_oauth_mail_account(
    input: ImapAccountInput,
    access_token: String,
) -> Result<MailConnectionReport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut imap = connect_oauth_imap(&input, &access_token)?;
        imap.select("INBOX")
            .map_err(|_| "mail_account_imap_failed".to_string())?;
        imap.logout().ok();
        let smtp = smtp_transport(&input, access_token, true)?;
        match smtp.test_connection() {
            Ok(true) => Ok(MailConnectionReport {
                imap_ok: true,
                smtp_ok: true,
                mailbox_count: 1,
            }),
            _ => Err("mail_account_smtp_failed".to_string()),
        }
    })
    .await
    .map_err(|_| "mail_account_test_interrupted".to_string())?
}

fn recipient_addresses(values: &[&str]) -> Result<Vec<Address>, String> {
    let mut result = Vec::new();
    for value in values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        let parsed = mailparse::addrparse(value)
            .map_err(|_| "mail_account_invalid_recipient".to_string())?;
        for address in parsed.iter() {
            match address {
                mailparse::MailAddr::Single(single) => result.push(
                    single
                        .addr
                        .parse()
                        .map_err(|_| "mail_account_invalid_recipient".to_string())?,
                ),
                mailparse::MailAddr::Group(group) => {
                    for single in &group.addrs {
                        result.push(
                            single
                                .addr
                                .parse()
                                .map_err(|_| "mail_account_invalid_recipient".to_string())?,
                        );
                    }
                }
            }
        }
    }
    if result.is_empty() {
        return Err("mail_account_invalid_recipient".to_string());
    }
    Ok(result)
}

pub async fn send_smtp_raw(
    app: &AppHandle,
    account_id: &str,
    to: &str,
    cc: &str,
    bcc: &str,
    raw_email: String,
) -> Result<(), String> {
    let input = stored_account_input(app, account_id)?;
    let oauth_token = account_oauth_token(app, account_id).await?;
    let recipients = recipient_addresses(&[to, cc, bcc])?;
    let sender: Address = account_id
        .parse()
        .map_err(|_| "mail_account_invalid_email".to_string())?;
    let envelope = Envelope::new(Some(sender), recipients)
        .map_err(|_| "mail_account_invalid_recipient".to_string())?;
    let sent_mailbox = crate::db::get_imap_mailboxes(app, account_id)?
        .into_iter()
        .find_map(|(role, mailbox)| (role == "sent").then_some(mailbox));
    let gmail_stores_sent = input.imap_host.eq_ignore_ascii_case("imap.gmail.com");
    tauri::async_runtime::spawn_blocking(move || {
        let (secret, oauth) = match oauth_token {
            Some(token) => (token, true),
            None => (input.password.clone(), false),
        };
        let transport = smtp_transport(&input, secret.clone(), oauth)?;
        transport
            .send_raw(&envelope, raw_email.as_bytes())
            .map_err(|_| "mail_account_smtp_send_failed".to_string())?;

        if !gmail_stores_sent {
            if let Some(mailbox) = sent_mailbox {
                let token = if oauth { Some(secret.as_str()) } else { None };
                if let Ok(mut session) = connect_sync_imap(&input, token) {
                    let _ = session.append_with_flags(
                        &mailbox,
                        raw_email.as_bytes(),
                        &[imap::types::Flag::Seen],
                    );
                    session.logout().ok();
                }
            }
        }
        Ok::<(), String>(())
    })
    .await
    .map_err(|_| "mail_account_send_interrupted".to_string())??;

    // Sending is already confirmed at this point. Cache reconciliation must not
    // turn a successful send into a retryable error.
    let _ = sync_imap_account(app, account_id, false).await;
    Ok(())
}

#[tauri::command]
pub async fn test_mail_account(
    window: tauri::WebviewWindow,
    input: ImapAccountInput,
) -> Result<MailConnectionReport, String> {
    crate::require_command_window(&window, &["main"])?;
    let input = input.normalized()?;
    let mailbox_count = test_imap(&input).await?;
    tauri::async_runtime::spawn_blocking(move || {
        test_smtp(&input)?;
        Ok(MailConnectionReport {
            imap_ok: true,
            smtp_ok: true,
            mailbox_count,
        })
    })
    .await
    .map_err(|_| "mail_account_test_interrupted".to_string())?
}

#[tauri::command]
pub async fn add_mail_account(
    window: tauri::WebviewWindow,
    app: AppHandle,
    input: ImapAccountInput,
) -> Result<crate::db::Account, String> {
    crate::require_command_window(&window, &["main"])?;
    let input = input.normalized()?;
    test_imap(&input).await?;
    let smtp_input = input.clone();
    tauri::async_runtime::spawn_blocking(move || test_smtp(&smtp_input))
        .await
        .map_err(|_| "mail_account_test_interrupted".to_string())??;

    let previous_password = load_stored_password(&input.email);
    let previous_tokens = crate::db::load_tokens(&input.email);
    save_stored_password(&input.email, &input.password)?;
    if let Err(error) = crate::db::delete_tokens(&input.email) {
        if let Some(previous_password) = previous_password {
            let _ = save_stored_password(&input.email, &previous_password);
        } else {
            let _ = delete_stored_password(&input.email);
        }
        return Err(error);
    }
    let settings = crate::db::ImapAccountSettings {
        account_id: input.email.clone(),
        username: input.username,
        imap_host: input.imap_host,
        imap_port: input.imap_port,
        imap_security: security_name(input.imap_security).to_string(),
        smtp_host: input.smtp_host,
        smtp_port: input.smtp_port,
        smtp_security: security_name(input.smtp_security).to_string(),
    };
    match crate::db::upsert_imap_account(&app, &settings) {
        Ok(account) => Ok(account),
        Err(error) => {
            if let Some(previous_password) = previous_password {
                let _ = save_stored_password(&settings.account_id, &previous_password);
            } else {
                let _ = delete_stored_password(&settings.account_id);
            }
            if let Some(previous_tokens) = previous_tokens {
                let _ = crate::db::save_tokens(&settings.account_id, &previous_tokens);
            }
            Err(error)
        }
    }
}

fn security_name(security: MailSecurity) -> &'static str {
    match security {
        MailSecurity::Tls => "tls",
        MailSecurity::Starttls => "starttls",
    }
}

fn security_from_name(value: &str) -> Result<MailSecurity, String> {
    match value {
        "tls" => Ok(MailSecurity::Tls),
        "starttls" => Ok(MailSecurity::Starttls),
        _ => Err("mail_account_invalid_security".to_string()),
    }
}

fn stored_account_input(app: &AppHandle, account_id: &str) -> Result<ImapAccountInput, String> {
    let settings = crate::db::get_imap_account_settings(app, account_id)?;
    let password = load_stored_password(account_id).unwrap_or_default();
    if password.is_empty() && crate::db::load_tokens(account_id).is_none() {
        return Err("mail_account_password_required".to_string());
    }
    Ok(ImapAccountInput {
        email: settings.account_id,
        username: settings.username,
        password,
        imap_host: settings.imap_host,
        imap_port: settings.imap_port,
        imap_security: security_from_name(&settings.imap_security)?,
        smtp_host: settings.smtp_host,
        smtp_port: settings.smtp_port,
        smtp_security: security_from_name(&settings.smtp_security)?,
    })
}

async fn account_oauth_token(app: &AppHandle, account_id: &str) -> Result<Option<String>, String> {
    if crate::db::load_tokens(account_id).is_none() {
        return Ok(None);
    }
    match crate::db::load_account_access_token(account_id) {
        Ok(token) => Ok(Some(token)),
        Err(_) => {
            crate::auth::refresh_session(app.clone(), account_id).await?;
            crate::db::load_account_access_token(account_id).map(Some)
        }
    }
}

/// Whether an error means the stored credential is gone rather than momentarily
/// unreachable. Only this deserves to interrupt the user for a new sign-in.
pub(crate) fn is_session_revoked(error: &str) -> bool {
    error.contains("mail_oauth_refresh_revoked") || error.contains("mail_oauth_refresh_token_missing")
}

fn header(parsed: &mailparse::ParsedMail<'_>, name: &str) -> String {
    parsed.headers.get_first_value(name).unwrap_or_default()
}

fn escape_plain_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace("\r\n", "<br>")
        .replace('\n', "<br>")
}

fn find_body(parsed: &mailparse::ParsedMail<'_>, mime: &str) -> Option<String> {
    if parsed.ctype.mimetype.eq_ignore_ascii_case(mime) {
        return parsed.get_body().ok();
    }
    parsed
        .subparts
        .iter()
        .find_map(|part| find_body(part, mime))
}

/// One part of a message, addressed by the IMAP section path that fetches it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MessagePart {
    path: Vec<u32>,
    mime_type: String,
    /// Content-Transfer-Encoding spelled as it would appear in a header, which
    /// is how it is handed back to the MIME decoder.
    encoding: String,
    charset: Option<String>,
    filename: Option<String>,
    /// Size on the wire, before decoding.
    octets: u32,
}

impl MessagePart {
    fn section(&self) -> String {
        self.path
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(".")
    }

    /// What the reader should show as the file size. Base64 carries three bytes
    /// in every four, and reporting the wire size would overstate every
    /// attachment by a third.
    fn decoded_size(&self) -> i64 {
        if self.encoding.eq_ignore_ascii_case("base64") {
            i64::from(self.octets) * 3 / 4
        } else {
            i64::from(self.octets)
        }
    }
}

/// The parts a message is made of, split by whether the sync downloads them.
#[derive(Debug, Default, PartialEq, Eq)]
struct MessageLayout {
    /// Body text the list and the reader need immediately.
    text: Vec<MessagePart>,
    /// Files recorded by name and size, left on the server until asked for.
    attachments: Vec<MessagePart>,
}

/// Splits a BODYSTRUCTURE into the parts the sync downloads and the ones it only
/// records. Attachment bytes are the bulk of a mailbox and almost none of what
/// the list and the reader need up front, so they stay on the server.
fn message_layout(structure: &imap_proto::BodyStructure<'_>) -> MessageLayout {
    let mut layout = MessageLayout::default();
    collect_parts(structure, &[], &mut layout);
    layout
}

fn collect_parts(
    structure: &imap_proto::BodyStructure<'_>,
    path: &[u32],
    layout: &mut MessageLayout,
) {
    use imap_proto::BodyStructure;
    match structure {
        BodyStructure::Multipart { bodies, .. } => {
            for (index, body) in bodies.iter().enumerate() {
                let mut child = path.to_vec();
                child.push(index as u32 + 1);
                collect_parts(body, &child, layout);
            }
        }
        BodyStructure::Text { common, other, .. } => {
            let part = describe_part(common, other, path);
            // A text part that names a file is a text file someone attached,
            // not the message body.
            if is_attachment_part(&part, common) {
                layout.attachments.push(part);
            } else {
                layout.text.push(part);
            }
        }
        BodyStructure::Message {
            common,
            other,
            body,
            ..
        } => {
            let part = describe_part(common, other, path);
            if is_attachment_part(&part, common) {
                // An .eml someone attached is a file like any other.
                layout.attachments.push(part);
                return;
            }
            // A message forwarded inline is body text the reader has always
            // shown. Its own parts are numbered underneath this one, and a
            // nested message that is not multipart puts its body at .1.
            match body.as_ref() {
                BodyStructure::Multipart { .. } => collect_parts(body, path, layout),
                nested => {
                    let mut child = path.to_vec();
                    child.push(1);
                    collect_parts(nested, &child, layout);
                }
            }
        }
        BodyStructure::Basic { common, other, .. } => {
            let part = describe_part(common, other, path);
            // Matching what the cache has always listed: a part counts as an
            // attachment when it says so or when it carries a filename.
            // Anything else is decoration this app has never shown.
            if is_attachment_part(&part, common) {
                layout.attachments.push(part);
            }
        }
    }
}

fn is_attachment_part(part: &MessagePart, common: &imap_proto::BodyContentCommon<'_>) -> bool {
    part.filename.is_some()
        || common
            .disposition
            .as_ref()
            .is_some_and(|disposition| disposition.ty.eq_ignore_ascii_case("attachment"))
}

fn describe_part(
    common: &imap_proto::BodyContentCommon<'_>,
    other: &imap_proto::BodyContentSinglePart<'_>,
    path: &[u32],
) -> MessagePart {
    use imap_proto::ContentEncoding;
    // The disposition is the authoritative place for a filename; the content
    // type's `name` is the older spelling servers still send.
    let filename = common
        .disposition
        .as_ref()
        .and_then(|disposition| decoded_param(&disposition.params, "filename"))
        .or_else(|| decoded_param(&common.ty.params, "name"));
    MessagePart {
        // A message that is not multipart still has its body at section 1.
        path: if path.is_empty() {
            vec![1]
        } else {
            path.to_vec()
        },
        mime_type: format!("{}/{}", common.ty.ty, common.ty.subtype).to_lowercase(),
        encoding: match other.transfer_encoding {
            ContentEncoding::SevenBit => "7bit".to_string(),
            ContentEncoding::EightBit => "8bit".to_string(),
            ContentEncoding::Binary => "binary".to_string(),
            ContentEncoding::Base64 => "base64".to_string(),
            ContentEncoding::QuotedPrintable => "quoted-printable".to_string(),
            ContentEncoding::Other(value) => value.to_string(),
        },
        charset: param(&common.ty.params, "charset"),
        filename: filename.filter(|value| !value.is_empty()),
        octets: other.octets,
    }
}

fn param(params: &imap_proto::BodyParams<'_>, name: &str) -> Option<String> {
    params
        .as_ref()?
        .iter()
        .find_map(|(key, value)| key.eq_ignore_ascii_case(name).then(|| (*value).to_string()))
}

/// Decodes either a traditional parameter or RFC 2231's extended/continued
/// form (`filename*`, `filename*0*`, ...). `mailparse` already implements the
/// charset, percent-decoding, and continuation rules for MIME parameters, so
/// reconstruct a harmless synthetic parameter list rather than maintaining a
/// second, subtly different decoder here.
fn decoded_param(params: &imap_proto::BodyParams<'_>, name: &str) -> Option<String> {
    let params = params.as_ref()?;
    if let Some(value) = params
        .iter()
        .find_map(|(key, value)| key.eq_ignore_ascii_case(name).then_some(*value))
    {
        return Some(decode_parameter_text(value.trim()));
    }
    let continued_prefix = format!("{name}*");
    if !params
        .iter()
        .any(|(key, _)| key.to_ascii_lowercase().starts_with(&continued_prefix))
    {
        return None;
    }
    let mut synthetic = "attachment".to_string();
    for (key, value) in params {
        synthetic.push_str("; ");
        synthetic.push_str(key);
        synthetic.push_str("=\"");
        synthetic.push_str(&value.replace('\\', "\\\\").replace('"', "\\\""));
        synthetic.push('"');
    }
    mailparse::parse_content_disposition(&synthetic)
        .params
        .get(name)
        .cloned()
        .map(|value| decode_parameter_text(value.trim()))
}

/// A filename can also arrive RFC 2047 encoded. Handing it back through a
/// header line lets the MIME decoder undo that, the same way it does for a
/// subject.
fn decode_parameter_text(value: &str) -> String {
    if !value.contains("=?") {
        return value.to_string();
    }
    mailparse::parse_header(format!("X-Name: {value}").as_bytes())
        .map(|(header, _)| header.get_value())
        .unwrap_or_else(|_| value.to_string())
}

/// Hands a downloaded part back to the MIME decoder with the headers that
/// describe it, so transfer encoding and charset are undone by the same code
/// that has always done it for whole messages.
fn decode_text_part(part: &MessagePart, raw: &[u8]) -> Option<String> {
    let charset = part
        .charset
        .as_deref()
        .map(|charset| format!("; charset=\"{}\"", charset.replace('"', "")))
        .unwrap_or_default();
    let mut synthetic = format!(
        "Content-Type: {}{}\r\nContent-Transfer-Encoding: {}\r\n\r\n",
        part.mime_type, charset, part.encoding
    )
    .into_bytes();
    synthetic.extend_from_slice(raw);
    mailparse::parse_mail(&synthetic).ok()?.get_body().ok()
}

/// A message as the sync learns it: its headers, the body text it chose to
/// download, and the attachment parts it deliberately left on the server.
struct FetchedMessage {
    uid: u32,
    unread: bool,
    starred: bool,
    header: Vec<u8>,
    plain: String,
    html: String,
    text_parts: Vec<MessagePart>,
    attachments: Vec<MessagePart>,
}

impl FetchedMessage {
    /// True when the structure promised body text that never arrived. A message
    /// with no text parts at all — a bare calendar invite, say — is complete as
    /// it is and must not be mistaken for one of these.
    fn body_missing(&self) -> bool {
        !self.text_parts.is_empty() && self.plain.is_empty() && self.html.is_empty()
    }
}

fn message_to_email(
    account_id: &str,
    label: &str,
    message: &FetchedMessage,
) -> Result<(crate::db::Email, Vec<crate::db::Attachment>), String> {
    let parsed = mailparse::parse_mail(&message.header)
        .map_err(|_| "mail_account_message_parse_failed".to_string())?;
    let uid = message.uid;
    let (unread, starred) = (message.unread, message.starred);
    let message_id = header(&parsed, "Message-ID");
    let references = header(&parsed, "References");
    let in_reply_to = header(&parsed, "In-Reply-To");
    let id = format!("imap:{label}:{uid}");
    let thread_id = references
        .split_whitespace()
        .last()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            if !in_reply_to.is_empty() {
                &in_reply_to
            } else if !message_id.is_empty() {
                &message_id
            } else {
                &id
            }
        })
        .to_string();
    let plain_body = &message.plain;
    let body_html = if message.html.is_empty() {
        escape_plain_text(plain_body)
    } else {
        message.html.clone()
    };
    let snippet = plain_body
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(240)
        .collect();
    let date = mailparse::dateparse(&header(&parsed, "Date"))
        .unwrap_or(0)
        .saturating_mul(1000);

    // No `data`: the bytes stay on the server until the reader asks, and the
    // section path is what fetches them then.
    let attachments = message
        .attachments
        .iter()
        .map(|part| crate::db::Attachment {
            id: format!("{id}:part:{}", part.section()),
            email_id: id.clone(),
            account_id: account_id.to_string(),
            filename: part
                .filename
                .clone()
                .unwrap_or_else(|| "attachment".to_string()),
            mime_type: part.mime_type.clone(),
            size: part.decoded_size(),
            attachment_id: Some(part.section()),
            data: None,
        })
        .collect();
    let email = crate::db::Email {
        id,
        thread_id,
        sender: header(&parsed, "From"),
        recipient: header(&parsed, "To"),
        cc: header(&parsed, "Cc"),
        reply_to: header(&parsed, "Reply-To"),
        message_id,
        references,
        subject: header(&parsed, "Subject"),
        snippet,
        body_html,
        date,
        unread,
        label: label.to_string(),
        gmail_label_ids: if starred {
            vec!["STARRED".to_string()]
        } else {
            Vec::new()
        },
    };
    Ok((email, attachments))
}

/// `force_reconcile` makes every mailbox take a full FLAGS pass instead of
/// trusting the UIDNEXT/EXISTS checkpoint. Read and starred state can change
/// without moving either value, so a sync woken by an IDLE notification has to
/// ask rather than assume nothing happened.
pub async fn sync_imap_account(
    app: &AppHandle,
    account_id: &str,
    force_reconcile: bool,
) -> Result<(), String> {
    let input = stored_account_input(app, account_id)?;
    let access_token = account_oauth_token(app, account_id).await?;
    if input.imap_host.eq_ignore_ascii_case("imap.gmail.com") {
        if let Some(token) = access_token.as_deref() {
            let _ = crate::mail_oauth::refresh_google_profile_picture(app, account_id, token).await;
        }
    }
    let app = app.clone();
    let account_id = account_id.to_string();
    tauri::async_runtime::spawn_blocking(move || {
        sync_imap_account_blocking(
            &app,
            &account_id,
            &input,
            access_token.as_deref(),
            force_reconcile,
        )
    })
    .await
    .map_err(|_| "mail_account_test_interrupted".to_string())?
}

/// Whether the sync/watch engine should sit still: the user paused sync
/// outright, or notifications are off and the window is out of sight, so
/// nobody is waiting on a live update. Mirrors the equivalent frontend gate on
/// the periodic path (`useMailSync.ts`'s `runBackgroundSync`), applied here so
/// it also reaches the IDLE watcher and any other Rust-side caller of the sync
/// engine that path does not cover.
fn should_skip_sync(paused: bool, notifications_disabled: bool, window_hidden: bool) -> bool {
    paused || (notifications_disabled && window_hidden)
}

/// Live query, not cached state: the main window's hidden/shown transitions
/// happen at several call sites (tray close, tray click, notification click,
/// background launch), and asking the window directly is simpler and cannot
/// drift out of sync with any of them.
fn main_window_hidden(app: &AppHandle) -> bool {
    app.get_webview_window("main")
        .and_then(|window| window.is_visible().ok())
        .map(|visible| !visible)
        .unwrap_or(false)
}

/// How long a discovered mailbox layout is trusted before the server is asked
/// for it again. It is kept in memory on purpose: a restart should re-read the
/// layout, and a folder made elsewhere should not have to wait for one.
const MAILBOX_LAYOUT_TTL: Duration = Duration::from_secs(600);

fn mailbox_layout_checks() -> &'static Mutex<HashMap<String, Instant>> {
    static CHECKS: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    CHECKS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn mailbox_layout_is_stale(account_id: &str) -> bool {
    mailbox_layout_checks()
        .lock()
        .map(|checks| {
            checks
                .get(account_id)
                .is_none_or(|checked| checked.elapsed() >= MAILBOX_LAYOUT_TTL)
        })
        .unwrap_or(true)
}

fn mark_mailbox_layout_fresh(account_id: &str) {
    if let Ok(mut checks) = mailbox_layout_checks().lock() {
        checks.insert(account_id.to_string(), Instant::now());
    }
}

/// Reads the server's mailbox list and makes the local cache match it: folders
/// that are gone take their cached messages with them, and a Gmail account's
/// label list follows the same listing, since Gmail exposes every label as a
/// mailbox of its own.
fn adopt_mailbox_layout(
    app: &AppHandle,
    account_id: &str,
    session: &mut OAuthImapSession,
    gmail_account: bool,
) -> Result<Vec<(String, String)>, String> {
    let discovered = discover_imap_mailboxes(session, gmail_account)?;
    let removed = crate::db::replace_imap_mailboxes(app, account_id, &discovered.roles)?;
    let generation = crate::db::get_account_cache_generation(app, account_id)?;
    for role in removed {
        // Only a user folder's cache is keyed by its own role, and only it can
        // be orphaned this way: a system role that disappears would take the
        // shared label of every other mailbox with it.
        if !role.starts_with("custom:") {
            continue;
        }
        let cached = crate::db::get_email_ids_for_label(app, account_id, &role)?;
        if !cached.is_empty() {
            imap_log(|| format!("layout: dropping {} cached message(s) from {role}", cached.len()));
            crate::db::delete_emails_by_ids(app, account_id, generation, &cached)?;
        }
    }
    if gmail_account {
        // Label names travel in modified UTF-7 like every other mailbox name;
        // they are stored and shown decoded, and the pairs carry the rows
        // written before that was true over to the readable id.
        let names: Vec<String> = discovered
            .gmail_labels
            .iter()
            .map(|name| crate::mutf7::decode(name))
            .collect();
        let renames: Vec<(String, String)> = discovered
            .gmail_labels
            .iter()
            .cloned()
            .zip(names.iter().cloned())
            .collect();
        for name in &names {
            crate::db::seed_gmail_label_if_missing(app, account_id, name, name)?;
        }
        crate::db::reconcile_server_labels(app, account_id, &names, &renames)?;
    }
    Ok(discovered.roles)
}

/// How often Gmail's labels are read back from the server. They cannot ride
/// along with the flags — see `reconcile_gmail_labels` — so they cost their own
/// round trips, and a few minutes of lag on a label set in another client is a
/// fair price for not paying them on every pass.
const GMAIL_LABEL_TTL: Duration = Duration::from_secs(600);
/// A ceiling on how much one reconcile may cost. Past this many labels the
/// searches stop being worth their round trips, and the local list keeps
/// whatever this app itself applied.
const GMAIL_LABEL_SEARCH_LIMIT: usize = 60;

fn gmail_label_checks() -> &'static Mutex<HashMap<String, Instant>> {
    static CHECKS: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    CHECKS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn gmail_labels_are_due(account_id: &str) -> bool {
    gmail_label_checks()
        .lock()
        .map(|checks| {
            checks
                .get(account_id)
                .is_none_or(|checked| checked.elapsed() >= GMAIL_LABEL_TTL)
        })
        .unwrap_or(true)
}

fn mark_gmail_labels_checked(account_id: &str) {
    if let Ok(mut checks) = gmail_label_checks().lock() {
        checks.insert(account_id.to_string(), Instant::now());
    }
}

/// Reads Gmail's label membership back from the server and makes the cache
/// match it.
///
/// It has to be a search. Gmail carries labels in `X-GM-LABELS`, which cannot
/// be fetched through this crate at all: every reply line is parsed by
/// `imap-proto`, which has no such attribute and fails the whole command when
/// one appears — and leaves the rest of the reply in the socket, which breaks
/// every command after it. `SEARCH X-GM-LABELS "name"` is ordinary IMAP the
/// parser understands, and Gmail answers it in any mailbox, so membership is
/// read one label at a time and reconciled for exactly the mailbox it was
/// searched in.
fn reconcile_gmail_labels(
    app: &AppHandle,
    account_id: &str,
    session: &mut OAuthImapSession,
    mailboxes: &[(String, String)],
) -> Result<(), String> {
    let labels = crate::db::get_gmail_labels_for_account(app, account_id)?;
    if labels.is_empty() {
        return Ok(());
    }
    if labels.len() > GMAIL_LABEL_SEARCH_LIMIT {
        imap_log(|| format!("labels: {} is too many to reconcile", labels.len()));
        return Ok(());
    }
    for (role, mailbox) in mailboxes {
        let Some(cache_label) = mailbox_label(role, true).map(str::to_string) else {
            continue;
        };
        // Nothing cached here means nothing a search could be reconciled
        // against, and one count is cheaper than a search per label.
        if crate::db::count_emails_for_label(app, account_id, &cache_label)? == 0 {
            continue;
        }
        if session.select(mailbox).is_err() {
            continue;
        }
        for label in &labels {
            let Ok(uids) = session.uid_search(tag_search_query(true, &label.name)) else {
                // One label that could not be searched leaves its own
                // membership alone; the rest of the pass is still worth doing.
                continue;
            };
            let member_ids: Vec<String> = uids
                .into_iter()
                .map(|uid| format!("imap:{cache_label}:{uid}"))
                .collect();
            let changed = crate::db::set_label_membership(
                app,
                account_id,
                &label.id,
                &cache_label,
                &member_ids,
            )?;
            if changed > 0 {
                imap_log(|| format!("labels: {changed} message(s) changed in {role}"));
            }
        }
    }
    Ok(())
}

fn sync_imap_account_blocking(
    app: &AppHandle,
    account_id: &str,
    input: &ImapAccountInput,
    access_token: Option<&str>,
    force_reconcile: bool,
) -> Result<(), String> {
    let controls = crate::settings::read_app_controls(app);
    if should_skip_sync(
        controls.mail_sync_paused,
        controls.notifications_disabled(),
        main_window_hidden(app),
    ) {
        return Ok(());
    }
    let gate = account_sync_gate(account_id);
    let _guard = gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut session = connect_sync_imap(input, access_token)?;
    let gmail_account = input.imap_host.eq_ignore_ascii_case("imap.gmail.com");
    let condstore = session_supports(&mut session, "CONDSTORE");
    // The layout changes rarely, so it is not worth a LIST on every sync — but
    // it does change: a folder or a label made in another client appears
    // nowhere else, and nothing but this asks the server for it again.
    let cached_mailboxes = crate::db::get_imap_mailboxes(app, account_id)?;
    let mailboxes = if cached_mailboxes.is_empty() || mailbox_layout_is_stale(account_id) {
        let discovered = adopt_mailbox_layout(app, account_id, &mut session, gmail_account)?;
        mark_mailbox_layout_fresh(account_id);
        discovered
    } else {
        cached_mailboxes
    };
    let gmail_has_all = gmail_account && mailboxes.iter().any(|(role, _)| role == "all");
    let generation = crate::db::get_account_cache_generation(app, account_id)?;
    let mut layout_stale = false;
    for (role, mailbox) in &mailboxes {
        if gmail_has_all && role == "archive" {
            continue;
        }
        let Some(label) = mailbox_label(role, gmail_account) else {
            continue;
        };
        let pass = MailboxSync {
            app,
            account_id,
            role,
            mailbox,
            label,
            generation,
            gmail_account,
            condstore,
            // IDLE only watches the inbox, so forcing a pass anywhere else would
            // cost a full FLAGS listing for a mailbox nothing reported a change
            // in. The other mailboxes stay on the timer.
            forced: force_reconcile && role == "inbox",
        };
        match pass.run(&mut session) {
            Ok(_) => {}
            Err(MailboxSyncError::Select) => {
                // The cached name no longer resolves, so the layout moved.
                layout_stale = true;
                if role == "inbox" {
                    return Err("mail_account_imap_failed".to_string());
                }
            }
            // A mailbox that failed keeps its checkpoint, so the next run
            // retries it. Only the inbox is worth failing the whole sync for.
            Err(MailboxSyncError::Failed(error)) => {
                if role == "inbox" {
                    return Err(error);
                }
            }
        }
    }
    if layout_stale {
        // Refresh the stored layout so the next sync selects the real names.
        if adopt_mailbox_layout(app, account_id, &mut session, gmail_account).is_ok() {
            mark_mailbox_layout_fresh(account_id);
        }
    }
    if gmail_account && gmail_labels_are_due(account_id) {
        // A failed reconcile is not worth failing a sync that already worked,
        // and the timer holds so the next sync tries again rather than
        // hammering the server.
        if let Err(error) = reconcile_gmail_labels(app, account_id, &mut session, &mailboxes) {
            imap_log(|| format!("labels: reconcile failed ({error})"));
        }
        mark_gmail_labels_checked(account_id);
    }
    session.logout().ok();
    Ok(())
}

/// What a SELECT reported about a mailbox. `highest_mod_seq` stays 0 on servers
/// without CONDSTORE, which is what keeps those mailboxes on the UID-range path.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct SelectedMailbox {
    uid_validity: u32,
    uid_next: u32,
    exists: u32,
    highest_mod_seq: u64,
    /// Whether this mailbox's PERMANENTFLAGS included `\*`, the server's
    /// permission to define new keywords — the primitive non-Gmail tagging
    /// rides on (see `tag_store_query`). Gmail's X-GM-LABELS is a separate
    /// extension and does not depend on this.
    supports_custom_keywords: bool,
}

/// Mailbox names travel inside a quoted string, so the two characters that can
/// end or escape one are escaped. A name carrying CR or LF is refused rather
/// than allowed to split the command line into a second command.
fn quote_mailbox(name: &str) -> Result<String, String> {
    if name.contains(['\r', '\n']) {
        return Err("mail_account_imap_failed".to_string());
    }
    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
    Ok(format!("\"{escaped}\""))
}

/// `SELECT (CONDSTORE)` enables the extension for this mailbox and makes the
/// server report HIGHESTMODSEQ. The crate's own `select` has no field for that
/// value and drops it, so the response is read here instead.
fn select_mailbox(
    session: &mut OAuthImapSession,
    mailbox: &str,
    condstore: bool,
) -> Result<SelectedMailbox, String> {
    if !condstore {
        let selected = session
            .select(mailbox)
            .map_err(|_| "mail_account_imap_failed".to_string())?;
        return Ok(SelectedMailbox {
            uid_validity: selected.uid_validity.unwrap_or(0),
            uid_next: selected.uid_next.unwrap_or(0),
            exists: selected.exists,
            highest_mod_seq: 0,
            supports_custom_keywords: selected
                .permanent_flags
                .contains(&imap::types::Flag::MayCreate),
        });
    }
    let command = format!("SELECT {} (CONDSTORE)", quote_mailbox(mailbox)?);
    let response = session
        .run_command_and_read_response(&command)
        .map_err(|_| "mail_account_imap_failed".to_string())?;
    let selected = parse_selected_mailbox(&response);
    if selected.is_err() {
        imap_log(|| format!("condstore select reply unparsed ({} bytes)", response.len()));
    }
    selected
}

/// Reads the untagged part of a SELECT reply. The tagged line is already
/// consumed and checked by the caller, so anything left here is either data we
/// want or an unsolicited response we can ignore.
fn parse_selected_mailbox(response: &[u8]) -> Result<SelectedMailbox, String> {
    use imap_proto::{MailboxDatum, Response, ResponseCode};

    let mut selected = SelectedMailbox::default();
    let mut rest = response;
    while !rest.is_empty() {
        let Ok((remaining, parsed)) = imap_proto::parse_response(rest) else {
            return Err("mail_account_imap_failed".to_string());
        };
        rest = remaining;
        match parsed {
            Response::Data {
                code: Some(code), ..
            } => match code {
                ResponseCode::UidValidity(value) => selected.uid_validity = value,
                ResponseCode::UidNext(value) => selected.uid_next = value,
                ResponseCode::HighestModSeq(value) => selected.highest_mod_seq = value,
                ResponseCode::PermanentFlags(flags) => {
                    selected.supports_custom_keywords = flags.iter().any(|flag| *flag == "\\*");
                }
                _ => {}
            },
            Response::MailboxData(MailboxDatum::Exists(value)) => selected.exists = value,
            _ => {}
        }
    }
    Ok(selected)
}

/// The system mailbox a server role is cached under, or `None` for roles the
/// cache has no place for. A `custom:` role (an unrecognized user folder,
/// see `discover_imap_mailboxes`) is cached under its own role: unlike
/// drafts, which are fetched live and deliberately never cached here, a
/// custom folder is browsed like Sent/Archive and needs the same local list.
fn mailbox_label(role: &str, gmail_account: bool) -> Option<&str> {
    match role {
        "inbox" => Some("inbox"),
        "sent" => Some("sent"),
        "trash" => Some("trash"),
        "junk" => Some("spam"),
        "archive" => Some("archive"),
        "all" if gmail_account => Some("archive"),
        _ if role.starts_with("custom:") => Some(role),
        _ => None,
    }
}

/// How long a mailbox may go without a full pass. A watched inbox learns about
/// flag changes from IDLE, so it does not need the short bound an unwatched one
/// depends on. With CONDSTORE the timer stops carrying flag freshness entirely:
/// its only remaining job is the deletion the message count could not reveal,
/// so it moves out of the way.
fn reconcile_interval_secs(account_id: &str, role: &str, condstore: bool) -> i64 {
    if condstore {
        CONDSTORE_RECONCILE_INTERVAL_SECS
    } else if role != "inbox" {
        MAILBOX_RECONCILE_INTERVAL_SECS
    } else if inbox_idle_active(account_id) {
        WATCHED_INBOX_RECONCILE_INTERVAL_SECS
    } else {
        INBOX_RECONCILE_INTERVAL_SECS
    }
}

/// One mailbox's incremental pass over an already-connected session.
struct MailboxSync<'a> {
    app: &'a AppHandle,
    account_id: &'a str,
    role: &'a str,
    mailbox: &'a str,
    label: &'a str,
    generation: i64,
    gmail_account: bool,
    /// Whether this connection's server advertised CONDSTORE. Asking a server
    /// that did not for `(CONDSTORE)` or `CHANGEDSINCE` is a protocol error, so
    /// the capability decides which commands the pass may use at all.
    condstore: bool,
    /// Reconcile even when the checkpoint looks unchanged, for a caller that
    /// knows the server reported a change it would not describe.
    forced: bool,
}

enum MailboxOutcome {
    /// The checkpoint matched, so nothing was fetched.
    Skipped,
    /// The pass ran; `changed` reports whether the local cache actually moved.
    Ran { changed: bool },
}

/// What a pass did, and whether it did all of it. `incomplete` means the server
/// owed this pass a message it never delivered, which is the one condition that
/// must hold the checkpoint back: advancing past a message the cache does not
/// have is how it would be lost until the next full listing.
#[derive(Debug, Default, PartialEq, Eq)]
struct PassReport {
    changed: bool,
    incomplete: bool,
}

impl PassReport {
    fn merge(&mut self, other: PassReport) {
        self.changed |= other.changed;
        self.incomplete |= other.incomplete;
    }
}

enum MailboxSyncError {
    /// SELECT failed, so the stored mailbox name no longer resolves.
    Select,
    /// Anything else that ended the pass.
    Failed(String),
}

/// What the server has to be asked for, decided from the checkpoint SELECT just
/// reported against the one the last pass stored.
#[derive(Debug, PartialEq, Eq)]
enum MailboxPlan {
    /// Nothing moved and nothing is due.
    Skip,
    /// List flags for the whole mailbox. Without CONDSTORE this is the only pass
    /// that can see a flag edit, and with or without it, the only one that can
    /// enumerate what the server no longer has.
    Reconcile,
    /// Ask only for what changed above the stored modification sequence. One
    /// command covers both new messages and flag edits, and it costs the changes
    /// rather than the mailbox.
    ChangedSince,
    /// No CONDSTORE, but the checkpoint says only messages were appended, so the
    /// UID range above it describes the entire change.
    NewMessages,
}

/// `always_reconcile` is for mailboxes whose contents cannot be described by a
/// UID range at all, and `forced` for a caller that knows the server reported
/// something without saying what.
fn plan_mailbox_pass(
    stored: &crate::db::ImapMailboxState,
    selected: &SelectedMailbox,
    reconcile_due: bool,
    forced: bool,
    always_reconcile: bool,
) -> MailboxPlan {
    // A server that withholds UIDVALIDITY or UIDNEXT leaves nothing to compare,
    // so those mailboxes keep taking the full path.
    let checkpoints_usable = selected.uid_validity != 0 && selected.uid_next != 0;
    let uid_validity_changed =
        stored.uid_validity != 0 && stored.uid_validity != selected.uid_validity;
    let synced_before = stored.uid_validity != 0 && !uid_validity_changed;
    let unchanged = checkpoints_usable
        && synced_before
        && stored.uid_next == selected.uid_next
        && stored.exists_count == selected.exists;
    // Both sides must have a modification sequence for the comparison to mean
    // anything: a stored 0 is a mailbox last synced without CONDSTORE, and a
    // reported 0 is a server that does not keep one. Two zeros are the absence
    // of the answer, never agreement.
    let condstore_usable =
        synced_before && stored.highest_mod_seq != 0 && selected.highest_mod_seq != 0;
    // An unchanged sequence rules out flag and tag edits too, which the UID
    // checkpoint alone never could. That is what lets a woken pass end without
    // asking — and, the other way round, what makes a moved sequence worth a
    // pass even when nothing woke this one: a message read, starred or
    // relabelled in another client moves no UID, so a mailbox whose sequence
    // has advanced is the only sign there is until the reconcile timer, which
    // on a CONDSTORE server is an hour away.
    let modseq_unchanged = condstore_usable && stored.highest_mod_seq == selected.highest_mod_seq;
    let nothing_moved = unchanged
        && if condstore_usable {
            modseq_unchanged
        } else {
            !forced
        };
    if nothing_moved && !reconcile_due {
        return MailboxPlan::Skip;
    }
    // The server keeps a modification sequence but this mailbox has none stored,
    // so there is nothing for CHANGEDSINCE to be relative to. One full pass buys
    // the baseline every later pass answers from.
    let condstore_baseline_missing = selected.highest_mod_seq != 0 && stored.highest_mod_seq == 0;
    if always_reconcile
        || !synced_before
        || uid_validity_changed
        || !checkpoints_usable
        || reconcile_due
        || condstore_baseline_missing
    {
        return MailboxPlan::Reconcile;
    }
    if condstore_usable {
        return MailboxPlan::ChangedSince;
    }
    // UIDs are handed out in ascending order, so a mailbox whose message count
    // grew by exactly as much as UIDNEXT advanced only had messages appended:
    // nothing was expunged and no older UID moved. That is the one shape a plain
    // range fetch describes completely, so it is also the only one allowed to
    // skip the full pass a forced sync would otherwise take.
    let appended = selected.uid_next.saturating_sub(stored.uid_next);
    let additions_only = appended > 0
        && selected.exists >= stored.exists_count
        && selected.exists - stored.exists_count == appended;
    if additions_only {
        MailboxPlan::NewMessages
    } else {
        MailboxPlan::Reconcile
    }
}

impl MailboxSync<'_> {
    fn run(&self, session: &mut OAuthImapSession) -> Result<MailboxOutcome, MailboxSyncError> {
        let selected = select_mailbox(session, self.mailbox, self.condstore)
            .map_err(|_| MailboxSyncError::Select)?;
        let stored = crate::db::get_imap_mailbox_state(self.app, self.account_id, self.role)
            .map_err(MailboxSyncError::Failed)?;
        let mut changed = false;
        if stored.uid_validity != 0 && stored.uid_validity != selected.uid_validity {
            // UIDs were reassigned, so every cached identifier here is stale.
            let cached = crate::db::get_email_ids_for_label(self.app, self.account_id, self.label)
                .map_err(MailboxSyncError::Failed)?;
            if !cached.is_empty() {
                crate::db::delete_emails_by_ids(
                    self.app,
                    self.account_id,
                    self.generation,
                    &cached,
                )
                .map_err(MailboxSyncError::Failed)?;
                changed = true;
            }
        }
        // Gmail's All Mail needs a server-side filter, so it cannot use a plain
        // UID range and always reconciles.
        let gmail_archive = self.role == "all" && self.gmail_account;
        let reconcile_due = unix_time_secs().saturating_sub(stored.reconciled_at)
            >= reconcile_interval_secs(
                self.account_id,
                self.role,
                stored.highest_mod_seq != 0 && selected.highest_mod_seq != 0,
            );
        let plan = plan_mailbox_pass(
            &stored,
            &selected,
            reconcile_due,
            self.forced,
            gmail_archive,
        );
        imap_log(|| {
            format!(
                "{}: plan={plan:?} condstore={} uid_next {}->{} exists {}->{} modseq {}->{}",
                self.role,
                self.condstore,
                stored.uid_next,
                selected.uid_next,
                stored.exists_count,
                selected.exists,
                stored.highest_mod_seq,
                selected.highest_mod_seq,
            )
        });
        if plan == MailboxPlan::Skip {
            return Ok(MailboxOutcome::Skipped);
        }

        let mut report = PassReport {
            changed,
            incomplete: false,
        };
        // Gmail carries its labels outside the flag list; every other server
        // carries tags as keywords, which only exist where the mailbox says it
        // can create them.
        let tags_readable = self.gmail_account || selected.supports_custom_keywords;
        if !self.gmail_account && self.role == "inbox" {
            remember_tag_capability(self.account_id, selected.supports_custom_keywords);
        }
        let mut reconciled = match plan {
            MailboxPlan::Skip => unreachable!("skipped above"),
            // Gmail's All Mail is reached through a search, and the resulting UID
            // set is the whole mailbox, so it reconciles like a full listing.
            MailboxPlan::Reconcile if gmail_archive => {
                let uids = session
                    .uid_search("X-GM-RAW \"-in:inbox -in:sent -in:drafts -in:trash -in:spam\"")
                    .map_err(|_| {
                        MailboxSyncError::Failed("mail_account_imap_failed".to_string())
                    })?;
                let uids: Vec<u32> = uids.into_iter().collect();
                let entries = self.fetch_flags_for(session, &uids)?;
                report.merge(self.absorb(session, entries, true, tags_readable)?);
                true
            }
            // One FLAGS listing reports which UIDs still exist and their current
            // state, replacing the separate UID SEARCH.
            MailboxPlan::Reconcile => {
                let entries = self.fetch_flags(session, "1:*")?;
                report.merge(self.absorb(session, entries, true, tags_readable)?);
                true
            }
            // Everything the server has touched since the stored modification
            // sequence, new messages included, in one command.
            MailboxPlan::ChangedSince => {
                let entries = self.fetch_changed_flags(session, stored.highest_mod_seq)?;
                report.merge(self.absorb(session, entries, false, tags_readable)?);
                false
            }
            // Only new messages arrived; nothing older needs revisiting.
            MailboxPlan::NewMessages => {
                let entries =
                    self.fetch_flags(session, &format!("{}:*", stored.uid_next.max(1)))?;
                report.merge(self.absorb(session, entries, false, tags_readable)?);
                false
            }
        };

        // An incremental pass never sees what the server dropped. Holding more
        // messages than the server reports is exactly that and nothing else, so
        // it is what earns the full listing.
        if !reconciled && !report.incomplete {
            let cached = crate::db::count_emails_for_label(self.app, self.account_id, self.label)
                .map_err(MailboxSyncError::Failed)?;
            if cached > selected.exists {
                let entries = self.fetch_flags(session, "1:*")?;
                report.merge(self.absorb(session, entries, true, tags_readable)?);
                reconciled = true;
            }
        }

        // The checkpoint is a promise that everything below it is cached. A pass
        // that could not keep that promise leaves it where it was, so the next
        // one asks for the same range again rather than stepping over the gap.
        if report.incomplete {
            imap_log(|| {
                format!(
                    "{}: pass incomplete, checkpoint held at uid_next {}",
                    self.role, stored.uid_next
                )
            });
            return Ok(MailboxOutcome::Ran {
                changed: report.changed,
            });
        }
        crate::db::set_imap_mailbox_state(
            self.app,
            self.account_id,
            self.role,
            crate::db::ImapMailboxState {
                uid_validity: selected.uid_validity,
                uid_next: selected.uid_next,
                exists_count: selected.exists,
                highest_mod_seq: selected.highest_mod_seq,
                reconciled_at: if reconciled {
                    unix_time_secs()
                } else {
                    stored.reconciled_at
                },
            },
        )
        .map_err(MailboxSyncError::Failed)?;
        Ok(MailboxOutcome::Ran {
            changed: report.changed,
        })
    }

    /// Stores the flags the server just reported, downloads the messages behind
    /// UIDs the cache does not have yet, and — when `full` says the listing
    /// covered the whole mailbox — drops the rows the server no longer has.
    fn absorb(
        &self,
        session: &mut OAuthImapSession,
        mut entries: Vec<MessageFlagState>,
        full: bool,
        tags_readable: bool,
    ) -> Result<PassReport, MailboxSyncError> {
        // A mailbox that cannot hold a keyword cannot report one either, so an
        // empty flag list here says nothing about the tags this app applied
        // locally. Reading it as "the server has none" is how a tag the server
        // never accepted would be erased from the only place it exists.
        if !tags_readable {
            for entry in &mut entries {
                entry.tags = None;
            }
        }
        let known_uids: Vec<u32> = entries.iter().map(|entry| entry.uid).collect();
        let known_ids = known_uids
            .iter()
            .map(|uid| format!("imap:{}:{uid}", self.label))
            .collect::<Vec<_>>();
        // A full listing has to read the whole label anyway to find what is no
        // longer there. An incremental one asks only about the UIDs it was told
        // about, which is what keeps its cost proportional to the change.
        let cached_ids = if full {
            crate::db::get_email_ids_for_label(self.app, self.account_id, self.label)
                .map_err(MailboxSyncError::Failed)?
                .into_iter()
                .collect::<HashSet<_>>()
        } else {
            crate::db::filter_cached_email_ids(self.app, self.account_id, &known_ids)
                .map_err(MailboxSyncError::Failed)?
        };
        let mut missing_uids = known_uids
            .iter()
            .copied()
            .zip(known_ids.iter())
            .filter(|(_, id)| !cached_ids.contains(*id))
            .map(|(uid, _)| uid)
            .collect::<Vec<_>>();
        // Newest first so the visible top of the list fills before the rest.
        missing_uids.sort_unstable_by(|left, right| right.cmp(left));

        let fetch = self.fetch_missing_messages(session, &missing_uids)?;
        // Flags and tags are stored after the download, so a message that has
        // just arrived is carrying the server's state from its first pass on,
        // rather than waiting for the next one to catch it up.
        let mut changed = self.store_flag_updates(&entries)? > 0;
        changed |= fetch.changed;

        if full {
            let remote_ids = known_ids.into_iter().collect::<HashSet<_>>();
            let stale_ids = cached_ids
                .difference(&remote_ids)
                .cloned()
                .collect::<Vec<_>>();
            if !stale_ids.is_empty() {
                crate::db::delete_emails_by_ids(
                    self.app,
                    self.account_id,
                    self.generation,
                    &stale_ids,
                )
                .map_err(MailboxSyncError::Failed)?;
                changed = true;
            }
        }
        Ok(PassReport {
            changed,
            incomplete: fetch.incomplete,
        })
    }

    /// Downloads and caches the messages behind `uids`. A failure here must not
    /// advance the checkpoint, or the messages that could not be stored would
    /// never be asked for again.
    fn fetch_missing_messages(
        &self,
        session: &mut OAuthImapSession,
        uids: &[u32],
    ) -> Result<PassReport, MailboxSyncError> {
        let mut report = PassReport::default();
        for uid_chunk in uids.chunks(100) {
            let mut messages = self.fetch_headers_and_layout(session, uid_chunk)?;
            // A UID that was asked for and did not come back is one the server
            // still owes, not one that does not exist.
            if messages.len() < uid_chunk.len() {
                report.incomplete = true;
            }
            self.fetch_text_parts(session, &mut messages)?;
            let mut emails = Vec::with_capacity(messages.len());
            let mut attachments = Vec::new();
            for message in &messages {
                // Storing a message whose body never arrived would cache it
                // empty and, worse, mark it known: the next pass would skip it
                // and the text would never appear. Leave it for the retry.
                if message.body_missing() {
                    report.incomplete = true;
                    imap_log(|| {
                        format!(
                            "{}: uid {} body incomplete, retrying",
                            self.role, message.uid
                        )
                    });
                    continue;
                }
                let Ok((email, mut message_attachments)) =
                    message_to_email(self.account_id, self.label, message)
                else {
                    // Headers this app cannot parse will not parse next time
                    // either, so this one does not hold the checkpoint back.
                    imap_log(|| format!("{}: uid {} could not be parsed", self.role, message.uid));
                    continue;
                };
                emails.push(email);
                attachments.append(&mut message_attachments);
            }
            if !emails.is_empty() {
                imap_log(|| format!("{}: storing {} message(s)", self.role, emails.len()));
                crate::db::upsert_sync_mail_batch(
                    self.app,
                    self.account_id,
                    self.generation,
                    None,
                    emails,
                    attachments,
                )
                .map_err(|_| MailboxSyncError::Failed("mail_account_cache_failed".to_string()))?;
                report.changed = true;
            }
        }
        Ok(report)
    }

    /// The first of the two passes a message takes: headers, flags, and the
    /// structure that says which of its parts are worth any bytes at all.
    fn fetch_headers_and_layout(
        &self,
        session: &mut OAuthImapSession,
        uids: &[u32],
    ) -> Result<Vec<FetchedMessage>, MailboxSyncError> {
        let uid_set = uids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let fetched = session
            .uid_fetch(&uid_set, "(UID FLAGS BODY.PEEK[HEADER] BODYSTRUCTURE)")
            .map_err(|_| MailboxSyncError::Failed("mail_account_imap_failed".to_string()))?;
        let mut messages = Vec::with_capacity(fetched.len());
        for message in fetched.iter() {
            let Some(uid) = message.uid else { continue };
            let Some(header) = message.header() else {
                continue;
            };
            let layout = message
                .bodystructure()
                .map(message_layout)
                .unwrap_or_default();
            messages.push(FetchedMessage {
                uid,
                unread: !message
                    .flags()
                    .iter()
                    .any(|flag| matches!(flag, imap::types::Flag::Seen)),
                starred: message
                    .flags()
                    .iter()
                    .any(|flag| matches!(flag, imap::types::Flag::Flagged)),
                header: header.to_vec(),
                plain: String::new(),
                html: String::new(),
                text_parts: layout.text,
                attachments: layout.attachments,
            });
        }
        Ok(messages)
    }

    /// The second pass: the body text, and only the body text. Messages are
    /// grouped by the exact set of sections they need, because a section number
    /// means a different part in every message and asking for one a message did
    /// not offer is how an attachment gets downloaded by accident.
    fn fetch_text_parts(
        &self,
        session: &mut OAuthImapSession,
        messages: &mut [FetchedMessage],
    ) -> Result<(), MailboxSyncError> {
        let mut groups: BTreeMap<Vec<Vec<u32>>, Vec<u32>> = BTreeMap::new();
        for message in messages.iter() {
            let sections = message
                .text_parts
                .iter()
                .map(|part| part.path.clone())
                .collect::<Vec<_>>();
            if sections.is_empty() {
                continue;
            }
            groups.entry(sections).or_default().push(message.uid);
        }

        let mut bodies: HashMap<(u32, Vec<u32>), Vec<u8>> = HashMap::new();
        for (sections, group_uids) in groups {
            let query = format!(
                "(UID {})",
                sections
                    .iter()
                    .map(|path| format!(
                        "BODY.PEEK[{}]",
                        path.iter()
                            .map(u32::to_string)
                            .collect::<Vec<_>>()
                            .join(".")
                    ))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            let uid_set = group_uids
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let fetched = session
                .uid_fetch(&uid_set, &query)
                .map_err(|_| MailboxSyncError::Failed("mail_account_imap_failed".to_string()))?;
            for message in fetched.iter() {
                let Some(uid) = message.uid else { continue };
                for path in &sections {
                    let section = imap_proto::SectionPath::Part(path.clone(), None);
                    if let Some(data) = message.section(&section) {
                        bodies.insert((uid, path.clone()), data.to_vec());
                    }
                }
            }
        }

        for message in messages.iter_mut() {
            for part in &message.text_parts {
                let Some(raw) = bodies.get(&(message.uid, part.path.clone())) else {
                    continue;
                };
                let Some(decoded) = decode_text_part(part, raw) else {
                    continue;
                };
                // The first part of each kind wins, which is how the whole
                // message was read back when it arrived in one piece.
                if part.mime_type == "text/html" {
                    if message.html.is_empty() {
                        message.html = decoded;
                    }
                } else if message.plain.is_empty() {
                    message.plain = decoded;
                }
            }
        }
        Ok(())
    }

    fn fetch_flags(
        &self,
        session: &mut OAuthImapSession,
        uid_set: &str,
    ) -> Result<Vec<MessageFlagState>, MailboxSyncError> {
        fetch_uid_flags(session, uid_set, self.gmail_account)
            .map_err(|_| MailboxSyncError::Failed("mail_account_imap_failed".to_string()))
    }

    fn fetch_changed_flags(
        &self,
        session: &mut OAuthImapSession,
        highest_mod_seq: u64,
    ) -> Result<Vec<MessageFlagState>, MailboxSyncError> {
        fetch_uid_flags_changed_since(session, highest_mod_seq, self.gmail_account)
            .map_err(|_| MailboxSyncError::Failed("mail_account_imap_failed".to_string()))
    }

    fn fetch_flags_for(
        &self,
        session: &mut OAuthImapSession,
        uids: &[u32],
    ) -> Result<Vec<MessageFlagState>, MailboxSyncError> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        let uid_set = uids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        self.fetch_flags(session, &uid_set)
    }

    /// Applies read and starred state the server reports onto already-cached
    /// messages, and reports how many of them moved. Without this, a message
    /// read on another device stays unread here.
    fn store_flag_updates(&self, entries: &[MessageFlagState]) -> Result<usize, MailboxSyncError> {
        let known_tags = if entries.iter().any(|entry| entry.tags.is_some()) {
            self.tag_names_by_wire_form()?
        } else {
            HashMap::new()
        };
        let updates = entries
            .iter()
            .map(|entry| crate::db::ImapMessageState {
                email_id: format!("imap:{}:{}", self.label, entry.uid),
                unread: entry.unread,
                starred: entry.starred,
                tags: entry.tags.as_ref().map(|tags| {
                    tags.iter()
                        .map(|tag| known_tags.get(tag).cloned().unwrap_or_else(|| tag.clone()))
                        .collect()
                }),
            })
            .collect::<Vec<_>>();
        crate::db::apply_imap_flag_state(self.app, self.account_id, &updates)
            .map_err(MailboxSyncError::Failed)
    }

    /// What each known tag looks like once it has been through this account's
    /// server, keyed back to the name the user reads. An IMAP keyword has no
    /// room for a space, so "Q1 notes" is stored as `Q1_notes` and comes back
    /// as that: without this the tag would be read as a new one and the name
    /// the user gave it would be replaced by the wire form.
    fn tag_names_by_wire_form(&self) -> Result<HashMap<String, String>, MailboxSyncError> {
        let labels = crate::db::get_gmail_labels_for_account(self.app, self.account_id)
            .map_err(MailboxSyncError::Failed)?;
        let mut names = HashMap::new();
        for label in labels {
            let wire = if self.gmail_account {
                gmail_label_wire_name(&label.name)
            } else {
                sanitize_tag_keyword(&label.name)
            };
            names.insert(wire, label.name.clone());
            names.insert(label.name.clone(), label.name);
        }
        Ok(names)
    }
}

/// What one message's flags say. `tags` is the user-visible part: the keywords
/// a server carries for it, or the labels Gmail carries, already decoded to
/// text. `None` means this pass could not read them at all, and the cached tags
/// must be left exactly as they are rather than being taken for "no tags".
#[derive(Debug, Clone, PartialEq, Eq)]
struct MessageFlagState {
    uid: u32,
    unread: bool,
    starred: bool,
    tags: Option<Vec<String>>,
}

/// Whether a flag is a tag the user gave a message. A name starting with
/// `\` is a system flag — `\Seen`, and on Gmail the label-flags `\Inbox` and
/// `\Important` — and one starting with `$` is a state other clients set for
/// their own bookkeeping (`$Forwarded`, `$MDNSent`, Thunderbird's `$label1`).
/// Neither is something this app should show as a tag.
fn tag_name_from_flag(flag: &str) -> Option<String> {
    let name = crate::mutf7::decode(flag);
    let is_tag = !name.is_empty()
        && !name.starts_with('\\')
        && !name.starts_with('$');
    is_tag.then_some(name)
}

fn tag_names_from_flags<'a>(flags: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut tags: Vec<String> = flags.filter_map(tag_name_from_flag).collect();
    tags.sort_unstable();
    tags.dedup();
    tags
}

/// Reads UID and flag state for a UID range without downloading message bodies.
fn fetch_uid_flags(
    session: &mut OAuthImapSession,
    uid_set: &str,
    gmail_account: bool,
) -> Result<Vec<MessageFlagState>, String> {
    fetch_uid_flags_with_query(session, uid_set, "(UID FLAGS)", gmail_account)
}

/// CHANGEDSINCE is a FETCH modifier, not part of the UID sequence set. Keeping
/// it in the query position is required by RFC 4551 and accepted by both Gmail
/// and Outlook. Putting it in `uid_set` produces an invalid command precisely
/// on the second and later CONDSTORE passes.
fn fetch_uid_flags_changed_since(
    session: &mut OAuthImapSession,
    highest_mod_seq: u64,
    gmail_account: bool,
) -> Result<Vec<MessageFlagState>, String> {
    let query = changed_since_fetch_query(highest_mod_seq);
    fetch_uid_flags_with_query(session, "1:*", &query, gmail_account)
}

fn changed_since_fetch_query(highest_mod_seq: u64) -> String {
    format!("(UID FLAGS) (CHANGEDSINCE {highest_mod_seq})")
}

fn fetch_uid_flags_with_query(
    session: &mut OAuthImapSession,
    uid_set: &str,
    query: &str,
    gmail_account: bool,
) -> Result<Vec<MessageFlagState>, String> {
    let fetched = session
        .uid_fetch(uid_set, query)
        .map_err(|_| "mail_account_imap_failed".to_string())?;
    let mut entries = Vec::with_capacity(fetched.len());
    for message in fetched.iter() {
        let Some(uid) = message.uid else { continue };
        let unread = !message
            .flags()
            .iter()
            .any(|flag| matches!(flag, imap::types::Flag::Seen));
        let starred = message
            .flags()
            .iter()
            .any(|flag| matches!(flag, imap::types::Flag::Flagged));
        let keywords = message.flags().iter().filter_map(|flag| match flag {
            imap::types::Flag::Custom(name) => Some(name.as_ref()),
            _ => None,
        });
        entries.push(MessageFlagState {
            uid,
            unread,
            starred,
            // Gmail's labels are not keywords and never appear in FLAGS, so
            // this pass reports nothing about them rather than reading an empty
            // keyword list as "the user cleared every label". They are read
            // separately, by search (see `reconcile_gmail_labels`).
            tags: (!gmail_account).then(|| tag_names_from_flags(keywords)),
        });
    }
    Ok(entries)
}

fn imap_remote_ref(message_id: &str) -> Option<(&str, u32)> {
    let value = message_id.strip_prefix("imap:")?;
    let (label, uid) = value.rsplit_once(':')?;
    Some((label, uid.parse().ok()?))
}

/// The server role a cached message's label points at, named the way this
/// account's own mailbox map names it. Two labels do not match their role
/// directly: `spam` is this cache's word for the `junk` role, and Gmail's All
/// Mail is cached under `archive` because that is the tab it feeds (see
/// `mailbox_label`) while the server only ever knows it as `all`. Without the
/// second mapping every message read from a Gmail archive resolves to a role
/// the account does not have, and each flag, tag, and move on it is dropped.
fn mailbox_role_for_label(label: &str, mappings: &BTreeMap<String, String>) -> Option<String> {
    let direct = if label == "spam" { "junk" } else { label };
    if mappings.contains_key(direct) {
        return Some(direct.to_string());
    }
    (direct == "archive" && mappings.contains_key("all")).then(|| "all".to_string())
}

/// Groups cached message ids into the mailboxes they live in, dropping the ones
/// this account has no mailbox for. Every command that reaches a message by UID
/// needs exactly this, so they all resolve the same way.
fn uids_by_source_role(
    message_ids: &[String],
    mappings: &BTreeMap<String, String>,
) -> BTreeMap<String, Vec<u32>> {
    let mut by_source = BTreeMap::<String, Vec<u32>>::new();
    for message_id in message_ids {
        let Some((label, uid)) = imap_remote_ref(message_id) else {
            continue;
        };
        let Some(role) = mailbox_role_for_label(label, mappings) else {
            continue;
        };
        by_source.entry(role).or_default().push(uid);
    }
    by_source
}

/// Undoing an archive on Gmail is putting the `\Inbox` label back, not
/// moving the message. A real MOVE out of All Mail would expunge it there, and
/// Gmail reads an expunge from All Mail as "delete this conversation" —
/// restoring a message must never be able to end in the trash.
fn gmail_unarchive_store_query() -> &'static str {
    "+X-GM-LABELS.SILENT (\\Inbox)"
}

fn gmail_archive_store_query() -> &'static str {
    "-X-GM-LABELS.SILENT (\\Inbox)"
}

/// Quotes a label value for use inside an `X-GM-LABELS` STORE/SEARCH term.
/// Gmail label names can contain spaces and other atom-breaking characters,
/// unlike IMAP keywords, so this rides in a quoted string rather than an atom.
fn quote_tag_value(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// IMAP keyword atoms cannot contain SP, CTL, or any of `(){}%*"\]`. A tag
/// name outside that set is folded into it here for the wire command only;
/// the display name in the local `gmail_labels` row keeps the original text.
fn sanitize_tag_keyword(name: &str) -> String {
    name.chars()
        .map(|ch| {
            if ch.is_ascii_control()
                || matches!(ch, ' ' | '(' | ')' | '{' | '}' | '%' | '*' | '"' | '\\' | ']')
            {
                '_'
            } else {
                ch
            }
        })
        .collect()
}

/// The STORE term that applies or removes a tag. Gmail carries labels through
/// its own `X-GM-LABELS` extension; every other server uses a plain IMAP
/// keyword, which only works when the mailbox's PERMANENTFLAGS include `\*`.
fn tag_store_query(gmail_account: bool, tag_name: &str, applied: bool) -> String {
    let sign = if applied { "+" } else { "-" };
    if gmail_account {
        format!(
            "{sign}X-GM-LABELS.SILENT ({})",
            quote_tag_value(&gmail_label_wire_name(tag_name))
        )
    } else {
        format!("{sign}FLAGS.SILENT ({})", sanitize_tag_keyword(tag_name))
    }
}

/// Runs a STORE and forgives the one failure that is not one: a reply this
/// crate cannot read. Every line comes back through `imap-proto`, which has no
/// `X-GM-LABELS` attribute, so the echo Gmail sends after a label change fails
/// the command even though the change was made — and leaves the rest of the
/// reply in the socket, which is why the caller must stop using the session.
/// The stores are asked for silently precisely so this stays unreachable.
fn store_on_selected(
    session: &mut OAuthImapSession,
    uid_set: &str,
    query: &str,
) -> Result<StoreOutcome, String> {
    match session.uid_store(uid_set, query) {
        Ok(_) => Ok(StoreOutcome::Continue),
        Err(imap::error::Error::Parse(_)) => {
            imap_log(|| "store: reply could not be read, ending the session".to_string());
            Ok(StoreOutcome::SessionSpent)
        }
        Err(_) => Err("mail_account_imap_failed".to_string()),
    }
}

/// Whether the session may still be used after a store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoreOutcome {
    Continue,
    SessionSpent,
}

/// The SEARCH term that finds every message currently carrying a tag.
fn tag_search_query(gmail_account: bool, tag_name: &str) -> String {
    if gmail_account {
        format!(
            "X-GM-LABELS {}",
            quote_tag_value(&gmail_label_wire_name(tag_name))
        )
    } else {
        format!("KEYWORD {}", sanitize_tag_keyword(tag_name))
    }
}

/// Gmail label names travel in the same modified UTF-7 as mailbox names, so a
/// label with a Turkish character is a different label on the wire than the one
/// the user typed. Labels are stored and shown as text; this is the one place
/// they turn back into what the server understands.
fn gmail_label_wire_name(tag_name: &str) -> String {
    crate::mutf7::encode(tag_name)
}

fn imap_draft_uid(draft_id: &str) -> Option<u32> {
    draft_id.strip_prefix("imap-draft:")?.parse().ok()
}

fn draft_mailbox(app: &AppHandle, account_id: &str) -> Result<String, String> {
    crate::db::get_imap_mailboxes(app, account_id)?
        .into_iter()
        .find_map(|(role, mailbox)| (role == "drafts").then_some(mailbox))
        .ok_or_else(|| "mail_account_label_not_supported".to_string())
}

fn draft_attachments(parsed: &mailparse::ParsedMail<'_>) -> Vec<crate::compose::AttachmentPayload> {
    parsed
        .parts()
        .skip(1)
        .filter_map(|part| {
            if !part.subparts.is_empty() {
                return None;
            }
            let disposition = part.get_content_disposition();
            let filename = disposition
                .params
                .get("filename")
                .or_else(|| part.ctype.params.get("name"))
                .cloned()
                .unwrap_or_default();
            if disposition.disposition != mailparse::DispositionType::Attachment
                && filename.is_empty()
            {
                return None;
            }
            let bytes = part.get_body_raw().ok()?;
            Some(crate::compose::AttachmentPayload {
                filename: if filename.is_empty() {
                    "attachment".to_string()
                } else {
                    filename
                },
                mime_type: part.ctype.mimetype.clone(),
                data: base64::engine::general_purpose::STANDARD.encode(bytes),
            })
        })
        .collect()
}

fn draft_content_from_raw(uid: u32, raw: &[u8]) -> Result<crate::compose::DraftContent, String> {
    let parsed =
        mailparse::parse_mail(raw).map_err(|_| "mail_account_message_parse_failed".to_string())?;
    let message_id = header(&parsed, "Message-ID");
    let references = header(&parsed, "References");
    let in_reply_to = header(&parsed, "In-Reply-To");
    let thread_id = references
        .split_whitespace()
        .last()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            if !in_reply_to.is_empty() {
                &in_reply_to
            } else {
                &message_id
            }
        })
        .to_string();
    let body = find_body(&parsed, "text/html")
        .or_else(|| find_body(&parsed, "text/plain"))
        .unwrap_or_default();
    Ok(crate::compose::DraftContent {
        id: format!("imap-draft:{uid}"),
        message_id: format!("imap:drafts:{uid}"),
        rfc_message_id: message_id,
        thread_id,
        in_reply_to,
        references,
        to: header(&parsed, "To"),
        cc: header(&parsed, "Cc"),
        bcc: header(&parsed, "Bcc"),
        subject: header(&parsed, "Subject"),
        body,
        updated_at: mailparse::dateparse(&header(&parsed, "Date"))
            .unwrap_or(0)
            .saturating_mul(1000),
        attachments: draft_attachments(&parsed),
    })
}

fn strip_bcc_header(raw: &str) -> String {
    let Some((headers, body)) = raw.split_once("\r\n\r\n") else {
        return raw.to_string();
    };
    let mut kept = Vec::new();
    let mut skipping_bcc = false;
    for line in headers.split("\r\n") {
        if line.starts_with(' ') || line.starts_with('\t') {
            if !skipping_bcc {
                kept.push(line);
            }
            continue;
        }
        skipping_bcc = line
            .split_once(':')
            .map(|(name, _)| name.eq_ignore_ascii_case("bcc"))
            .unwrap_or(false);
        if !skipping_bcc {
            kept.push(line);
        }
    }
    format!("{}\r\n\r\n{}", kept.join("\r\n"), body)
}

pub async fn list_imap_drafts(
    app: &AppHandle,
    account_id: &str,
) -> Result<crate::compose::DraftPage, String> {
    let input = stored_account_input(app, account_id)?;
    let access_token = account_oauth_token(app, account_id).await?;
    let mailbox = draft_mailbox(app, account_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut session = connect_sync_imap(&input, access_token.as_deref())?;
        session
            .select(&mailbox)
            .map_err(|_| "mail_account_imap_failed".to_string())?;
        let mut uids = session
            .uid_search("ALL")
            .map_err(|_| "mail_account_imap_failed".to_string())?
            .into_iter()
            .collect::<Vec<_>>();
        uids.sort_unstable();
        uids.reverse();
        uids.truncate(20);
        let mut drafts = Vec::new();
        if !uids.is_empty() {
            let uid_set = uids
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let fetched = session
                .uid_fetch(&uid_set, "(UID INTERNALDATE BODY.PEEK[])")
                .map_err(|_| "mail_account_imap_failed".to_string())?;
            for message in fetched.iter() {
                let (Some(uid), Some(raw)) = (message.uid, message.body()) else {
                    continue;
                };
                let mut content = draft_content_from_raw(uid, raw)?;
                if let Some(internal_date) = message.internal_date() {
                    content.updated_at = internal_date.timestamp_millis();
                }
                let snippet = mailparse::parse_mail(raw)
                    .ok()
                    .and_then(|parsed| find_body(&parsed, "text/plain"))
                    .unwrap_or_default()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(240)
                    .collect();
                drafts.push(crate::compose::DraftSummary {
                    id: content.id,
                    message_id: content.message_id,
                    rfc_message_id: content.rfc_message_id,
                    thread_id: content.thread_id,
                    in_reply_to: content.in_reply_to,
                    references: content.references,
                    to: content.to,
                    cc: content.cc,
                    bcc: content.bcc,
                    subject: content.subject,
                    snippet,
                    updated_at: content.updated_at,
                });
            }
        }
        session.logout().ok();
        drafts.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(crate::compose::DraftPage {
            drafts,
            next_page_token: None,
        })
    })
    .await
    .map_err(|_| "mail_account_test_interrupted".to_string())?
}

pub async fn get_imap_draft(
    app: &AppHandle,
    account_id: &str,
    draft_id: &str,
) -> Result<crate::compose::DraftContent, String> {
    let uid =
        imap_draft_uid(draft_id).ok_or_else(|| "mail_account_message_not_found".to_string())?;
    let input = stored_account_input(app, account_id)?;
    let access_token = account_oauth_token(app, account_id).await?;
    let mailbox = draft_mailbox(app, account_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut session = connect_sync_imap(&input, access_token.as_deref())?;
        session
            .select(&mailbox)
            .map_err(|_| "mail_account_imap_failed".to_string())?;
        let fetched = session
            .uid_fetch(uid.to_string(), "(UID INTERNALDATE BODY.PEEK[])")
            .map_err(|_| "mail_account_imap_failed".to_string())?;
        let message = fetched
            .iter()
            .next()
            .ok_or_else(|| "mail_account_message_not_found".to_string())?;
        let raw = message
            .body()
            .ok_or_else(|| "mail_account_message_not_found".to_string())?;
        let mut content = draft_content_from_raw(uid, raw)?;
        if let Some(internal_date) = message.internal_date() {
            content.updated_at = internal_date.timestamp_millis();
        }
        session.logout().ok();
        Ok(content)
    })
    .await
    .map_err(|_| "mail_account_test_interrupted".to_string())?
}

fn delete_selected_uid(session: &mut OAuthImapSession, uid: u32) -> Result<(), String> {
    let capabilities = session
        .capabilities()
        .map_err(|_| "mail_account_imap_failed".to_string())?;
    if !capabilities.has_str("UIDPLUS") {
        return Err("mail_account_label_not_supported".to_string());
    }
    let uid = uid.to_string();
    session
        .uid_store(&uid, "+FLAGS.SILENT (\\Deleted)")
        .map_err(|_| "mail_account_imap_failed".to_string())?;
    session
        .uid_expunge(&uid)
        .map(|_| ())
        .map_err(|_| "mail_account_imap_failed".to_string())
}

pub async fn save_imap_draft(
    app: &AppHandle,
    account_id: &str,
    previous_draft_id: Option<&str>,
    raw_email: String,
    verification_message_id: String,
) -> Result<crate::compose::SavedDraft, String> {
    let previous_uid = previous_draft_id.and_then(imap_draft_uid);
    let input = stored_account_input(app, account_id)?;
    let access_token = account_oauth_token(app, account_id).await?;
    let mailbox = draft_mailbox(app, account_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut session = connect_sync_imap(&input, access_token.as_deref())?;
        session
            .select(&mailbox)
            .map_err(|_| "mail_account_imap_failed".to_string())?;
        session
            .append_with_flags(&mailbox, raw_email.as_bytes(), &[imap::types::Flag::Draft])
            .map_err(|_| "mail_account_draft_save_failed".to_string())?;
        let query = format!("HEADER Message-ID {verification_message_id}");
        let new_uid = session
            .uid_search(&query)
            .map_err(|_| "mail_account_imap_failed".to_string())?
            .into_iter()
            .max()
            .ok_or_else(|| "mail_account_draft_save_failed".to_string())?;
        if let Some(previous_uid) = previous_uid.filter(|uid| *uid != new_uid) {
            delete_selected_uid(&mut session, previous_uid)?;
        }
        session.logout().ok();
        Ok(crate::compose::SavedDraft {
            id: format!("imap-draft:{new_uid}"),
            message_id: format!("imap:drafts:{new_uid}"),
            verification_message_id,
            updated_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64,
        })
    })
    .await
    .map_err(|_| "mail_account_test_interrupted".to_string())?
}

pub async fn delete_imap_draft(
    app: &AppHandle,
    account_id: &str,
    draft_id: &str,
) -> Result<(), String> {
    let uid =
        imap_draft_uid(draft_id).ok_or_else(|| "mail_account_message_not_found".to_string())?;
    let input = stored_account_input(app, account_id)?;
    let access_token = account_oauth_token(app, account_id).await?;
    let mailbox = draft_mailbox(app, account_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut session = connect_sync_imap(&input, access_token.as_deref())?;
        session
            .select(&mailbox)
            .map_err(|_| "mail_account_imap_failed".to_string())?;
        delete_selected_uid(&mut session, uid)?;
        session.logout().ok();
        Ok(())
    })
    .await
    .map_err(|_| "mail_account_test_interrupted".to_string())?
}

pub async fn send_imap_draft(
    app: &AppHandle,
    account_id: &str,
    draft_id: &str,
) -> Result<(), String> {
    let uid =
        imap_draft_uid(draft_id).ok_or_else(|| "mail_account_message_not_found".to_string())?;
    let input = stored_account_input(app, account_id)?;
    let access_token = account_oauth_token(app, account_id).await?;
    let mailbox = draft_mailbox(app, account_id)?;
    let (raw_email, to, cc, bcc) = tauri::async_runtime::spawn_blocking(move || {
        let mut session = connect_sync_imap(&input, access_token.as_deref())?;
        session
            .select(&mailbox)
            .map_err(|_| "mail_account_imap_failed".to_string())?;
        let fetched = session
            .uid_fetch(uid.to_string(), "(UID BODY.PEEK[])")
            .map_err(|_| "mail_account_imap_failed".to_string())?;
        let raw = fetched
            .iter()
            .next()
            .and_then(|message| message.body())
            .ok_or_else(|| "mail_account_message_not_found".to_string())?
            .to_vec();
        let parsed = mailparse::parse_mail(&raw)
            .map_err(|_| "mail_account_message_parse_failed".to_string())?;
        let recipients = (
            header(&parsed, "To"),
            header(&parsed, "Cc"),
            header(&parsed, "Bcc"),
        );
        session.logout().ok();
        let raw =
            String::from_utf8(raw).map_err(|_| "mail_account_message_parse_failed".to_string())?;
        Ok::<_, String>((raw, recipients.0, recipients.1, recipients.2))
    })
    .await
    .map_err(|_| "mail_account_test_interrupted".to_string())??;

    let outbound_raw = strip_bcc_header(&raw_email);
    send_smtp_raw(app, account_id, &to, &cc, &bcc, outbound_raw).await?;
    // SMTP success is authoritative; a later draft-cleanup failure must never
    // invite the UI to resend the already delivered message.
    let _ = delete_imap_draft(app, account_id, draft_id).await;
    Ok(())
}

pub async fn move_imap_thread(
    app: &AppHandle,
    account_id: &str,
    thread_id: &str,
    target_role: &str,
) -> Result<(), String> {
    let message_ids = crate::db::get_thread_email_ids(app, account_id, thread_id)?;
    let mappings = crate::db::get_imap_mailboxes(app, account_id)?
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    let target_mailbox = if target_role == "archive" {
        mappings.get("archive").or_else(|| mappings.get("all"))
    } else {
        mappings.get(target_role)
    }
    .cloned()
    .ok_or_else(|| "mail_account_label_not_supported".to_string())?;
    let mut by_source = uids_by_source_role(&message_ids, &mappings);
    // A message already sitting in the target mailbox has nothing to move.
    by_source.remove(target_role);
    if target_role == "archive" {
        by_source.remove("all");
    }
    if by_source.is_empty() {
        return Err("mail_account_message_not_found".to_string());
    }
    let input = stored_account_input(app, account_id)?;
    let gmail_account = input.imap_host.eq_ignore_ascii_case("imap.gmail.com");
    let target_is_archive = target_role == "archive";
    let target_is_inbox = target_role == "inbox";
    let access_token = account_oauth_token(app, account_id).await?;
    let mappings_for_move = mappings.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut session = connect_sync_imap(&input, access_token.as_deref())?;
        let (supports_move, supports_uidplus) = {
            let capabilities = session
                .capabilities()
                .map_err(|_| "mail_account_imap_failed".to_string())?;
            (
                capabilities.has_str("MOVE"),
                capabilities.has_str("UIDPLUS"),
            )
        };
        for (source_role, mut uids) in by_source {
            if gmail_account && target_is_archive && source_role != "inbox" {
                continue;
            }
            let Some(source_mailbox) = mappings_for_move.get(&source_role) else {
                continue;
            };
            session
                .select(source_mailbox)
                .map_err(|_| "mail_account_imap_failed".to_string())?;
            uids.sort_unstable();
            uids.dedup();
            let uid_set = uids
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            if gmail_account && target_is_archive {
                if store_on_selected(&mut session, &uid_set, gmail_archive_store_query())?
                    == StoreOutcome::SessionSpent
                {
                    return Ok(());
                }
                continue;
            }
            // Restoring an archived Gmail message is a label change, never a
            // move out of All Mail.
            if gmail_account && target_is_inbox && source_role == "all" {
                if store_on_selected(&mut session, &uid_set, gmail_unarchive_store_query())?
                    == StoreOutcome::SessionSpent
                {
                    return Ok(());
                }
                continue;
            }
            if supports_move {
                session
                    .uid_mv(&uid_set, &target_mailbox)
                    .map_err(|_| "mail_account_imap_failed".to_string())?;
            } else if supports_uidplus {
                session
                    .uid_copy(&uid_set, &target_mailbox)
                    .map_err(|_| "mail_account_imap_failed".to_string())?;
                session
                    .uid_store(&uid_set, "+FLAGS.SILENT (\\Deleted)")
                    .map_err(|_| "mail_account_imap_failed".to_string())?;
                session
                    .uid_expunge(&uid_set)
                    .map_err(|_| "mail_account_imap_failed".to_string())?;
            } else {
                return Err("mail_account_label_not_supported".to_string());
            }
        }
        session.logout().ok();
        Ok(())
    })
    .await
    .map_err(|_| "mail_account_test_interrupted".to_string())??;

    let generation = crate::db::get_account_cache_generation(app, account_id)?;
    crate::db::delete_emails_by_ids(app, account_id, generation, &message_ids)?;
    let _ = sync_imap_account(app, account_id, false).await;
    Ok(())
}

pub async fn set_imap_message_flag(
    app: &AppHandle,
    account_id: &str,
    target_id: &str,
    thread_target: bool,
    flag: imap::types::Flag<'static>,
    applied: bool,
) -> Result<(), String> {
    let message_ids = if thread_target {
        crate::db::get_thread_email_ids(app, account_id, target_id)?
    } else {
        vec![target_id.to_string()]
    };
    let mappings = crate::db::get_imap_mailboxes(app, account_id)?
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    let by_source = uids_by_source_role(&message_ids, &mappings);
    if by_source.is_empty() {
        return Err("mail_account_message_not_found".to_string());
    }
    let input = stored_account_input(app, account_id)?;
    let access_token = account_oauth_token(app, account_id).await?;
    let flag_name = flag.to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let mut session = connect_sync_imap(&input, access_token.as_deref())?;
        let query = format!(
            "{}FLAGS.SILENT ({})",
            if applied { "+" } else { "-" },
            flag_name
        );
        for (source_role, mut uids) in by_source {
            let Some(mailbox) = mappings.get(&source_role) else {
                continue;
            };
            session
                .select(mailbox)
                .map_err(|_| "mail_account_imap_failed".to_string())?;
            uids.sort_unstable();
            uids.dedup();
            let uid_set = uids
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            session
                .uid_store(&uid_set, &query)
                .map_err(|_| "mail_account_imap_failed".to_string())?;
        }
        session.logout().ok();
        Ok(())
    })
    .await
    .map_err(|_| "mail_account_test_interrupted".to_string())?
}

/// Applies or removes an arbitrary tag (a Gmail label, or an IMAP keyword on
/// any other server) on a message or its whole thread. Generalizes the
/// `X-GM-LABELS` pattern `gmail_archive_store_query` already uses for the
/// Inbox pseudo-label to any tag name, and reuses the exact message-to-UID
/// resolution `set_imap_message_flag` uses.
pub async fn set_imap_message_tag(
    app: &AppHandle,
    account_id: &str,
    target_id: &str,
    thread_target: bool,
    tag_name: &str,
    applied: bool,
) -> Result<(), String> {
    let message_ids = if thread_target {
        crate::db::get_thread_email_ids(app, account_id, target_id)?
    } else {
        vec![target_id.to_string()]
    };
    let mappings = crate::db::get_imap_mailboxes(app, account_id)?
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    let by_source = uids_by_source_role(&message_ids, &mappings);
    if by_source.is_empty() {
        return Err("mail_account_message_not_found".to_string());
    }
    let input = stored_account_input(app, account_id)?;
    let access_token = account_oauth_token(app, account_id).await?;
    let gmail_account = input.imap_host.eq_ignore_ascii_case("imap.gmail.com");
    let query = tag_store_query(gmail_account, tag_name, applied);
    tauri::async_runtime::spawn_blocking(move || {
        let mut session = connect_sync_imap(&input, access_token.as_deref())?;
        for (source_role, mut uids) in by_source {
            let Some(mailbox) = mappings.get(&source_role) else {
                continue;
            };
            session
                .select(mailbox)
                .map_err(|_| "mail_account_imap_failed".to_string())?;
            uids.sort_unstable();
            uids.dedup();
            let uid_set = uids
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            if store_on_selected(&mut session, &uid_set, &query)? == StoreOutcome::SessionSpent {
                return Ok(());
            }
        }
        session.logout().ok();
        Ok(())
    })
    .await
    .map_err(|_| "mail_account_test_interrupted".to_string())?
}

/// Every UID carrying `tag_name`, grouped by the DB role of the mailbox it
/// was found in. Gmail's label model makes a tag visible from any mailbox
/// (each is a view over the same message identity), so searching All Mail
/// alone is enough; other servers keep a keyword local to whichever mailbox
/// actually carries it, so every synced mailbox is searched.
fn find_tagged_messages(
    session: &mut OAuthImapSession,
    mailboxes: &[(String, String)],
    gmail_account: bool,
    tag_name: &str,
) -> Result<Vec<(String, String, Vec<u32>)>, String> {
    let scope: Vec<&(String, String)> = if gmail_account {
        mailboxes.iter().filter(|(role, _)| role == "all").collect()
    } else {
        mailboxes.iter().collect()
    };
    let query = tag_search_query(gmail_account, tag_name);
    let mut found = Vec::new();
    for (role, mailbox) in scope {
        session
            .select(mailbox)
            .map_err(|_| "mail_account_imap_failed".to_string())?;
        let uids = session
            .uid_search(&query)
            .map_err(|_| "mail_account_imap_failed".to_string())?;
        if !uids.is_empty() {
            found.push((role.clone(), mailbox.clone(), uids.into_iter().collect()));
        }
    }
    Ok(found)
}

/// Whether this account can carry arbitrary tags at all: Gmail always can
/// (its own `X-GM-LABELS` extension, independent of PERMANENTFLAGS); any
/// other server only if its Inbox reports `\*` in PERMANENTFLAGS. Checking
/// only Inbox is a simplification — real servers apply keyword support
/// uniformly per account, not per folder.
async fn get_tag_capability(app: &AppHandle, account_id: &str) -> Result<bool, String> {
    let input = stored_account_input(app, account_id)?;
    if input.imap_host.eq_ignore_ascii_case("imap.gmail.com") {
        return Ok(true);
    }
    if let Some(supported) = cached_tag_capability(account_id) {
        return Ok(supported);
    }
    let access_token = account_oauth_token(app, account_id).await?;
    let inbox = crate::db::get_imap_mailboxes(app, account_id)?
        .into_iter()
        .find_map(|(role, mailbox)| (role == "inbox").then_some(mailbox))
        .unwrap_or_else(|| "INBOX".to_string());
    let supported = tauri::async_runtime::spawn_blocking(move || {
        let mut session = connect_sync_imap(&input, access_token.as_deref())?;
        let condstore = session_supports(&mut session, "CONDSTORE");
        let selected = select_mailbox(&mut session, &inbox, condstore)?;
        session.logout().ok();
        Ok::<bool, String>(selected.supports_custom_keywords)
    })
    .await
    .map_err(|_| "mail_account_test_interrupted".to_string())??;
    remember_tag_capability(account_id, supported);
    Ok(supported)
}

/// How long the answer to "can this server hold a keyword" is reused. Without
/// it every single tag click paid for its own connection and SELECT before the
/// tag itself could be sent.
const TAG_CAPABILITY_TTL: Duration = Duration::from_secs(1800);

fn tag_capabilities() -> &'static Mutex<HashMap<String, (bool, Instant)>> {
    static CAPABILITIES: OnceLock<Mutex<HashMap<String, (bool, Instant)>>> = OnceLock::new();
    CAPABILITIES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_tag_capability(account_id: &str) -> Option<bool> {
    let capabilities = tag_capabilities().lock().ok()?;
    let (supported, checked) = capabilities.get(account_id)?;
    (checked.elapsed() < TAG_CAPABILITY_TTL).then_some(*supported)
}

/// Remembers what a SELECT reported. Every sync pass over the inbox already
/// learns this for free, so the answer is usually there before anything asks.
fn remember_tag_capability(account_id: &str, supported: bool) {
    if let Ok(mut capabilities) = tag_capabilities().lock() {
        capabilities.insert(account_id.to_string(), (supported, Instant::now()));
    }
}

const GMAIL_LABEL_COLOR_PAIRS: &[(&str, &str)] = &[
    ("#000000", "#ffffff"),
    ("#434343", "#ffffff"),
    ("#666666", "#ffffff"),
    ("#999999", "#ffffff"),
    ("#cccccc", "#000000"),
    ("#efefef", "#000000"),
    ("#fb4c2f", "#ffffff"),
    ("#ffad47", "#000000"),
    ("#fad165", "#000000"),
    ("#16a766", "#ffffff"),
    ("#43d692", "#000000"),
    ("#4a86e8", "#ffffff"),
    ("#a479e2", "#ffffff"),
    ("#f691b3", "#000000"),
    ("#e66550", "#ffffff"),
    ("#285bac", "#ffffff"),
];

fn tag_color_allowed(background_color: &str, text_color: &str) -> bool {
    GMAIL_LABEL_COLOR_PAIRS
        .iter()
        .any(|pair| pair.0 == background_color && pair.1 == text_color)
}

/// Names a tag may not take. Every one of these is a mailbox somewhere: on
/// Gmail a label applied through `X-GM-LABELS` with one of these names does not
/// tag the message, it *moves* it — a "Trash" tag would throw mail away — and
/// in this app they are the fixed folder roles besides. A leading `\` or `$`
/// belongs to system flags, which the tag reader deliberately skips, so a tag
/// named that way would vanish on the next sync.
const RESERVED_TAG_NAMES: &[&str] = &[
    "inbox",
    "sent",
    "sent mail",
    "draft",
    "drafts",
    "spam",
    "junk",
    "trash",
    "bin",
    "deleted",
    "starred",
    "important",
    "archive",
    "all mail",
];

fn normalize_tag_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 225 {
        return Err("mail_account_label_name_invalid".to_string());
    }
    if name.starts_with('\\') || name.starts_with('$') {
        return Err("mail_account_label_reserved_name".to_string());
    }
    // A nested name is judged by each of its parts: "Trash" is reserved,
    // "Work/Trash" is a child folder of the user's own making.
    if name
        .split('/')
        .any(|part| RESERVED_TAG_NAMES.iter().any(|reserved| part.trim().eq_ignore_ascii_case(reserved)))
    {
        return Err("mail_account_label_reserved_name".to_string());
    }
    Ok(name.to_string())
}

/// Creates a local tag record. Nothing is sent to the server here regardless
/// of capability: IMAP has no primitive for an empty tag with no messages
/// yet, so the tag only becomes real on the server the first time
/// `set_message_tag` applies it — matching how the label picker already
/// creates then immediately applies a label. A server without keyword
/// support (no `\*` in PERMANENTFLAGS, and not Gmail) never blocks creation;
/// it just means `set_message_tag` will keep the tag local-only, the same
/// graceful degradation well-known IMAP clients (e.g. Thunderbird's message
/// tags) already use rather than hiding the feature outright.
pub async fn create_tag(
    app: &AppHandle,
    account_id: &str,
    name: &str,
) -> Result<crate::db::GmailLabel, String> {
    let name = normalize_tag_name(name)?;
    if crate::db::gmail_label_exists(app, account_id, &name)? {
        return Err("mail_account_label_name_taken".to_string());
    }
    let label = crate::db::GmailLabel {
        id: name.clone(),
        account_id: account_id.to_string(),
        name,
        background_color: None,
        text_color: None,
    };
    crate::db::upsert_gmail_label(app, &label)?;
    Ok(label)
}

/// Renames a tag: every message currently carrying the old name gets the new
/// one applied and the old one removed, then the local record (and every
/// `email_labels` row referencing it) moves to the new identity — an IMAP tag
/// has no id separate from its own name, unlike a Gmail REST label id.
pub async fn rename_tag(
    app: &AppHandle,
    account_id: &str,
    label_id: &str,
    name: &str,
) -> Result<crate::db::GmailLabel, String> {
    let new_name = normalize_tag_name(name)?;
    let Some(existing) = crate::db::get_gmail_label(app, account_id, label_id)? else {
        return Err("mail_account_label_missing".to_string());
    };
    // A nested tag is renamed with its parent, the way a folder takes its
    // subfolders along: leaving "Work/Q1" behind under a renamed "Work" would
    // strand it at the top of the list with a name that no longer means
    // anything.
    let child_prefix = format!("{}/", existing.name);
    let renames: Vec<(String, String)> = std::iter::once((existing.name.clone(), new_name.clone()))
        .chain(
            crate::db::get_gmail_labels_for_account(app, account_id)?
                .into_iter()
                .filter_map(|label| {
                    let suffix = label.name.strip_prefix(&child_prefix)?;
                    Some((label.name.clone(), format!("{new_name}/{suffix}")))
                }),
        )
        .collect();
    for (from, to) in &renames {
        if from != to && crate::db::gmail_label_exists(app, account_id, to)? {
            return Err("mail_account_label_name_taken".to_string());
        }
    }

    // A tag that never reached the server (see `create_tag`/`set_message_tag`
    // on a server without keyword support) has nothing to rename there either.
    if get_tag_capability(app, account_id).await? {
        let input = stored_account_input(app, account_id)?;
        let access_token = account_oauth_token(app, account_id).await?;
        let gmail_account = input.imap_host.eq_ignore_ascii_case("imap.gmail.com");
        let mailboxes = crate::db::get_imap_mailboxes(app, account_id)?;
        let server_renames = renames.clone();
        tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
            let mut session = connect_sync_imap(&input, access_token.as_deref())?;
            let mut server_deletes: Vec<String> = Vec::new();
            for (old_name, new_name) in server_renames {
                let apply_query = tag_store_query(gmail_account, &new_name, true);
                let remove_query = tag_store_query(gmail_account, &old_name, false);
                // Two names this server cannot tell apart — a keyword has no
                // room for a space, so "Q1 notes" and "Q1_notes" are one tag on
                // the wire. Applying and then removing it would leave the
                // messages with no tag at all, so only the local name changes.
                if apply_query.trim_start_matches('+') == remove_query.trim_start_matches('-') {
                    continue;
                }
                let found =
                    find_tagged_messages(&mut session, &mailboxes, gmail_account, &old_name)?;
                for (_, mailbox, uids) in found {
                    session
                        .select(&mailbox)
                        .map_err(|_| "mail_account_imap_failed".to_string())?;
                    let uid_set = uids
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join(",");
                    if store_on_selected(&mut session, &uid_set, &apply_query)?
                        == StoreOutcome::SessionSpent
                        || store_on_selected(&mut session, &uid_set, &remove_query)?
                            == StoreOutcome::SessionSpent
                    {
                        return Ok(());
                    }
                }
                // Gmail keeps a label alive as an empty mailbox after the last
                // message loses it, and the layout refresh would read it back
                // and put the old name in the sidebar again. Children first,
                // since deleting a parent takes its sub-labels with it.
                if gmail_account {
                    server_deletes.push(gmail_label_wire_name(&old_name));
                }
            }
            for mailbox in server_deletes.iter().rev() {
                // A label that was only ever local has no mailbox to delete.
                session.delete(mailbox).ok();
            }
            session.logout().ok();
            Ok(())
        })
        .await
        .map_err(|_| "mail_account_test_interrupted".to_string())??;
    }
    for (from, to) in &renames {
        crate::db::rename_gmail_label_local(app, account_id, from, to, to)?;
    }
    Ok(crate::db::GmailLabel {
        id: new_name.clone(),
        account_id: account_id.to_string(),
        name: new_name,
        background_color: existing.background_color,
        text_color: existing.text_color,
    })
}

/// Deletes a tag: removes it from every message that carries it, then the
/// local record and every `email_labels` row referencing it.
pub async fn delete_tag(app: &AppHandle, account_id: &str, label_id: &str) -> Result<(), String> {
    let Some(existing) = crate::db::get_gmail_label(app, account_id, label_id)? else {
        return Err("mail_account_label_missing".to_string());
    };
    // A tag that never reached the server has nothing to remove there either.
    if get_tag_capability(app, account_id).await? {
        let input = stored_account_input(app, account_id)?;
        let access_token = account_oauth_token(app, account_id).await?;
        let gmail_account = input.imap_host.eq_ignore_ascii_case("imap.gmail.com");
        let mailboxes = crate::db::get_imap_mailboxes(app, account_id)?;
        let name = existing.name.clone();
        tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
            let mut session = connect_sync_imap(&input, access_token.as_deref())?;
            let found = find_tagged_messages(&mut session, &mailboxes, gmail_account, &name)?;
            let remove_query = tag_store_query(gmail_account, &name, false);
            for (_, mailbox, uids) in found {
                session
                    .select(&mailbox)
                    .map_err(|_| "mail_account_imap_failed".to_string())?;
                let uid_set = uids
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(",");
                if store_on_selected(&mut session, &uid_set, &remove_query)?
                    == StoreOutcome::SessionSpent
                {
                    return Ok(());
                }
            }
            // On Gmail a label outlives the messages that carried it, as an
            // empty mailbox the next layout refresh would list and put straight
            // back in the sidebar. Deleting a label there deletes no mail.
            if gmail_account {
                session.delete(&gmail_label_wire_name(&name)).ok();
            }
            session.logout().ok();
            Ok(())
        })
        .await
        .map_err(|_| "mail_account_test_interrupted".to_string())??;
    }
    // Gmail deletes a label's sub-labels with it, and a tag whose parent is
    // gone reads as a stray either way, so the local list follows.
    let child_prefix = format!("{}/", existing.name);
    for child in crate::db::get_gmail_labels_for_account(app, account_id)? {
        if child.name.starts_with(&child_prefix) {
            crate::db::delete_gmail_label_local(app, account_id, &child.id)?;
        }
    }
    crate::db::delete_gmail_label_local(app, account_id, label_id)
}

/// Colors are local-only: neither Gmail's `X-GM-LABELS` extension nor a plain
/// IMAP keyword carries a color, so this never touches the server. Existing
/// Gmail label colors already stored server-side stop being read back from
/// here on; only a locally-set color persists.
pub async fn set_tag_color(
    app: &AppHandle,
    account_id: &str,
    label_id: &str,
    background_color: Option<String>,
    text_color: Option<String>,
) -> Result<crate::db::GmailLabel, String> {
    let Some(existing) = crate::db::get_gmail_label(app, account_id, label_id)? else {
        return Err("mail_account_label_missing".to_string());
    };
    let color = match (&background_color, &text_color) {
        (None, None) => (None, None),
        (Some(background), Some(text)) if tag_color_allowed(background, text) => {
            (Some(background.as_str()), Some(text.as_str()))
        }
        _ => return Err("mail_account_label_color_unsupported".to_string()),
    };
    crate::db::set_gmail_label_color_local(app, account_id, label_id, color.0, color.1)?;
    Ok(crate::db::GmailLabel {
        background_color,
        text_color,
        ..existing
    })
}

/// Applies or removes a tag on a whole thread. `STARRED` stays a thin wrapper
/// over the standard `\Flagged` IMAP flag, since it is a real message state
/// every server understands, not a label. Every other id is resolved to its
/// display name and carried as an arbitrary tag via `set_imap_message_tag` —
/// but only when the server can actually hold it (Gmail, or `\*` PERMANENTFLAGS
/// elsewhere); otherwise the tag stays local-only, same as a label created on
/// a server without keyword support (`create_tag`).
pub async fn set_message_tag(
    app: &AppHandle,
    account_id: &str,
    thread_id: &str,
    label_id: &str,
    applied: bool,
) -> Result<(), String> {
    if label_id == "STARRED" {
        set_imap_message_flag(
            app,
            account_id,
            thread_id,
            true,
            imap::types::Flag::Flagged,
            applied,
        )
        .await?;
    } else {
        let Some(existing) = crate::db::get_gmail_label(app, account_id, label_id)? else {
            return Err("mail_account_label_missing".to_string());
        };
        if get_tag_capability(app, account_id).await? {
            set_imap_message_tag(app, account_id, thread_id, true, &existing.name, applied)
                .await?;
        }
    }
    crate::db::set_thread_gmail_label_local(app, account_id, thread_id, label_id, applied)
}

#[tauri::command]
pub async fn create_gmail_label(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    name: String,
) -> Result<crate::db::GmailLabel, String> {
    crate::require_command_window(&window, &["main"])?;
    create_tag(&app, &account_id, &name).await
}

#[tauri::command]
pub async fn rename_gmail_label(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    label_id: String,
    name: String,
) -> Result<crate::db::GmailLabel, String> {
    crate::require_command_window(&window, &["main"])?;
    rename_tag(&app, &account_id, &label_id, &name).await
}

#[tauri::command]
pub async fn set_gmail_label_color(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    label_id: String,
    background_color: Option<String>,
    text_color: Option<String>,
) -> Result<crate::db::GmailLabel, String> {
    crate::require_command_window(&window, &["main"])?;
    set_tag_color(&app, &account_id, &label_id, background_color, text_color).await
}

#[tauri::command]
pub async fn delete_gmail_label(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    label_id: String,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    delete_tag(&app, &account_id, &label_id).await
}

#[tauri::command]
pub async fn set_thread_gmail_label(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    thread_id: String,
    label_id: String,
    applied: bool,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    set_message_tag(&app, &account_id, &thread_id, &label_id, applied).await
}


/// Downloads one attachment the sync deliberately left on the server. The
/// part's own MIME headers come along in the same command, so the bytes are
/// decoded by the same path that decodes a whole message.
pub async fn fetch_imap_attachment(
    app: &AppHandle,
    account_id: &str,
    email_id: &str,
    section: &str,
) -> Result<Vec<u8>, String> {
    let (label, uid) = imap_remote_ref(email_id)
        .ok_or_else(|| "mail_account_attachment_data_missing".to_string())?;
    let path = section
        .split('.')
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "mail_account_attachment_data_missing".to_string())?;
    if path.is_empty() {
        return Err("mail_account_attachment_data_missing".to_string());
    }
    let input = stored_account_input(app, account_id)?;
    let mailbox = mailbox_for_label(app, account_id, label)?;
    let access_token = account_oauth_token(app, account_id).await?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut session = connect_sync_imap(&input, access_token.as_deref())?;
        session
            .select(&mailbox)
            .map_err(|_| "mail_account_imap_failed".to_string())?;
        let section = path
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(".");
        let fetched = session
            .uid_fetch(
                uid.to_string(),
                format!("(BODY.PEEK[{section}.MIME] BODY.PEEK[{section}])"),
            )
            .map_err(|_| "mail_account_imap_failed".to_string())?;
        let message = fetched
            .iter()
            .next()
            .ok_or_else(|| "mail_account_attachment_data_missing".to_string())?;
        let mime = message
            .section(&imap_proto::SectionPath::Part(
                path.clone(),
                Some(imap_proto::MessageSection::Mime),
            ))
            .ok_or_else(|| "mail_account_attachment_data_missing".to_string())?;
        let body = message
            .section(&imap_proto::SectionPath::Part(path, None))
            .ok_or_else(|| "mail_account_attachment_data_missing".to_string())?;
        let mut raw = mime.to_vec();
        if !raw.ends_with(b"\r\n\r\n") {
            raw.extend_from_slice(b"\r\n");
        }
        raw.extend_from_slice(body);
        let decoded = mailparse::parse_mail(&raw)
            .map_err(|_| "mail_account_message_parse_failed".to_string())?
            .get_body_raw()
            .map_err(|_| "mail_account_message_parse_failed".to_string())?;
        session.logout().ok();
        Ok(decoded)
    })
    .await
    .map_err(|_| "mail_account_test_interrupted".to_string())?
}

/// Which server mailbox a cached message's UID belongs to.
fn mailbox_for_label(app: &AppHandle, account_id: &str, label: &str) -> Result<String, String> {
    let mappings = crate::db::get_imap_mailboxes(app, account_id)?
        .into_iter()
        .collect::<BTreeMap<_, _>>();
    mailbox_role_for_label(label, &mappings)
        .and_then(|role| mappings.get(&role).cloned())
        .ok_or_else(|| "mail_account_label_not_supported".to_string())
}

#[tauri::command]
pub async fn sync_imap_emails(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    // This runs right after an account is added, when nothing is cached yet.
    sync_imap_account(&app, &account_id, true).await
}

/// Starts the account's IMAP watcher, or does nothing when one already runs.
#[tauri::command]
pub async fn start_imap_watch(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    mailbox_role: String,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    start_imap_watcher(&app, &account_id, &mailbox_role);
    Ok(())
}

/// Ends the (account, mailbox) watcher. The connection closes at the end of
/// the IDLE cycle it is currently in, so this returns before the socket is gone.
#[tauri::command]
pub async fn stop_imap_watch(
    window: tauri::WebviewWindow,
    account_id: String,
    mailbox_role: String,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    stop_imap_watcher(&account_id, &mailbox_role);
    Ok(())
}

pub(crate) fn start_imap_watcher(app: &AppHandle, account_id: &str, role: &str) {
    let key = (account_id.to_string(), role.to_string());
    let mut watchers = imap_watchers()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if watchers.contains_key(&key) {
        return;
    }
    let watcher = Arc::new(ImapWatcher {
        stop: AtomicBool::new(false),
        idling: AtomicBool::new(false),
    });
    watchers.insert(key.clone(), Arc::clone(&watcher));
    drop(watchers);

    let app = app.clone();
    let thread_account = account_id.to_string();
    let thread_role = role.to_string();
    // A dedicated thread rather than the blocking pool: this one parks for the
    // lifetime of the (account, mailbox) pair instead of finishing a unit of work.
    if let Err(error) = std::thread::Builder::new()
        .name(format!("imap-watch-{role}"))
        .spawn(move || watch_imap_account(app, thread_account, thread_role, watcher))
    {
        eprintln!("[IMAP] watcher thread could not start: {error}");
        imap_watchers()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&key);
    }
}

pub(crate) fn stop_imap_watcher(account_id: &str, role: &str) {
    let watcher = imap_watchers()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&(account_id.to_string(), role.to_string()));
    if let Some(watcher) = watcher {
        watcher.stop.store(true, Ordering::Relaxed);
    }
}

/// Stops every watched mailbox for an account (inbox and any active-folder
/// watcher), regardless of role — used when the account itself is going away
/// and its credential must outlive no connection at all.
pub(crate) fn stop_all_imap_watchers_for_account(account_id: &str) {
    let mut watchers = imap_watchers()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let keys: Vec<_> = watchers
        .keys()
        .filter(|(id, _)| id == account_id)
        .cloned()
        .collect();
    for key in keys {
        if let Some(watcher) = watchers.remove(&key) {
            watcher.stop.store(true, Ordering::Relaxed);
        }
    }
}

/// One long-lived IMAP connection per watched (account, mailbox) pair, parked
/// on IDLE. Waking up runs a pass on that same connection, so a change costs a
/// SELECT and the fetch it implies rather than a fresh TLS handshake and login.
struct ImapWatcher {
    stop: AtomicBool,
    /// True only while the connection is actually parked on IDLE, which is when
    /// the server can be trusted to report flag changes.
    idling: AtomicBool,
}

/// Keyed by `(account_id, mailbox_role)` rather than just the account: the
/// inbox is always watched, and the mailbox currently open in the UI gets a
/// second connection so it enjoys the same push freshness (see
/// `watchableFolderRole` on the frontend, which decides what else to watch).
fn imap_watchers() -> &'static Mutex<HashMap<(String, String), Arc<ImapWatcher>>> {
    static WATCHERS: OnceLock<Mutex<HashMap<(String, String), Arc<ImapWatcher>>>> =
        OnceLock::new();
    WATCHERS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn mailbox_idle_active(account_id: &str, role: &str) -> bool {
    imap_watchers()
        .lock()
        .map(|watchers| {
            watchers
                .get(&(account_id.to_string(), role.to_string()))
                .is_some_and(|watcher| watcher.idling.load(Ordering::Relaxed))
        })
        .unwrap_or(false)
}

fn inbox_idle_active(account_id: &str) -> bool {
    mailbox_idle_active(account_id, "inbox")
}

/// Serializes the passes that write one account's cache. A watcher insert that
/// landed between a full sync's remote listing and its deletion pass would look
/// like a message the server no longer has, and be deleted again.
fn account_sync_gate(account_id: &str) -> Arc<Mutex<()>> {
    static GATES: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let mut gates = GATES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(gates.entry(account_id.to_string()).or_default())
}

enum WatchStep {
    /// Nothing left to watch: the account is gone, the server cannot IDLE, or
    /// the watcher was stopped.
    Done,
    /// The connection ended; reconnect after a delay. `idled` reports whether
    /// this attempt ever reached a healthy IDLE, which is what separates a
    /// dropped connection from a server or credential that is not working.
    Retry { idled: bool },
}

fn watch_imap_account(app: AppHandle, account_id: String, role: String, watcher: Arc<ImapWatcher>) {
    let mut retry_delay = WATCH_RETRY_DELAY;
    while !watcher.stop.load(Ordering::Relaxed) {
        match watch_imap_session(&app, &account_id, &role, &watcher) {
            WatchStep::Done => break,
            WatchStep::Retry { idled } => {
                // A connection that worked and then dropped is not evidence of
                // anything being wrong, so it starts the backoff over.
                if idled {
                    retry_delay = WATCH_RETRY_DELAY;
                }
                if !watch_sleep(&watcher, retry_delay) {
                    break;
                }
                retry_delay = (retry_delay * 2).min(WATCH_RETRY_MAX_DELAY);
            }
        }
    }
    let key = (account_id, role);
    let mut watchers = imap_watchers()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // A stop followed by a start leaves a newer watcher registered under this
    // key, and it must not be removed by the thread it replaced.
    if watchers
        .get(&key)
        .is_some_and(|current| Arc::ptr_eq(current, &watcher))
    {
        watchers.remove(&key);
    }
}

/// Sleeps in slices so a stopped watcher leaves early. Returns false when the
/// watcher was stopped while waiting.
fn watch_sleep(watcher: &ImapWatcher, duration: Duration) -> bool {
    let mut remaining = duration;
    while !remaining.is_zero() {
        if watcher.stop.load(Ordering::Relaxed) {
            return false;
        }
        let slice = remaining.min(WATCH_STOP_POLL);
        std::thread::sleep(slice);
        remaining -= slice;
    }
    !watcher.stop.load(Ordering::Relaxed)
}

/// Holds one connection for as long as it stays healthy: park on IDLE, and on
/// every wake run a pass for `role` over the same session. `role` is "inbox"
/// for the always-on watcher, or the mailbox currently open in the UI for the
/// second, optional watcher (see `watchableFolderRole` on the frontend).
fn watch_imap_session(app: &AppHandle, account_id: &str, role: &str, watcher: &ImapWatcher) -> WatchStep {
    let Ok(input) = stored_account_input(app, account_id) else {
        // No stored credential means there is nothing to watch until the
        // account is set up again, which starts a new watcher.
        return WatchStep::Done;
    };
    let access_token = match tauri::async_runtime::block_on(account_oauth_token(app, account_id)) {
        Ok(token) => token,
        Err(error) => {
            // A revoked credential will not come back on its own, and this
            // thread is the only one still trying: say so, or the account goes
            // quiet with no explanation. The watcher keeps its backoff, so a
            // sign-in is picked up by the next attempt.
            if is_session_revoked(&error) {
                let payload = ImapChangePayload {
                    account_id: account_id.to_string(),
                };
                if let Err(error) = app.emit("mail-session-expired", payload) {
                    eprintln!("[IMAP] session-expired event failed: {error}");
                }
            }
            return WatchStep::Retry { idled: false };
        }
    };
    let gmail_account = input.imap_host.eq_ignore_ascii_case("imap.gmail.com");
    let Some(mailbox) = crate::db::get_imap_mailboxes(app, account_id)
        .ok()
        .and_then(|mailboxes| {
            mailboxes
                .into_iter()
                .find_map(|(mailbox_role, mailbox)| (mailbox_role == role).then_some(mailbox))
        })
    else {
        // Not discovered (yet), or the role was torn down between the
        // frontend's request and this connection attempt — nothing to watch.
        return WatchStep::Done;
    };
    let Some(label) = mailbox_label(role, gmail_account).map(str::to_string) else {
        return WatchStep::Done;
    };
    let Ok(mut session) = connect_sync_imap(&input, access_token.as_deref()) else {
        return WatchStep::Retry { idled: false };
    };
    if !session_supports(&mut session, "IDLE") {
        // Without IDLE the periodic sync is the only freshness this server can
        // offer, and a parked connection would buy nothing.
        imap_log(|| "watcher: server does not advertise IDLE, not watching".to_string());
        session.logout().ok();
        return WatchStep::Done;
    }
    let condstore = session_supports(&mut session, "CONDSTORE");
    imap_log(|| format!("watcher: connected role={role}, condstore={condstore}"));

    // A connection that has just been opened cannot know what happened while it
    // was down, so the first pass always runs.
    let mut pending_change = true;
    let mut idled = false;
    loop {
        if watcher.stop.load(Ordering::Relaxed) {
            break;
        }
        // Pausing sync (or a hidden window with notifications off) has to stop
        // the mail traffic, not only the UI updates. The connection stays
        // parked so a later wake needs no reconnect, and `pending_change`
        // stays set so the next allowed pass catches up.
        let controls = crate::settings::read_app_controls(app);
        let skip = should_skip_sync(
            controls.mail_sync_paused,
            controls.notifications_disabled(),
            main_window_hidden(app),
        );
        if pending_change && !skip {
            match sync_watched_mailbox(
                app,
                account_id,
                &mut session,
                &mailbox,
                role,
                &label,
                gmail_account,
                condstore,
            ) {
                Ok(changed) => {
                    pending_change = false;
                    if changed {
                        let payload = ImapChangePayload {
                            account_id: account_id.to_string(),
                        };
                        if let Err(error) = app.emit("imap-mailbox-changed", payload) {
                            eprintln!("[IMAP] change event failed: {error}");
                        }
                    }
                }
                // The session is the thing in doubt after a failed pass, so drop
                // it and come back on a fresh connection.
                Err(_) => return WatchStep::Retry { idled },
            }
        }
        if watcher.stop.load(Ordering::Relaxed) {
            break;
        }

        watcher.idling.store(true, Ordering::Relaxed);
        let outcome = session
            .idle()
            .and_then(|handle| handle.wait_with_timeout(IDLE_CYCLE));
        watcher.idling.store(false, Ordering::Relaxed);
        match outcome {
            Ok(WaitOutcome::MailboxChanged) => {
                imap_log(|| "watcher: woken by the server".to_string());
                idled = true;
                pending_change = true;
            }
            // The cycle expired with nothing to report; re-issue IDLE so the
            // server keeps the connection.
            Ok(WaitOutcome::TimedOut) => idled = true,
            Err(_) => return WatchStep::Retry { idled },
        }
    }
    session.logout().ok();
    WatchStep::Done
}

/// Runs a pass for `role` on the watcher's own session and reports whether
/// the cache moved. Forced, because a wake says something changed without
/// saying what, and only a pass can tell new mail from a flag edit.
fn sync_watched_mailbox(
    app: &AppHandle,
    account_id: &str,
    session: &mut OAuthImapSession,
    mailbox: &str,
    role: &str,
    label: &str,
    gmail_account: bool,
    condstore: bool,
) -> Result<bool, String> {
    let gate = account_sync_gate(account_id);
    let _guard = gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let generation = crate::db::get_account_cache_generation(app, account_id)?;
    let pass = MailboxSync {
        app,
        account_id,
        role,
        mailbox,
        label,
        generation,
        gmail_account,
        condstore,
        forced: true,
    };
    match pass.run(session) {
        Ok(MailboxOutcome::Ran { changed }) => Ok(changed),
        Ok(MailboxOutcome::Skipped) => Ok(false),
        Err(MailboxSyncError::Select) => Err("mail_account_imap_failed".to_string()),
        Err(MailboxSyncError::Failed(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        changed_since_fetch_query, decode_text_part, fallback_mailbox_role,
        gmail_archive_store_query, gmail_unarchive_store_query, imap_auth_failed_code,
        imap_remote_ref, imap_watchers, is_gmail_system_view, mailbox_idle_active, mailbox_label,
        mailbox_role_for_label, normalize_tag_name, tag_names_from_flags, uids_by_source_role,
        message_layout, message_to_email, parse_selected_mailbox, plan_mailbox_pass,
        quote_mailbox, quote_tag_value, resolve_mailbox_role, sanitize_tag_keyword,
        should_skip_sync, stop_imap_watcher, strip_bcc_header, tag_search_query, tag_store_query,
        FetchedMessage, ImapAccountInput, ImapWatcher, MailSecurity, MailboxPlan, MessageLayout,
        MessagePart, SelectedMailbox,
    };
    use std::collections::BTreeMap;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    #[test]
    fn changed_since_is_a_fetch_modifier_not_a_uid_set_suffix() {
        assert_eq!(
            changed_since_fetch_query(405),
            "(UID FLAGS) (CHANGEDSINCE 405)"
        );
    }

    #[test]
    fn paused_sync_is_skipped_regardless_of_notifications_or_visibility() {
        assert!(should_skip_sync(true, false, false));
        assert!(should_skip_sync(true, true, true));
    }

    #[test]
    fn a_hidden_window_only_skips_sync_once_notifications_are_also_off() {
        assert!(!should_skip_sync(false, false, true));
        assert!(!should_skip_sync(false, true, false));
        assert!(should_skip_sync(false, true, true));
    }

    #[test]
    fn gmail_archive_removes_the_inbox_label() {
        assert_eq!(gmail_archive_store_query(), "-X-GM-LABELS.SILENT (\\Inbox)");
    }

    #[test]
    fn a_gmail_tag_stores_and_searches_through_x_gm_labels() {
        assert_eq!(
            tag_store_query(true, "Projects/Q1", true),
            "+X-GM-LABELS.SILENT (\"Projects/Q1\")"
        );
        assert_eq!(
            tag_store_query(true, "Projects/Q1", false),
            "-X-GM-LABELS.SILENT (\"Projects/Q1\")"
        );
        assert_eq!(
            tag_search_query(true, "Projects/Q1"),
            "X-GM-LABELS \"Projects/Q1\""
        );
    }

    #[test]
    fn a_gmail_tag_value_escapes_quotes_and_backslashes() {
        assert_eq!(
            quote_tag_value(r#"Weird"Name\"#),
            r#""Weird\"Name\\""#
        );
    }

    #[test]
    fn a_generic_tag_stores_and_searches_through_a_keyword() {
        assert_eq!(
            tag_store_query(false, "Work", true),
            "+FLAGS.SILENT (Work)"
        );
        assert_eq!(tag_search_query(false, "Work"), "KEYWORD Work");
    }

    #[test]
    fn a_keyword_cannot_carry_atom_breaking_characters_onto_the_wire() {
        assert_eq!(sanitize_tag_keyword("Work Trip"), "Work_Trip");
        assert_eq!(sanitize_tag_keyword(r#"a(b)c{d}e%f*g"h\i]j"#), "a_b_c_d_e_f_g_h_i_j");
        assert_eq!(sanitize_tag_keyword("Projects/Q1"), "Projects/Q1");
    }

    /// A mailbox with 50 messages whose next UID is 100, synced a moment ago on
    /// a server without CONDSTORE.
    fn synced_state() -> crate::db::ImapMailboxState {
        crate::db::ImapMailboxState {
            uid_validity: 7,
            uid_next: 100,
            exists_count: 50,
            highest_mod_seq: 0,
            reconciled_at: 0,
        }
    }

    fn reported(uid_next: u32, exists: u32) -> SelectedMailbox {
        SelectedMailbox {
            uid_validity: 7,
            uid_next,
            exists,
            highest_mod_seq: 0,
            supports_custom_keywords: false,
        }
    }

    fn plan(uid_next: u32, exists: u32, forced: bool) -> MailboxPlan {
        plan_mailbox_pass(
            &synced_state(),
            &reported(uid_next, exists),
            false,
            forced,
            false,
        )
    }

    #[test]
    fn a_matching_checkpoint_costs_nothing() {
        assert_eq!(plan(100, 50, false), MailboxPlan::Skip);
    }

    #[test]
    fn two_new_messages_are_fetched_without_relisting_the_mailbox() {
        // The wake says something changed; the checkpoint says all of it was
        // appended, so the pass asks about the new range only.
        assert_eq!(plan(102, 52, true), MailboxPlan::NewMessages);
    }

    #[test]
    fn a_wake_that_moved_no_uid_can_only_be_a_flag_change() {
        // Read or starred elsewhere: nothing else explains a change with the
        // same UIDNEXT and count, and without CONDSTORE only a full listing can
        // find it.
        assert_eq!(plan(100, 50, true), MailboxPlan::Reconcile);
    }

    #[test]
    fn an_expunge_forces_the_pass_that_can_see_deletions() {
        assert_eq!(plan(100, 49, false), MailboxPlan::Reconcile);
        // One appended and one expunged leaves the count where it was, but
        // UIDNEXT still moved, so the two no longer agree.
        assert_eq!(plan(101, 50, true), MailboxPlan::Reconcile);
    }

    #[test]
    fn a_server_that_skips_uids_is_not_trusted_to_be_additions_only() {
        // UIDNEXT jumped by three while two messages appeared, so something the
        // range fetch would not describe happened as well.
        assert_eq!(plan(103, 52, true), MailboxPlan::Reconcile);
    }

    #[test]
    fn the_reconcile_timer_still_reaches_a_mailbox_that_only_gains_messages() {
        let due = plan_mailbox_pass(&synced_state(), &reported(102, 52), true, true, false);
        assert_eq!(due, MailboxPlan::Reconcile);
    }

    #[test]
    fn a_mailbox_without_usable_checkpoints_always_reconciles() {
        // Never synced, reassigned UIDs, and a server that withholds UIDNEXT.
        let fresh = crate::db::ImapMailboxState::default();
        assert_eq!(
            plan_mailbox_pass(&fresh, &reported(100, 50), false, false, false),
            MailboxPlan::Reconcile
        );
        let reassigned = SelectedMailbox {
            uid_validity: 9,
            ..reported(102, 52)
        };
        assert_eq!(
            plan_mailbox_pass(&synced_state(), &reassigned, false, false, false),
            MailboxPlan::Reconcile
        );
        assert_eq!(
            plan_mailbox_pass(&synced_state(), &reported(0, 52), false, false, false),
            MailboxPlan::Reconcile
        );
    }

    #[test]
    fn a_search_backed_mailbox_never_takes_the_range_shortcut() {
        // Gmail's All Mail is reached through a search, so a UID range says
        // nothing about it.
        assert_eq!(
            plan_mailbox_pass(&synced_state(), &reported(102, 52), false, false, true),
            MailboxPlan::Reconcile
        );
    }

    /// The same mailbox on a CONDSTORE server, last synced at sequence 400.
    fn condstore_state() -> crate::db::ImapMailboxState {
        crate::db::ImapMailboxState {
            highest_mod_seq: 400,
            ..synced_state()
        }
    }

    fn condstore_reported(uid_next: u32, exists: u32, highest_mod_seq: u64) -> SelectedMailbox {
        SelectedMailbox {
            highest_mod_seq,
            ..reported(uid_next, exists)
        }
    }

    #[test]
    fn condstore_answers_a_flag_change_without_listing_the_mailbox() {
        // Same UIDNEXT and count, higher sequence: something was read or
        // starred, and CHANGEDSINCE names exactly which messages.
        let decision = plan_mailbox_pass(
            &condstore_state(),
            &condstore_reported(100, 50, 405),
            false,
            true,
            false,
        );
        assert_eq!(decision, MailboxPlan::ChangedSince);
    }

    #[test]
    fn a_sequence_that_moved_is_read_even_without_a_wake() {
        // A label or a flag changed in another client: no UID moved, nothing
        // woke this pass, and only the modification sequence says so.
        let decision = plan_mailbox_pass(
            &condstore_state(),
            &condstore_reported(100, 50, 405),
            false,
            false,
            false,
        );
        assert_eq!(decision, MailboxPlan::ChangedSince);
    }

    #[test]
    fn condstore_covers_new_messages_with_the_same_command() {
        let decision = plan_mailbox_pass(
            &condstore_state(),
            &condstore_reported(102, 52, 410),
            false,
            true,
            false,
        );
        assert_eq!(decision, MailboxPlan::ChangedSince);
    }

    #[test]
    fn an_unmoved_sequence_ends_a_woken_pass_without_asking_anything() {
        // A wake with every checkpoint identical, the modification sequence
        // included, cannot have been a change to this mailbox's contents.
        let decision = plan_mailbox_pass(
            &condstore_state(),
            &condstore_reported(100, 50, 400),
            false,
            true,
            false,
        );
        assert_eq!(decision, MailboxPlan::Skip);
        // Without CONDSTORE the same wake has to be taken at face value.
        assert_eq!(plan(100, 50, true), MailboxPlan::Reconcile);
    }

    #[test]
    fn a_server_without_condstore_never_reaches_the_condstore_path() {
        // The whole fallback decision table: whatever the mailbox looks like, a
        // pass can only ask CHANGEDSINCE when both sides carry a sequence.
        for uid_next in [0, 99, 100, 101, 103] {
            for exists in [0, 49, 50, 52] {
                for forced in [false, true] {
                    for due in [false, true] {
                        let decision = plan_mailbox_pass(
                            &synced_state(),
                            &reported(uid_next, exists),
                            due,
                            forced,
                            false,
                        );
                        assert_ne!(
                            decision,
                            MailboxPlan::ChangedSince,
                            "uid_next={uid_next} exists={exists} forced={forced} due={due}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_server_that_drops_condstore_falls_back_instead_of_asking_for_nothing() {
        // The mailbox has a stored sequence but this connection's server did not
        // report one, so the range path has to carry the pass.
        let decision =
            plan_mailbox_pass(&condstore_state(), &reported(102, 52), false, true, false);
        assert_eq!(decision, MailboxPlan::NewMessages);
        // And a flag-only wake is back to costing a full listing.
        let decision =
            plan_mailbox_pass(&condstore_state(), &reported(100, 50), false, true, false);
        assert_eq!(decision, MailboxPlan::Reconcile);
    }

    #[test]
    fn reassigned_uids_reconcile_even_when_the_sequence_looks_familiar() {
        // A new UIDVALIDITY is a new numbering: the sequence that came with it
        // belongs to a different generation and must not be compared to the
        // stored one, however equal the two happen to look.
        let reassigned = SelectedMailbox {
            uid_validity: 9,
            uid_next: 100,
            exists: 50,
            highest_mod_seq: 400,
            supports_custom_keywords: false,
        };
        let decision = plan_mailbox_pass(&condstore_state(), &reassigned, false, true, false);
        assert_eq!(decision, MailboxPlan::Reconcile);
        // Not even an unforced pass may skip it.
        let decision = plan_mailbox_pass(&condstore_state(), &reassigned, false, false, false);
        assert_eq!(decision, MailboxPlan::Reconcile);
    }

    #[test]
    fn a_message_whose_body_never_arrived_is_not_stored() {
        let part = MessagePart {
            path: vec![1],
            mime_type: "text/plain".to_string(),
            encoding: "7bit".to_string(),
            charset: None,
            filename: None,
            octets: 10,
        };
        let mut message = FetchedMessage {
            uid: 7,
            unread: true,
            starred: false,
            header: b"Subject: Hi\r\n\r\n".to_vec(),
            plain: String::new(),
            html: String::new(),
            text_parts: vec![part],
            attachments: Vec::new(),
        };
        assert!(message.body_missing());
        message.plain = "arrived".to_string();
        assert!(!message.body_missing());
        // A message that promised no text at all is complete without any.
        message.plain = String::new();
        message.text_parts.clear();
        assert!(!message.body_missing());
    }

    #[test]
    fn a_mailbox_synced_before_condstore_reconciles_once_to_get_a_sequence() {
        // The stored 0 is a checkpoint from a connection that had no CONDSTORE,
        // so there is no sequence to ask CHANGEDSINCE about yet.
        let decision = plan_mailbox_pass(
            &synced_state(),
            &condstore_reported(102, 52, 410),
            false,
            false,
            false,
        );
        assert_eq!(decision, MailboxPlan::Reconcile);
    }

    #[test]
    fn a_select_reply_yields_the_condstore_checkpoint() {
        let reply = b"* 231 EXISTS\r\n\
                      * 0 RECENT\r\n\
                      * OK [UIDVALIDITY 3857529045] UIDs valid\r\n\
                      * OK [UIDNEXT 44292] Predicted next UID\r\n\
                      * OK [PERMANENTFLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft \\*)] Flags permitted\r\n\
                      * OK [HIGHESTMODSEQ 90060128194045007] Highest\r\n";
        let selected = parse_selected_mailbox(reply).expect("parse select reply");
        assert_eq!(
            selected,
            SelectedMailbox {
                uid_validity: 3_857_529_045,
                uid_next: 44_292,
                exists: 231,
                highest_mod_seq: 90_060_128_194_045_007,
                supports_custom_keywords: true,
            }
        );
    }

    #[test]
    fn a_select_reply_without_a_wildcard_permanent_flag_cannot_create_keywords() {
        let reply = b"* 4 EXISTS\r\n\
                      * OK [UIDVALIDITY 1] UIDs valid\r\n\
                      * OK [UIDNEXT 5] Predicted next UID\r\n\
                      * OK [PERMANENTFLAGS (\\Answered \\Flagged \\Deleted \\Seen)] Flags permitted\r\n";
        let selected = parse_selected_mailbox(reply).expect("parse select reply");
        assert!(!selected.supports_custom_keywords);
    }

    #[test]
    fn a_server_without_condstore_reports_no_sequence() {
        let reply = b"* 3 EXISTS\r\n* OK [UIDVALIDITY 1] valid\r\n* OK [UIDNEXT 9] next\r\n";
        let selected = parse_selected_mailbox(reply).expect("parse select reply");
        assert_eq!(selected.highest_mod_seq, 0);
        assert_eq!(selected.exists, 3);
    }

    #[test]
    fn mailbox_names_cannot_carry_a_second_command_onto_the_wire() {
        assert_eq!(quote_mailbox("INBOX").as_deref(), Ok("\"INBOX\""));
        // Quotes and backslashes end or escape the string they sit in.
        assert_eq!(
            quote_mailbox(r#"Odd\Name"Here"#).as_deref(),
            Ok(r#""Odd\\Name\"Here""#)
        );
        assert!(quote_mailbox("INBOX\r\nA1 DELETE \"x\"").is_err());
    }

    #[test]
    fn only_cacheable_roles_map_to_a_system_mailbox() {
        assert_eq!(mailbox_label("junk", false), Some("spam"));
        assert_eq!(mailbox_label("all", true), Some("archive"));
        // All Mail is Gmail's own shape, and drafts are fetched live, never
        // mirrored into the local cache.
        assert_eq!(mailbox_label("all", false), None);
        assert_eq!(mailbox_label("drafts", true), None);
    }

    #[test]
    fn a_custom_folder_is_cached_under_its_own_role() {
        assert_eq!(mailbox_label("custom:Work", false), Some("custom:Work"));
        assert_eq!(
            mailbox_label("custom:Projects/2026", true),
            Some("custom:Projects/2026")
        );
    }

    #[test]
    fn an_unrecognized_folder_keeps_its_path_as_a_role_instead_of_being_dropped() {
        assert_eq!(
            resolve_mailbox_role(None, "Work", false),
            Some("custom:Work".to_string())
        );
        assert_eq!(
            resolve_mailbox_role(None, "Projects/2026", false),
            Some("custom:Projects/2026".to_string())
        );
        // Recognized names still resolve to their fixed role, not a custom one.
        assert_eq!(
            resolve_mailbox_role(None, "Sent Mail", false),
            Some("sent".to_string())
        );
        assert_eq!(
            resolve_mailbox_role(Some("sent"), "Gesendet", false),
            Some("sent".to_string())
        );
    }

    #[test]
    fn a_gmail_mailbox_with_no_recognized_role_is_a_label_not_a_custom_folder() {
        // Gmail LISTs every label as its own mailbox in addition to exposing
        // it via X-GM-LABELS; treating it as a custom folder too would
        // duplicate every label in the sidebar's Folders section.
        assert_eq!(resolve_mailbox_role(None, "Projects/Q1", true), None);
        assert_eq!(resolve_mailbox_role(None, "Newsletters", true), None);
        // System roles still resolve normally on Gmail.
        assert_eq!(
            resolve_mailbox_role(Some("sent"), "[Gmail]/Sent Mail", true),
            Some("sent".to_string())
        );
        assert_eq!(
            resolve_mailbox_role(None, "Sent Mail", true),
            Some("sent".to_string())
        );
    }

    #[test]
    fn gmails_starred_and_important_views_are_neither_a_role_nor_a_label() {
        assert!(is_gmail_system_view("\\flagged"));
        assert!(is_gmail_system_view("\\important"));
        assert!(!is_gmail_system_view("\\sent"));
        assert!(!is_gmail_system_view("\\projects"));
    }

    #[test]
    fn only_outlook_gets_the_imap_not_enabled_hint_on_auth_failure() {
        assert_eq!(
            imap_auth_failed_code("outlook.office365.com"),
            "mail_account_outlook_auth_failed"
        );
        assert_eq!(
            imap_auth_failed_code("OUTLOOK.OFFICE365.COM"),
            "mail_account_outlook_auth_failed"
        );
        assert_eq!(imap_auth_failed_code("imap.gmail.com"), "mail_account_auth_failed");
        assert_eq!(imap_auth_failed_code("imap.mail.yahoo.com"), "mail_account_auth_failed");
    }

    fn valid_input() -> ImapAccountInput {
        ImapAccountInput {
            email: " Person@Example.COM ".to_string(),
            username: "person@example.com".to_string(),
            password: "secret".to_string(),
            imap_host: " IMAP.Example.com. ".to_string(),
            imap_port: 993,
            imap_security: MailSecurity::Tls,
            smtp_host: "smtp.example.com".to_string(),
            smtp_port: 587,
            smtp_security: MailSecurity::Starttls,
        }
    }

    #[test]
    fn normalizes_safe_account_input() {
        let input = valid_input().normalized().expect("valid input");
        assert_eq!(input.email, "person@example.com");
        assert_eq!(input.imap_host, "imap.example.com");
    }

    #[test]
    fn recognizes_common_special_mailbox_names() {
        assert_eq!(fallback_mailbox_role("[Gmail]/Sent Mail"), Some("sent"));
        assert_eq!(fallback_mailbox_role("Deleted Items"), Some("trash"));
        assert_eq!(fallback_mailbox_role("Junk Email"), Some("junk"));
        assert_eq!(fallback_mailbox_role("Archive"), Some("archive"));
        assert_eq!(fallback_mailbox_role("Projects"), None);
    }

    #[test]
    fn a_tag_may_not_be_named_after_a_mailbox() {
        // On Gmail this is not a naming quibble: applying a label called
        // "Trash" through X-GM-LABELS moves the message to the bin.
        for reserved in ["Trash", "inbox", "SPAM", " Drafts ", "Starred", "All Mail"] {
            assert!(
                normalize_tag_name(reserved).is_err(),
                "{reserved} should be reserved"
            );
        }
        // System flags are skipped by the tag reader, so a tag shaped like one
        // would disappear on the next sync.
        assert!(normalize_tag_name("\\Seen").is_err());
        assert!(normalize_tag_name("$label1").is_err());
        // A folder of the user's own, nested under their own name, is fine.
        assert_eq!(normalize_tag_name(" Work/Trash notes ").unwrap(), "Work/Trash notes");
        assert_eq!(normalize_tag_name("Faturalar").unwrap(), "Faturalar");
    }

    #[test]
    fn only_user_keywords_become_tags() {
        let flags = ["\\Seen", "\\Important", "$Forwarded", "Work", "Fatura"];
        assert_eq!(
            tag_names_from_flags(flags.into_iter()),
            vec!["Fatura".to_string(), "Work".to_string()]
        );
    }

    #[test]
    fn parses_imap_remote_ids_without_confusing_mailbox_roles() {
        assert_eq!(imap_remote_ref("imap:inbox:42"), Some(("inbox", 42)));
        assert_eq!(imap_remote_ref("imap:spam:7"), Some(("spam", 7)));
        assert_eq!(imap_remote_ref("gmail-message-id"), None);
        let mappings = mailbox_map(&[
            ("inbox", "INBOX"),
            ("junk", "Junk"),
            ("sent", "Sent"),
            ("custom:Work", "Work"),
        ]);
        assert_eq!(
            mailbox_role_for_label("spam", &mappings).as_deref(),
            Some("junk")
        );
        assert_eq!(
            mailbox_role_for_label("sent", &mappings).as_deref(),
            Some("sent")
        );
        // A custom-folder label carries its own colon; rsplit_once still finds
        // the UID at the end rather than splitting on the label's colon.
        assert_eq!(
            imap_remote_ref("imap:custom:Work:99"),
            Some(("custom:Work", 99))
        );
        assert_eq!(
            mailbox_role_for_label("custom:Work", &mappings).as_deref(),
            Some("custom:Work")
        );
        // A role this account has no mailbox for resolves to nothing, rather
        // than to a name that would then be selected by mistake.
        assert!(mailbox_role_for_label("archive", &mappings).is_none());
    }

    fn mailbox_map(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(role, mailbox)| (role.to_string(), mailbox.to_string()))
            .collect()
    }

    #[test]
    fn a_gmail_archive_label_resolves_back_to_all_mail() {
        // Gmail lists no archive folder, so the sync caches All Mail under the
        // archive label. Every command that reaches one of those messages has
        // to find its way back to the `all` role.
        let gmail = mailbox_map(&[("inbox", "INBOX"), ("all", "[Gmail]/All Mail")]);
        assert_eq!(
            mailbox_role_for_label("archive", &gmail).as_deref(),
            Some("all")
        );
        let grouped = uids_by_source_role(
            &[
                "imap:archive:11".to_string(),
                "imap:inbox:12".to_string(),
                "imap:archive:13".to_string(),
                "imap:trash:14".to_string(),
            ],
            &gmail,
        );
        assert_eq!(grouped.get("all"), Some(&vec![11, 13]));
        assert_eq!(grouped.get("inbox"), Some(&vec![12]));
        // Trash is not mapped for this account, so those UIDs are dropped
        // instead of being looked up in some other mailbox.
        assert!(!grouped.contains_key("trash"));
    }

    #[test]
    fn a_server_with_its_own_archive_folder_keeps_using_it() {
        let plain = mailbox_map(&[("inbox", "INBOX"), ("archive", "Archive"), ("all", "All")]);
        assert_eq!(
            mailbox_role_for_label("archive", &plain).as_deref(),
            Some("archive")
        );
    }

    #[test]
    fn restoring_a_gmail_archive_puts_the_inbox_label_back() {
        assert_eq!(
            gmail_unarchive_store_query(),
            "+X-GM-LABELS.SILENT (\\Inbox)"
        );
    }

    #[test]
    fn removes_bcc_header_and_its_folded_lines_before_smtp_delivery() {
        let raw = "To: a@example.test\r\nBcc: hidden@example.test,\r\n another@example.test\r\nSubject: Hi\r\n\r\nBody";
        assert_eq!(
            strip_bcc_header(raw),
            "To: a@example.test\r\nSubject: Hi\r\n\r\nBody"
        );
    }

    #[test]
    fn rejects_invalid_hosts_and_empty_passwords() {
        let mut input = valid_input();
        input.imap_host = "127.0.0.1 / bad".to_string();
        assert_eq!(input.normalized().unwrap_err(), "mail_account_invalid_host");

        let mut input = valid_input();
        input.password.clear();
        assert_eq!(
            input.normalized().unwrap_err(),
            "mail_account_password_required"
        );
    }

    #[test]
    fn rejects_zero_ports() {
        let mut input = valid_input();
        input.smtp_port = 0;
        assert_eq!(input.normalized().unwrap_err(), "mail_account_invalid_port");
    }

    #[test]
    fn parses_rfc_message_for_local_cache() {
        let header = b"From: Alice <alice@example.test>\r\nTo: Bob <bob@example.test>\r\nSubject: Hello\r\nDate: Tue, 30 Jul 2024 10:00:00 +0000\r\nMessage-ID: <child@example.test>\r\nIn-Reply-To: <root@example.test>\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n";
        let message = FetchedMessage {
            uid: 42,
            unread: true,
            starred: false,
            header: header.to_vec(),
            plain: "Hello from IMAP".to_string(),
            html: String::new(),
            text_parts: Vec::new(),
            attachments: Vec::new(),
        };
        let (email, attachments) =
            message_to_email("bob@example.test", "inbox", &message).expect("build message");
        assert_eq!(email.id, "imap:inbox:42");
        assert_eq!(email.thread_id, "<root@example.test>");
        assert_eq!(email.subject, "Hello");
        assert!(email.body_html.contains("Hello from IMAP"));
        assert_eq!(email.snippet, "Hello from IMAP");
        assert!(email.unread);
        assert!(attachments.is_empty());
    }

    /// Parses a real `BODYSTRUCTURE` reply, so the walk is exercised against
    /// the shape a server actually sends rather than a hand-built tree.
    fn layout_of(fetch_reply: &[u8]) -> MessageLayout {
        let (_, response) = imap_proto::parse_response(fetch_reply).expect("parse fetch");
        let imap_proto::Response::Fetch(_, attributes) = response else {
            panic!("expected a FETCH response");
        };
        let structure = attributes
            .iter()
            .find_map(|attribute| match attribute {
                imap_proto::AttributeValue::BodyStructure(structure) => Some(structure),
                _ => None,
            })
            .expect("bodystructure present");
        message_layout(structure)
    }

    #[test]
    fn a_plain_message_downloads_its_only_part() {
        let layout = layout_of(
            b"* 1 FETCH (BODYSTRUCTURE (\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 24 1))\r\n",
        );
        assert_eq!(layout.text.len(), 1);
        // A message that is not multipart still has its body at section 1.
        assert_eq!(layout.text[0].section(), "1");
        assert_eq!(layout.text[0].charset.as_deref(), Some("utf-8"));
        assert!(layout.attachments.is_empty());
    }

    #[test]
    fn an_attachment_is_recorded_but_never_downloaded() {
        let layout = layout_of(
            b"* 1 FETCH (BODYSTRUCTURE ((\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 40 2)\
              (\"APPLICATION\" \"PDF\" (\"NAME\" \"invoice.pdf\") NIL NIL \"BASE64\" 4000 NIL) \
              \"MIXED\"))\r\n",
        );
        // Only the text part is worth bytes up front.
        assert_eq!(
            layout
                .text
                .iter()
                .map(MessagePart::section)
                .collect::<Vec<_>>(),
            vec!["1"]
        );
        assert_eq!(layout.attachments.len(), 1);
        let attachment = &layout.attachments[0];
        assert_eq!(attachment.section(), "2");
        assert_eq!(attachment.filename.as_deref(), Some("invoice.pdf"));
        assert_eq!(attachment.mime_type, "application/pdf");
        // Base64 carries three bytes in four, so the wire size overstates it.
        assert_eq!(attachment.decoded_size(), 3000);
    }

    #[test]
    fn both_alternatives_of_a_nested_message_are_downloaded() {
        let layout = layout_of(
            b"* 1 FETCH (BODYSTRUCTURE (((\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"QUOTED-PRINTABLE\" 10 1)\
              (\"TEXT\" \"HTML\" (\"CHARSET\" \"utf-8\") NIL NIL \"QUOTED-PRINTABLE\" 20 1) \"ALTERNATIVE\")\
              (\"IMAGE\" \"PNG\" (\"NAME\" \"logo.png\") NIL NIL \"BASE64\" 800 NIL) \"MIXED\"))\r\n",
        );
        assert_eq!(
            layout
                .text
                .iter()
                .map(MessagePart::section)
                .collect::<Vec<_>>(),
            vec!["1.1", "1.2"]
        );
        assert_eq!(layout.text[1].mime_type, "text/html");
        assert_eq!(layout.text[1].encoding, "quoted-printable");
        assert_eq!(
            layout
                .attachments
                .iter()
                .map(MessagePart::section)
                .collect::<Vec<_>>(),
            vec!["2"]
        );
    }

    #[test]
    fn a_forwarded_message_still_contributes_its_text() {
        // message/rfc822 with no filename is a mail forwarded inline, and its
        // body sits one level below the part that carries it.
        let layout = layout_of(
            b"* 1 FETCH (BODYSTRUCTURE ((\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 40 2)\
              (\"MESSAGE\" \"RFC822\" NIL NIL NIL \"7BIT\" 500 \
              (\"Wed, 17 Jul 1996 02:23:25 -0700\" \"Fwd\" ((\"A\" NIL \"a\" \"example.test\")) \
              ((\"A\" NIL \"a\" \"example.test\")) ((\"A\" NIL \"a\" \"example.test\")) \
              ((NIL NIL \"b\" \"example.test\")) NIL NIL NIL \"<fwd@example.test>\") \
              (\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 300 8) 12) \"MIXED\"))\r\n",
        );
        assert_eq!(
            layout
                .text
                .iter()
                .map(MessagePart::section)
                .collect::<Vec<_>>(),
            vec!["1", "2.1"]
        );
        assert!(layout.attachments.is_empty());
    }

    #[test]
    fn a_text_part_that_names_a_file_is_an_attachment() {
        let layout = layout_of(
            b"* 1 FETCH (BODYSTRUCTURE ((\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 40 2)\
              (\"TEXT\" \"CSV\" (\"NAME\" \"report.csv\") NIL NIL \"BASE64\" 120 3) \"MIXED\"))\r\n",
        );
        assert_eq!(layout.text.len(), 1);
        assert_eq!(
            layout.attachments[0].filename.as_deref(),
            Some("report.csv")
        );
    }

    #[test]
    fn related_parts_are_body_and_inline_images_are_left_on_the_server() {
        // multipart/related: the HTML plus the images it refers to. Only the
        // HTML is worth bytes during a sync.
        let layout = layout_of(
            b"* 1 FETCH (BODYSTRUCTURE ((\"TEXT\" \"HTML\" (\"CHARSET\" \"utf-8\") NIL NIL \"BASE64\" 900 9)\
              (\"IMAGE\" \"PNG\" (\"NAME\" \"header.png\") \"<logo@example.test>\" NIL \"BASE64\" 40000 NIL) \
              \"RELATED\"))\r\n",
        );
        assert_eq!(
            layout
                .text
                .iter()
                .map(MessagePart::section)
                .collect::<Vec<_>>(),
            vec!["1"]
        );
        assert_eq!(layout.attachments.len(), 1);
        assert_eq!(layout.attachments[0].decoded_size(), 30_000);
    }

    #[test]
    fn a_message_nested_two_levels_deep_still_numbers_its_parts_correctly() {
        // An alternative inside a forwarded message inside a mixed part.
        let layout = layout_of(
            b"* 1 FETCH (BODYSTRUCTURE ((\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 10 1)\
              (\"MESSAGE\" \"RFC822\" NIL NIL NIL \"7BIT\" 900 \
              (\"Wed, 17 Jul 1996 02:23:25 -0700\" \"Fwd\" ((\"A\" NIL \"a\" \"example.test\")) \
              ((\"A\" NIL \"a\" \"example.test\")) ((\"A\" NIL \"a\" \"example.test\")) \
              ((NIL NIL \"b\" \"example.test\")) NIL NIL NIL \"<fwd@example.test>\") \
              ((\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 100 4)\
              (\"TEXT\" \"HTML\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 200 6) \"ALTERNATIVE\") 20) \
              \"MIXED\"))\r\n",
        );
        assert_eq!(
            layout
                .text
                .iter()
                .map(MessagePart::section)
                .collect::<Vec<_>>(),
            vec!["1", "2.1", "2.2"]
        );
    }

    #[test]
    fn a_part_without_a_filename_is_not_turned_into_an_attachment() {
        // No NAME, no disposition: this is decoration the cache has never shown,
        // and inventing a nameless attachment row for it would be worse.
        let layout = layout_of(
            b"* 1 FETCH (BODYSTRUCTURE ((\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 10 1)\
              (\"APPLICATION\" \"OCTET-STREAM\" NIL NIL NIL \"BASE64\" 100 NIL) \"MIXED\"))\r\n",
        );
        assert_eq!(layout.text.len(), 1);
        assert!(layout.attachments.is_empty());
    }

    #[test]
    fn an_attachment_marked_by_disposition_alone_is_still_listed() {
        let layout = layout_of(
            b"* 1 FETCH (BODYSTRUCTURE ((\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 10 1)\
              (\"APPLICATION\" \"OCTET-STREAM\" NIL NIL NIL \"BASE64\" 100 NIL \
              (\"attachment\" NIL) NIL NIL) \"MIXED\"))\r\n",
        );
        assert_eq!(layout.attachments.len(), 1);
        // Nothing named it, so the row falls back to a generic name later.
        assert_eq!(layout.attachments[0].filename, None);
    }

    #[test]
    fn an_encoded_filename_reaches_the_cache_readable() {
        let layout = layout_of(
            b"* 1 FETCH (BODYSTRUCTURE ((\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 10 1)\
              (\"APPLICATION\" \"PDF\" NIL NIL NIL \"BASE64\" 100 NIL \
              (\"attachment\" (\"FILENAME\" \"=?UTF-8?B?ZmF0dXJhLcOnYWzEscWfbWEucGRm?=\")) NIL NIL) \
              \"MIXED\"))\r\n",
        );
        assert_eq!(
            layout.attachments[0].filename.as_deref(),
            Some("fatura-çalışma.pdf")
        );
    }

    #[test]
    fn a_filename_split_across_rfc_2231_continuations_is_reassembled() {
        // A long or non-ASCII filename arrives as FILENAME*0*, FILENAME*1*, ...
        // rather than one FILENAME parameter, and each segment needs the
        // percent-decoding and charset that only the *0 segment declares.
        let layout = layout_of(
            b"* 1 FETCH (BODYSTRUCTURE ((\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 10 1)\
              (\"APPLICATION\" \"PDF\" NIL NIL NIL \"BASE64\" 100 NIL \
              (\"attachment\" (\"FILENAME*0*\" \"UTF-8''fatura-%C3%A7al%C4%B1%C5%9F\" \
              \"FILENAME*1*\" \"ma-uzun-bir-isim.pdf\")) NIL NIL) \"MIXED\"))\r\n",
        );
        assert_eq!(
            layout.attachments[0].filename.as_deref(),
            Some("fatura-çalışma-uzun-bir-isim.pdf")
        );
    }

    #[test]
    fn a_plain_filename_is_not_treated_as_a_continuation() {
        // Guards the fallback path: a single FILENAME parameter must not be
        // routed through the continuation reconstruction at all.
        let layout = layout_of(
            b"* 1 FETCH (BODYSTRUCTURE ((\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 10 1)\
              (\"APPLICATION\" \"PDF\" (\"NAME\" \"plain.pdf\") NIL NIL \"BASE64\" 100 NIL) \"MIXED\"))\r\n",
        );
        assert_eq!(layout.attachments[0].filename.as_deref(), Some("plain.pdf"));
    }

    #[test]
    fn an_rfc2231_continued_turkish_filename_is_reassembled() {
        let layout = layout_of(
            b"* 1 FETCH (BODYSTRUCTURE ((\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\") NIL NIL \"7BIT\" 10 1)\
              (\"APPLICATION\" \"PDF\" NIL NIL NIL \"BASE64\" 100 NIL \
              (\"attachment\" (\"FILENAME*0*\" \"utf-8''%C3%87ok%20uzun%20\" \
              \"FILENAME*1*\" \"dosya%20ad%C4%B1.pdf\")) NIL NIL) \
              \"MIXED\"))\r\n",
        );
        assert_eq!(
            layout.attachments[0].filename.as_deref(),
            Some("Çok uzun dosya adı.pdf")
        );
    }

    #[test]
    fn a_message_with_no_body_at_all_produces_no_download() {
        // A bare attachment with nothing to read: there is no text to fetch, and
        // that is a complete answer rather than a missing one.
        let layout = layout_of(
            b"* 1 FETCH (BODYSTRUCTURE (\"APPLICATION\" \"PDF\" (\"NAME\" \"only.pdf\") NIL NIL \"BASE64\" 40 NIL))\r\n",
        );
        assert!(layout.text.is_empty());
        assert_eq!(layout.attachments[0].section(), "1");
    }

    #[test]
    fn an_empty_text_part_decodes_to_an_empty_body() {
        let part = MessagePart {
            path: vec![1],
            mime_type: "text/plain".to_string(),
            encoding: "7bit".to_string(),
            charset: Some("utf-8".to_string()),
            filename: None,
            octets: 0,
        };
        assert_eq!(decode_text_part(&part, b"").as_deref(), Some(""));
    }

    #[test]
    fn a_downloaded_part_is_decoded_with_its_own_headers() {
        let part = MessagePart {
            path: vec![1],
            mime_type: "text/plain".to_string(),
            encoding: "quoted-printable".to_string(),
            charset: Some("utf-8".to_string()),
            filename: None,
            octets: 0,
        };
        assert_eq!(
            decode_text_part(&part, b"Caf=C3=A9 a=\r\nnd more").as_deref(),
            Some("Café and more")
        );
    }

    #[test]
    fn two_roles_on_the_same_account_watch_independently() {
        let account = "watcher-test-account@example.test";
        let make_watcher = || {
            Arc::new(ImapWatcher {
                stop: AtomicBool::new(false),
                idling: AtomicBool::new(true),
            })
        };
        {
            let mut watchers = imap_watchers().lock().unwrap();
            watchers.insert((account.to_string(), "inbox".to_string()), make_watcher());
            watchers.insert(
                (account.to_string(), "custom:Work".to_string()),
                make_watcher(),
            );
        }

        assert!(mailbox_idle_active(account, "inbox"));
        assert!(mailbox_idle_active(account, "custom:Work"));
        // A role never registered for this account is not active.
        assert!(!mailbox_idle_active(account, "sent"));

        stop_imap_watcher(account, "inbox");
        assert!(!mailbox_idle_active(account, "inbox"));
        // Stopping one role's watcher must not touch the other.
        assert!(mailbox_idle_active(account, "custom:Work"));

        stop_imap_watcher(account, "custom:Work");
        assert!(!mailbox_idle_active(account, "custom:Work"));
    }
}

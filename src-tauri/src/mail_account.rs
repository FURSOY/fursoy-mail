use base64::Engine;
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
use std::collections::{BTreeMap, HashSet};
use std::net::TcpStream;
use std::time::Duration;
use tauri::AppHandle;

const IO_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_HOST_LEN: usize = 253;
const MAX_USERNAME_LEN: usize = 320;

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

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImapIdleOutcome {
    Changed,
    Unsupported,
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
    use imap_rs::credentials::Password;

    let session = match input.imap_security {
        MailSecurity::Tls => imap_rs::connect_tls(&input.imap_host, input.imap_port)
            .await
            .map_err(|_| "mail_account_tls_failed".to_string())?,
        MailSecurity::Starttls => imap_rs::connect_starttls(&input.imap_host, input.imap_port)
            .await
            .map_err(|_| "mail_account_starttls_failed".to_string())?,
    };
    let authenticated = session
        .login(&input.username, Password::new(&input.password))
        .await
        .map_err(|_| "mail_account_auth_failed".to_string())?;
    let _inbox = authenticated
        .select("INBOX")
        .await
        .map_err(|_| "mail_account_imap_failed".to_string())?;
    Ok(1)
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
        .map_err(|_| "mail_account_auth_failed".to_string())
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
        .map_err(|_| "mail_account_auth_failed".to_string())
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

fn discover_imap_mailboxes(session: &mut OAuthImapSession) -> Result<Vec<(String, String)>, String> {
    let listed = session
        .list(None, Some("*"))
        .map_err(|_| "mail_account_imap_failed".to_string())?;
    let mut roles = BTreeMap::<String, String>::new();
    for mailbox in listed.iter() {
        let mut special_role = None;
        for attribute in mailbox.attributes() {
            if let imap::types::NameAttribute::Custom(value) = attribute {
                special_role = match value.to_ascii_lowercase().as_str() {
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
        let role = special_role.or_else(|| fallback_mailbox_role(mailbox.name()));
        if let Some(role) = role {
            roles
                .entry(role.to_string())
                .or_insert_with(|| mailbox.name().to_string());
        }
    }
    roles
        .entry("inbox".to_string())
        .or_insert_with(|| "INBOX".to_string());
    Ok(roles.into_iter().collect())
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
    let oauth_token = if crate::db::load_tokens(account_id).is_some() {
        let token = match crate::db::load_account_access_token(account_id) {
            Ok(token) => token,
            Err(_) => {
                crate::mail_oauth::refresh_mail_oauth_token(app.clone(), account_id).await?;
                crate::db::load_account_access_token(account_id)?
            }
        };
        Some(token)
    } else {
        None
    };
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
    let _ = sync_imap_account(app, account_id).await;
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

async fn account_oauth_token(
    app: &AppHandle,
    account_id: &str,
) -> Result<Option<String>, String> {
    if crate::db::load_tokens(account_id).is_none() {
        return Ok(None);
    }
    match crate::db::load_account_access_token(account_id) {
        Ok(token) => Ok(Some(token)),
        Err(_) => {
            crate::mail_oauth::refresh_mail_oauth_token(app.clone(), account_id).await?;
            crate::db::load_account_access_token(account_id).map(Some)
        }
    }
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

fn collect_attachments(
    parsed: &mailparse::ParsedMail<'_>,
    account_id: &str,
    email_id: &str,
) -> Vec<crate::db::Attachment> {
    parsed
        .parts()
        .skip(1)
        .enumerate()
        .filter_map(|(index, part)| {
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
            let is_attachment = disposition.disposition == mailparse::DispositionType::Attachment
                || !filename.is_empty();
            if !is_attachment {
                return None;
            }
            let bytes = part.get_body_raw().ok()?;
            Some(crate::db::Attachment {
                id: format!("{email_id}:part:{index}"),
                email_id: email_id.to_string(),
                account_id: account_id.to_string(),
                filename: if filename.is_empty() {
                    "attachment".to_string()
                } else {
                    filename
                },
                mime_type: part.ctype.mimetype.clone(),
                size: bytes.len() as i64,
                attachment_id: None,
                data: Some(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)),
            })
        })
        .collect()
}

fn parsed_mail_to_email(
    account_id: &str,
    label: &str,
    uid: u32,
    unread: bool,
    starred: bool,
    raw: &[u8],
) -> Result<(crate::db::Email, Vec<crate::db::Attachment>), String> {
    let parsed =
        mailparse::parse_mail(raw).map_err(|_| "mail_account_message_parse_failed".to_string())?;
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
    let plain_body = find_body(&parsed, "text/plain").unwrap_or_default();
    let body_html =
        find_body(&parsed, "text/html").unwrap_or_else(|| escape_plain_text(&plain_body));
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

    let attachments = collect_attachments(&parsed, account_id, &id);
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

pub async fn sync_imap_account(app: &AppHandle, account_id: &str) -> Result<(), String> {
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
        sync_imap_account_blocking(&app, &account_id, &input, access_token.as_deref())
    })
    .await
    .map_err(|_| "mail_account_test_interrupted".to_string())?
}

fn sync_imap_account_blocking(
    app: &AppHandle,
    account_id: &str,
    input: &ImapAccountInput,
    access_token: Option<&str>,
) -> Result<(), String> {
    let mut session = connect_sync_imap(input, access_token)?;
    let gmail_account = input.imap_host.eq_ignore_ascii_case("imap.gmail.com");
    let mailboxes = discover_imap_mailboxes(&mut session).map_err(|error| {
        eprintln!("[IMAP_SYNC] mailbox discovery failed");
        error
    })?;
    let gmail_has_all = gmail_account && mailboxes.iter().any(|(role, _)| role == "all");
    crate::db::replace_imap_mailboxes(app, account_id, &mailboxes)?;
    let generation = crate::db::get_account_cache_generation(app, account_id)?;
    for (role, mailbox) in &mailboxes {
        if gmail_has_all && role == "archive" {
            continue;
        }
        let label = match role.as_str() {
            "inbox" => "inbox",
            "sent" => "sent",
            "trash" => "trash",
            "junk" => "spam",
            "archive" => "archive",
            "all" if gmail_account => "archive",
            _ => continue,
        };
        if session.select(mailbox).is_err() {
            eprintln!("[IMAP_SYNC] select failed for role {role}");
            if role == "inbox" {
                return Err("mail_account_imap_failed".to_string());
            }
            continue;
        }
        let search = if role == "all" && gmail_account {
            "X-GM-RAW \"-in:inbox -in:sent -in:drafts -in:trash -in:spam\""
        } else {
            "ALL"
        };
        let mut all_uids = match session.uid_search(search) {
            Ok(uids) => uids.into_iter().collect::<Vec<_>>(),
            Err(_) if role == "inbox" => {
                eprintln!("[IMAP_SYNC] UID search failed for inbox");
                return Err("mail_account_imap_failed".to_string())
            }
            Err(_) => {
                eprintln!("[IMAP_SYNC] UID search failed for role {role}");
                continue;
            }
        };
        all_uids.sort_unstable_by(|left, right| right.cmp(left));
        let existing_ids = crate::db::get_email_ids_for_label(app, account_id, label)?
            .into_iter()
            .collect::<HashSet<_>>();
        if all_uids.is_empty() {
            if !existing_ids.is_empty() {
                crate::db::delete_emails_by_ids(
                    app,
                    account_id,
                    generation,
                    &existing_ids.into_iter().collect::<Vec<_>>(),
                )?;
            }
            continue;
        }
        let remote_ids = all_uids
            .iter()
            .map(|uid| format!("imap:{label}:{uid}"))
            .collect::<HashSet<_>>();
        for uid_chunk in all_uids.chunks(100) {
            let missing_uids = uid_chunk
                .iter()
                .copied()
                .filter(|uid| !existing_ids.contains(&format!("imap:{label}:{uid}")))
                .collect::<Vec<_>>();
            if missing_uids.is_empty() {
                continue;
            }
            let uid_set = missing_uids
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let fetched = match session.uid_fetch(&uid_set, "(UID FLAGS BODY.PEEK[])") {
                Ok(fetched) => fetched,
                Err(_) if role == "inbox" => {
                    eprintln!("[IMAP_SYNC] UID fetch failed for inbox");
                    return Err("mail_account_imap_failed".to_string())
                }
                Err(_) => {
                    eprintln!("[IMAP_SYNC] UID fetch failed for role {role}");
                    break;
                }
            };
            let mut emails = Vec::with_capacity(fetched.len());
            let mut attachments = Vec::new();
            for message in fetched.iter() {
                let Some(uid) = message.uid else { continue };
                let Some(body) = message.body() else { continue };
                let unread = !message
                    .flags()
                    .iter()
                    .any(|flag| matches!(flag, imap::types::Flag::Seen));
                let starred = message
                    .flags()
                    .iter()
                    .any(|flag| matches!(flag, imap::types::Flag::Flagged));
                let Ok((email, mut message_attachments)) =
                    parsed_mail_to_email(account_id, label, uid, unread, starred, body)
                else {
                    continue;
                };
                emails.push(email);
                attachments.append(&mut message_attachments);
            }
            if !emails.is_empty() {
                crate::db::upsert_sync_mail_batch(
                    app,
                    account_id,
                    generation,
                    None,
                    emails,
                    attachments,
                )
                .map_err(|_| {
                    eprintln!("[IMAP_SYNC] cache write failed for role {role}");
                    "mail_account_cache_failed".to_string()
                })?;
            }
        }
        let stale_ids = existing_ids
            .difference(&remote_ids)
            .cloned()
            .collect::<Vec<_>>();
        if !stale_ids.is_empty() {
            crate::db::delete_emails_by_ids(app, account_id, generation, &stale_ids)?;
        }
    }
    session.logout().ok();
    Ok(())
}

fn imap_remote_ref(message_id: &str) -> Option<(&str, u32)> {
    let value = message_id.strip_prefix("imap:")?;
    let (label, uid) = value.rsplit_once(':')?;
    Some((label, uid.parse().ok()?))
}

fn mailbox_role_for_label(label: &str) -> &str {
    if label == "spam" { "junk" } else { label }
}

fn gmail_archive_store_query() -> &'static str {
    "-X-GM-LABELS (\\Inbox)"
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

fn draft_attachments(parsed: &mailparse::ParsedMail<'_>) -> Vec<crate::gmail::AttachmentPayload> {
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
            Some(crate::gmail::AttachmentPayload {
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

fn draft_content_from_raw(uid: u32, raw: &[u8]) -> Result<crate::gmail::DraftContent, String> {
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
    Ok(crate::gmail::DraftContent {
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
) -> Result<crate::gmail::DraftPage, String> {
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
            let uid_set = uids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
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
                drafts.push(crate::gmail::DraftSummary {
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
        Ok(crate::gmail::DraftPage {
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
) -> Result<crate::gmail::DraftContent, String> {
    let uid = imap_draft_uid(draft_id)
        .ok_or_else(|| "mail_account_message_not_found".to_string())?;
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
) -> Result<crate::gmail::SavedDraft, String> {
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
            .append_with_flags(
                &mailbox,
                raw_email.as_bytes(),
                &[imap::types::Flag::Draft],
            )
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
        Ok(crate::gmail::SavedDraft {
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
    let uid = imap_draft_uid(draft_id)
        .ok_or_else(|| "mail_account_message_not_found".to_string())?;
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
    let uid = imap_draft_uid(draft_id)
        .ok_or_else(|| "mail_account_message_not_found".to_string())?;
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
        let raw = String::from_utf8(raw)
            .map_err(|_| "mail_account_message_parse_failed".to_string())?;
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
        mappings
            .get("archive")
            .or_else(|| mappings.get("all"))
    } else {
        mappings.get(target_role)
    }
    .cloned()
    .ok_or_else(|| "mail_account_label_not_supported".to_string())?;
    let mut by_source = BTreeMap::<String, Vec<u32>>::new();
    for message_id in &message_ids {
        let Some((label, uid)) = imap_remote_ref(message_id) else {
            continue;
        };
        let source_role = mailbox_role_for_label(label);
        if source_role == target_role || (target_role == "archive" && source_role == "all") {
            continue;
        }
        if mappings.contains_key(source_role) {
            by_source.entry(source_role.to_string()).or_default().push(uid);
        }
    }
    if by_source.is_empty() {
        return Err("mail_account_message_not_found".to_string());
    }
    let input = stored_account_input(app, account_id)?;
    let gmail_account = input.imap_host.eq_ignore_ascii_case("imap.gmail.com");
    let target_is_archive = target_role == "archive";
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
            let uid_set = uids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
            if gmail_account && target_is_archive {
                session
                    .uid_store(&uid_set, gmail_archive_store_query())
                    .map_err(|_| "mail_account_imap_failed".to_string())?;
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
    let _ = sync_imap_account(app, account_id).await;
    Ok(())
}

pub async fn set_imap_message_flag(
    app: &AppHandle,
    account_id: &str,
    target_id: &str,
    thread_target: bool,
    flag: imap_rs::client::Flag,
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
    let mut by_source = BTreeMap::<String, Vec<u32>>::new();
    for message_id in &message_ids {
        let Some((label, uid)) = imap_remote_ref(message_id) else {
            continue;
        };
        let source_role = mailbox_role_for_label(label);
        if mappings.contains_key(source_role) {
            by_source.entry(source_role.to_string()).or_default().push(uid);
        }
    }
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
            let uid_set = uids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
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

#[tauri::command]
pub async fn sync_imap_emails(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    sync_imap_account(&app, &account_id).await
}

#[tauri::command]
pub async fn wait_for_imap_change(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
) -> Result<ImapIdleOutcome, String> {
    crate::require_command_window(&window, &["main"])?;
    let input = stored_account_input(&app, &account_id)?;
    let access_token = account_oauth_token(&app, &account_id).await?;
    let inbox = crate::db::get_imap_mailboxes(&app, &account_id)?
        .into_iter()
        .find_map(|(role, mailbox)| (role == "inbox").then_some(mailbox))
        .unwrap_or_else(|| "INBOX".to_string());

    tauri::async_runtime::spawn_blocking(move || {
        let mut session = connect_sync_imap(&input, access_token.as_deref())?;
        let supports_idle = session
            .capabilities()
            .map_err(|_| "mail_account_imap_failed".to_string())?
            .has_str("IDLE");
        if !supports_idle {
            session.logout().ok();
            return Ok(ImapIdleOutcome::Unsupported);
        }
        session
            .select(&inbox)
            .map_err(|_| "mail_account_imap_failed".to_string())?;
        session
            .idle()
            .map_err(|_| "mail_account_imap_failed".to_string())?
            .wait_keepalive()
            .map_err(|_| "mail_account_imap_failed".to_string())?;
        session.logout().ok();
        Ok(ImapIdleOutcome::Changed)
    })
    .await
    .map_err(|_| "mail_account_test_interrupted".to_string())?
}

#[cfg(test)]
mod tests {
    use super::{
        fallback_mailbox_role, gmail_archive_store_query, imap_remote_ref,
        mailbox_role_for_label, strip_bcc_header, ImapAccountInput, MailSecurity,
    };

    #[test]
    fn gmail_archive_removes_the_inbox_label() {
        assert_eq!(gmail_archive_store_query(), "-X-GM-LABELS (\\Inbox)");
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
    fn parses_imap_remote_ids_without_confusing_mailbox_roles() {
        assert_eq!(imap_remote_ref("imap:inbox:42"), Some(("inbox", 42)));
        assert_eq!(imap_remote_ref("imap:spam:7"), Some(("spam", 7)));
        assert_eq!(imap_remote_ref("gmail-message-id"), None);
        assert_eq!(mailbox_role_for_label("spam"), "junk");
        assert_eq!(mailbox_role_for_label("sent"), "sent");
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
        let raw = b"From: Alice <alice@example.test>\r\nTo: Bob <bob@example.test>\r\nSubject: Hello\r\nDate: Tue, 30 Jul 2024 10:00:00 +0000\r\nMessage-ID: <child@example.test>\r\nIn-Reply-To: <root@example.test>\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHello from IMAP";
        let (email, attachments) =
            super::parsed_mail_to_email("bob@example.test", "inbox", 42, true, false, raw)
                .expect("parse message");
        assert_eq!(email.id, "imap:inbox:42");
        assert_eq!(email.thread_id, "<root@example.test>");
        assert_eq!(email.subject, "Hello");
        assert!(email.body_html.contains("Hello from IMAP"));
        assert!(email.unread);
        assert!(attachments.is_empty());
    }
}

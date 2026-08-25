use crate::db::{get_all_mailbox_sync_states, get_mailbox_sync_state, has_pending_mailbox_pages};
use base64::Engine;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct AttachmentPayload {
    pub filename: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    pub data: String, // base64-encoded file content
}

#[derive(Serialize)]
pub struct MailboxDownloadStatus {
    pub running: bool,
    pub pending: bool,
    pub state: String,
    #[serde(rename = "retryAfter")]
    pub retry_after: Option<i64>,
}

#[tauri::command]
pub fn get_mailbox_download_status(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: Option<String>,
) -> Result<MailboxDownloadStatus, String> {
    crate::require_command_window(&window, &["main"])?;
    let state = app.state::<crate::SyncState>();
    let workers = state
        .workers
        .lock()
        .map_err(|_| "Sync worker lock poisoned")?;
    let running = workers.is_backfilling(account_id.as_deref());
    drop(workers);
    let pending = has_pending_mailbox_pages(&app, account_id.as_deref())?;
    let persisted = match account_id.as_deref() {
        Some(account_id) => get_mailbox_sync_state(&app, account_id)?,
        None => get_all_mailbox_sync_states(&app)?
            .into_iter()
            .max_by_key(|state| match state.status.as_str() {
                "relogin_required" => 4,
                "rate_limited" => 3,
                "error" | "paused" => 2,
                "waiting" => 1,
                _ => 0,
            }),
    };
    let retry_after = persisted.as_ref().and_then(|state| state.retry_after);
    let state = if running {
        "running".to_string()
    } else if let Some(state) = persisted {
        state.status
    } else if pending {
        "waiting".to_string()
    } else {
        "completed".to_string()
    };
    Ok(MailboxDownloadStatus {
        running,
        pending,
        state,
        retry_after,
    })
}

#[tauri::command]
pub async fn sync_emails(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    force: Option<bool>,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    crate::mail_account::sync_imap_account(&app, &account_id, force.unwrap_or(false)).await
}

#[tauri::command]
pub async fn archive_email(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    thread_id: String,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    // The reader toolbar represents the whole conversation.
    crate::mail_account::move_imap_thread(&app, &account_id, &thread_id, "archive").await
}

#[tauri::command]
pub async fn report_spam(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    thread_id: String,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    // Spam classification applies to the whole conversation.
    crate::mail_account::move_imap_thread(&app, &account_id, &thread_id, "junk").await
}

#[tauri::command]
pub async fn trash_email(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    thread_id: String,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    // Trash the whole conversation, matching the reader toolbar scope.
    crate::mail_account::move_imap_thread(&app, &account_id, &thread_id, "trash").await
}

/// Moves a conversation into one of the account's own folders. The fixed roles
/// have their own commands, with their own server semantics — archiving on
/// Gmail is a label change, not a move — so this one deliberately only accepts
/// a user folder.
#[tauri::command]
pub async fn move_to_mailbox(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    thread_id: String,
    mailbox_role: String,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    if !mailbox_role.starts_with("custom:") {
        return Err("mail_account_label_not_supported".to_string());
    }
    crate::mail_account::move_imap_thread(&app, &account_id, &thread_id, &mailbox_role).await
}

#[tauri::command]
pub async fn move_to_inbox(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    thread_id: String,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    // Restore the whole conversation, matching the reader toolbar scope.
    crate::mail_account::move_imap_thread(&app, &account_id, &thread_id, "inbox").await
}

const MAX_OUTBOUND_ATTACHMENT_BYTES: usize = 20 * 1024 * 1024;
const MAX_RECIPIENT_HEADER_BYTES: usize = 8 * 1024;
const MAX_SUBJECT_BYTES: usize = 4 * 1024;

fn validate_header_value(name: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    if value.len() > max_bytes {
        return Err(format!("{name} is too long"));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{name} contains unsupported control characters"));
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftSummary {
    pub(crate) id: String,
    pub(crate) message_id: String,
    pub(crate) rfc_message_id: String,
    pub(crate) thread_id: String,
    pub(crate) in_reply_to: String,
    pub(crate) references: String,
    pub(crate) to: String,
    pub(crate) cc: String,
    pub(crate) bcc: String,
    pub(crate) subject: String,
    pub(crate) snippet: String,
    pub(crate) updated_at: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftPage {
    pub(crate) drafts: Vec<DraftSummary>,
    pub(crate) next_page_token: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftContent {
    pub(crate) id: String,
    pub(crate) message_id: String,
    pub(crate) rfc_message_id: String,
    pub(crate) thread_id: String,
    pub(crate) in_reply_to: String,
    pub(crate) references: String,
    pub(crate) to: String,
    pub(crate) cc: String,
    pub(crate) bcc: String,
    pub(crate) subject: String,
    pub(crate) body: String,
    pub(crate) updated_at: i64,
    pub(crate) attachments: Vec<AttachmentPayload>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedDraft {
    pub(crate) id: String,
    pub(crate) message_id: String,
    pub(crate) verification_message_id: String,
    pub(crate) updated_at: i64,
}

fn validate_recipient_header(value: &str) -> Result<(), String> {
    validate_header_value("Recipient", value, MAX_RECIPIENT_HEADER_BYTES)?;
    if value.trim().is_empty() || !value.contains('@') {
        return Err("Recipient address is invalid".to_string());
    }
    Ok(())
}

fn validate_optional_recipient_header(value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Ok(());
    }
    validate_recipient_header(value)
}

fn validate_draft_recipient_header(name: &str, value: &str) -> Result<(), String> {
    validate_header_value(name, value, MAX_RECIPIENT_HEADER_BYTES)
}

fn build_reply_references(prior: &str, in_reply_to: &str) -> String {
    if prior.split_whitespace().any(|value| value == in_reply_to) {
        prior.trim().to_string()
    } else if prior.trim().is_empty() {
        in_reply_to.to_string()
    } else {
        format!("{} {}", prior.trim(), in_reply_to)
    }
}

fn is_mime_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
                )
        })
}

fn validate_mime_type(value: &str) -> Result<(), String> {
    let Some((top_level, subtype)) = value.split_once('/') else {
        return Err("Attachment MIME type is invalid".to_string());
    };
    if subtype.contains('/') || !is_mime_token(top_level) || !is_mime_token(subtype) {
        return Err("Attachment MIME type is invalid".to_string());
    }
    Ok(())
}

fn is_blocked_attachment_filename(filename: &str) -> bool {
    let extension = std::path::Path::new(filename)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "exe"
            | "bat"
            | "cmd"
            | "com"
            | "msi"
            | "scr"
            | "pif"
            | "vbs"
            | "vbe"
            | "js"
            | "jse"
            | "jar"
            | "wsf"
            | "wsh"
            | "ps1"
            | "reg"
            | "inf"
            | "lnk"
    )
}

/// RFC 2047 encodes header text so quotes and line breaks can never escape the
/// header value. Uses UTF-8 base64 encoded-word format: =?UTF-8?B?<base64>?=
fn mime_encode_header(value: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(value.as_bytes());
    format!("=?UTF-8?B?{}?=", encoded)
}

/// Base64-encodes a string body per RFC 2045 (wraps at 76 chars).
fn mime_body_base64(body: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(body.as_bytes());
    encoded
        .as_bytes()
        .chunks(76)
        .map(|c| std::str::from_utf8(c).unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\r\n")
}

/// Builds a RFC 2822 raw email. Without attachments: simple text/html.
/// With attachments: multipart/mixed with HTML part + attachment parts.
pub(crate) fn build_raw_mime(
    headers: &[(&str, String)],
    body: &str,
    attachments: &[AttachmentPayload],
) -> Result<String, String> {
    if attachments.len() > 100 {
        return Err("Too many attachments".to_string());
    }
    let mut lines = String::from("MIME-Version: 1.0\r\n");
    for (name, value) in headers {
        validate_header_value(name, value, MAX_RECIPIENT_HEADER_BYTES)?;
        lines.push_str(&format!("{}: {}\r\n", name, value));
    }

    if attachments.is_empty() {
        lines.push_str("Content-Type: text/html; charset=\"UTF-8\"\r\n");
        lines.push_str("Content-Transfer-Encoding: base64\r\n");
        lines.push_str("\r\n");
        lines.push_str(&mime_body_base64(body));
    } else {
        let boundary = "----=_NextPart_fursoymail_001";
        lines.push_str(&format!(
            "Content-Type: multipart/mixed; boundary=\"{}\"\r\n",
            boundary
        ));
        lines.push_str("\r\n");

        // HTML body part
        lines.push_str(&format!("--{}\r\n", boundary));
        lines.push_str("Content-Type: text/html; charset=\"UTF-8\"\r\n");
        lines.push_str("Content-Transfer-Encoding: base64\r\n");
        lines.push_str("\r\n");
        lines.push_str(&mime_body_base64(body));
        lines.push_str("\r\n");

        // Attachment parts
        let mut total_attachment_bytes = 0usize;
        for att in attachments {
            if att.filename.len() > 1_024
                || att.mime_type.len() > 255
                || att.data.len() > MAX_OUTBOUND_ATTACHMENT_BYTES.saturating_mul(2)
            {
                return Err("Attachment payload is too large".to_string());
            }
            validate_mime_type(&att.mime_type)?;
            let safe_filename = safe_attachment_filename(&att.filename);
            if is_blocked_attachment_filename(&safe_filename) {
                return Err("Blocked attachment file type".to_string());
            }
            let normalized_data: String = att
                .data
                .chars()
                .filter(|character| !character.is_ascii_whitespace())
                .collect();
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(normalized_data.as_bytes())
                .map_err(|_| "Attachment data is not valid base64".to_string())?;
            total_attachment_bytes = total_attachment_bytes.saturating_add(decoded.len());
            if total_attachment_bytes > MAX_OUTBOUND_ATTACHMENT_BYTES {
                return Err("Total attachment size cannot exceed 20 MB".to_string());
            }
            let encoded_data = base64::engine::general_purpose::STANDARD.encode(decoded);
            let encoded_name = mime_encode_header(&safe_filename);
            lines.push_str(&format!("--{}\r\n", boundary));
            lines.push_str(&format!(
                "Content-Type: {}; name=\"{}\"\r\n",
                att.mime_type, encoded_name
            ));
            lines.push_str("Content-Transfer-Encoding: base64\r\n");
            lines.push_str(&format!(
                "Content-Disposition: attachment; filename=\"{}\"\r\n",
                encoded_name
            ));
            lines.push_str("\r\n");
            // Wrap attachment data at 76 chars
            let wrapped = encoded_data
                .as_bytes()
                .chunks(76)
                .map(|c| std::str::from_utf8(c).unwrap_or(""))
                .collect::<Vec<_>>()
                .join("\r\n");
            lines.push_str(&wrapped);
            lines.push_str("\r\n");
        }

        lines.push_str(&format!("--{}--\r\n", boundary));
    }

    Ok(lines)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendOutcome {
    status: &'static str,
    message_id: String,
}

fn generate_outbound_message_id() -> String {
    let random = (0..4)
        .map(|_| format!("{:016x}", rand::random::<u64>()))
        .collect::<String>();
    format!("<fursoy-{random}@mail.invalid>")
}

fn validate_outbound_message_id(message_id: &str) -> Result<(), String> {
    let Some(inner) = message_id
        .strip_prefix("<fursoy-")
        .and_then(|value| value.strip_suffix("@mail.invalid>"))
    else {
        return Err("Invalid outbound message ID".to_string());
    };
    if inner.len() != 64 || !inner.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Invalid outbound message ID".to_string());
    }
    Ok(())
}

#[tauri::command]
pub async fn send_reply(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    account_id: String,
    to: String,
    cc: String,
    subject: String,
    body: String,
    thread_id: String,
    in_reply_to: String,
    references: String,
    attachments: Option<Vec<AttachmentPayload>>,
) -> Result<SendOutcome, String> {
    crate::require_command_window(&window, &["main"])?;
    let _ = thread_id;
    let atts = attachments.unwrap_or_default();
    validate_recipient_header(&to)?;
    validate_optional_recipient_header(&cc)?;
    validate_header_value("Subject", &subject, MAX_SUBJECT_BYTES)?;
    let outbound_message_id = generate_outbound_message_id();

    let clean_subject = subject
        .trim_start_matches("Re: ")
        .trim_start_matches("re: ");
    let mut headers = vec![
        ("From", account_id.clone()),
        ("To", to.clone()),
        (
            "Subject",
            format!("Re: {}", mime_encode_header(clean_subject)),
        ),
    ];
    if !cc.trim().is_empty() {
        headers.push(("Cc", cc.clone()));
    }
    if !in_reply_to.trim().is_empty() {
        validate_header_value("In-Reply-To", &in_reply_to, MAX_RECIPIENT_HEADER_BYTES)?;
        headers.push(("In-Reply-To", in_reply_to.clone()));
        let combined_references = build_reply_references(&references, &in_reply_to);
        validate_header_value(
            "References",
            &combined_references,
            MAX_RECIPIENT_HEADER_BYTES,
        )?;
        headers.push(("References", combined_references));
    }
    headers.push(("Message-ID", outbound_message_id.clone()));
    let raw_email = build_raw_mime(&headers, &body, &atts)?;

    crate::mail_account::send_smtp_raw(&app, &account_id, &to, &cc, "", raw_email).await?;
    Ok(SendOutcome {
        status: "sent",
        message_id: outbound_message_id,
    })
}

#[tauri::command]
pub async fn send_email(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    account_id: String,
    to: String,
    cc: String,
    bcc: String,
    subject: String,
    body: String,
    attachments: Option<Vec<AttachmentPayload>>,
) -> Result<SendOutcome, String> {
    crate::require_command_window(&window, &["main"])?;
    let atts = attachments.unwrap_or_default();
    validate_recipient_header(&to)?;
    validate_optional_recipient_header(&cc)?;
    validate_optional_recipient_header(&bcc)?;
    validate_header_value("Subject", &subject, MAX_SUBJECT_BYTES)?;
    let outbound_message_id = generate_outbound_message_id();

    // Bcc stays out of the transmitted headers; the SMTP envelope carries it.
    let mut headers = vec![("From", account_id.clone()), ("To", to.clone())];
    if !cc.trim().is_empty() {
        headers.push(("Cc", cc.clone()));
    }
    headers.push(("Subject", mime_encode_header(&subject)));
    headers.push(("Message-ID", outbound_message_id.clone()));
    let raw_email = build_raw_mime(&headers, &body, &atts)?;

    crate::mail_account::send_smtp_raw(&app, &account_id, &to, &cc, &bcc, raw_email).await?;
    Ok(SendOutcome {
        status: "sent",
        message_id: outbound_message_id,
    })
}

#[tauri::command]
pub async fn list_drafts(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    account_id: String,
    page_token: Option<String>,
) -> Result<DraftPage, String> {
    crate::require_command_window(&window, &["main"])?;
    let _ = page_token;
    crate::mail_account::list_imap_drafts(&app, &account_id).await
}

#[tauri::command]
pub async fn get_draft(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    account_id: String,
    draft_id: String,
) -> Result<DraftContent, String> {
    crate::require_command_window(&window, &["main"])?;
    crate::mail_account::get_imap_draft(&app, &account_id, &draft_id).await
}

#[tauri::command]
pub async fn save_draft(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    account_id: String,
    draft_id: Option<String>,
    to: String,
    cc: String,
    bcc: String,
    subject: String,
    body: String,
    attachments: Option<Vec<AttachmentPayload>>,
    thread_id: Option<String>,
    in_reply_to: Option<String>,
    references: Option<String>,
) -> Result<SavedDraft, String> {
    crate::require_command_window(&window, &["main"])?;
    let _ = thread_id;
    // Drafts deliberately accept incomplete addresses. They are validated
    // strictly only when the user sends the message.
    validate_draft_recipient_header("To", &to)?;
    validate_draft_recipient_header("Cc", &cc)?;
    validate_draft_recipient_header("Bcc", &bcc)?;
    validate_header_value("Subject", &subject, MAX_SUBJECT_BYTES)?;
    let verification_message_id = generate_outbound_message_id();
    let mut headers = vec![("To", to)];
    if !cc.trim().is_empty() {
        headers.push(("Cc", cc));
    }
    if !bcc.trim().is_empty() {
        headers.push(("Bcc", bcc));
    }
    headers.push(("Subject", mime_encode_header(&subject)));
    if let Some(in_reply_to) = in_reply_to.filter(|value| !value.trim().is_empty()) {
        validate_header_value("In-Reply-To", &in_reply_to, MAX_RECIPIENT_HEADER_BYTES)?;
        headers.push(("In-Reply-To", in_reply_to.clone()));
        let prior = references.unwrap_or_default();
        let combined = build_reply_references(&prior, &in_reply_to);
        validate_header_value("References", &combined, MAX_RECIPIENT_HEADER_BYTES)?;
        headers.push(("References", combined));
    }
    headers.push(("Message-ID", verification_message_id.clone()));
    let raw_email = build_raw_mime(&headers, &body, &attachments.unwrap_or_default())?;
    crate::mail_account::save_imap_draft(
        &app,
        &account_id,
        draft_id.as_deref(),
        raw_email,
        verification_message_id,
    )
    .await
}

#[tauri::command]
pub async fn send_draft(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    account_id: String,
    draft_id: String,
    verification_message_id: String,
) -> Result<SendOutcome, String> {
    crate::require_command_window(&window, &["main"])?;
    validate_outbound_message_id(&verification_message_id)?;
    crate::mail_account::send_imap_draft(&app, &account_id, &draft_id).await?;
    Ok(SendOutcome {
        status: "sent",
        message_id: verification_message_id,
    })
}

#[tauri::command]
pub async fn delete_draft(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    account_id: String,
    draft_id: String,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    crate::mail_account::delete_imap_draft(&app, &account_id, &draft_id).await
}

#[tauri::command]
pub async fn mark_as_read(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    message_id: String,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    crate::mail_account::set_imap_message_flag(
        &app,
        &account_id,
        &message_id,
        false,
        imap::types::Flag::Seen,
        true,
    )
    .await?;
    crate::db::mark_email_as_read_local(&app, &message_id, &account_id)
}

#[tauri::command]
pub async fn mark_thread_as_read(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    thread_id: String,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    crate::mail_account::set_imap_message_flag(
        &app,
        &account_id,
        &thread_id,
        true,
        imap::types::Flag::Seen,
        true,
    )
    .await?;
    crate::db::mark_thread_as_read_local(&app, &thread_id, &account_id)
}

#[tauri::command]
pub async fn mark_as_unread(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    thread_id: String,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    crate::mail_account::set_imap_message_flag(
        &app,
        &account_id,
        &thread_id,
        true,
        imap::types::Flag::Seen,
        false,
    )
    .await?;
    crate::db::mark_thread_as_unread_local(&app, &thread_id, &account_id)
}

/// Reads one cached attachment. IMAP sync stores the payload alongside the
/// message, so there is no separate remote fetch.
async fn get_attachment_bytes(
    app: &tauri::AppHandle,
    email_id: &str,
    account_id: &str,
    attachment_db_id: &str,
) -> Result<(Vec<u8>, String, String), String> {
    let atts = crate::db::get_email_attachments_for_account(
        app.clone(),
        email_id.to_string(),
        account_id.to_string(),
    )
    .map_err(|e| e.to_string())?;
    let att = atts
        .into_iter()
        .find(|a| a.id == attachment_db_id)
        .ok_or_else(|| "Attachment not found".to_string())?;

    // A row without bytes is one the sync recorded but left on the server, and
    // its stored identifier is the section path that fetches it.
    let Some(b64) = att.data.clone().filter(|data| !data.is_empty()) else {
        let section = att
            .attachment_id
            .as_deref()
            .ok_or_else(|| "mail_account_attachment_data_missing".to_string())?;
        let bytes =
            crate::mail_account::fetch_imap_attachment(app, account_id, email_id, section).await?;
        return Ok((bytes, att.filename, att.mime_type));
    };

    let bytes = base64::engine::general_purpose::URL_SAFE
        .decode(b64.replace(['\n', '\r'], "").as_str())
        .map_err(|e| format!("Base64 decode error: {}", e))?;

    Ok((bytes, att.filename, att.mime_type))
}

fn safe_attachment_filename(filename: &str) -> String {
    let basename = std::path::Path::new(filename)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("attachment")
        .trim();

    let cleaned: String = basename
        .chars()
        .map(|ch| {
            if ch.is_control()
                || matches!(
                    ch,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' | '\u{202e}'
                )
            {
                '_'
            } else {
                ch
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches(|ch: char| ch == '.' || ch == ' ');
    let cleaned: String = cleaned.chars().take(120).collect();
    let cleaned = cleaned.trim_matches(|ch: char| ch == '.' || ch == ' ');

    if cleaned.is_empty() {
        return "attachment".to_string();
    }

    let stem = cleaned.split('.').next().unwrap_or("").to_ascii_uppercase();
    let is_reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));

    if is_reserved {
        let extension = std::path::Path::new(cleaned)
            .extension()
            .and_then(|extension| extension.to_str());
        match extension {
            Some(extension) if !extension.is_empty() => format!("{}_file.{}", stem, extension),
            _ => format!("{}_file", stem),
        }
    } else {
        cleaned.to_string()
    }
}

/// Saves attachment to Downloads folder and reveals it in Windows Explorer.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedAttachment {
    file_name: String,
    revealed: bool,
}

#[tauri::command]
pub async fn save_and_reveal_attachment(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    email_id: String,
    account_id: String,
    attachment_db_id: String,
) -> Result<SavedAttachment, String> {
    crate::require_command_window(&window, &["main"])?;
    let (bytes, filename, _mime) =
        get_attachment_bytes(&app, &email_id, &account_id, &attachment_db_id).await?;

    let downloads = app
        .path()
        .download_dir()
        .map_err(|e| format!("Cannot find Downloads folder: {}", e))?;

    // A sender controls attachment filenames. Keep the saved file inside Downloads
    // instead of trusting path segments or Windows device names from the message.
    let safe_filename = safe_attachment_filename(&filename);
    let mut dest = downloads.join(&safe_filename);
    if dest.parent() != Some(downloads.as_path()) {
        return Err("Unsafe attachment filename".to_string());
    }

    // Avoid overwriting existing files by appending a counter
    if dest.exists() {
        let stem = std::path::Path::new(&safe_filename)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("file");
        let ext = std::path::Path::new(&safe_filename)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let mut i = 2u32;
        loop {
            let candidate = if ext.is_empty() {
                format!("{} ({})", stem, i)
            } else {
                format!("{} ({}).{}", stem, i, ext)
            };
            dest = downloads.join(&candidate);
            if !dest.exists() {
                break;
            }
            i += 1;
        }
    }

    crate::safe_fs::atomic_write_new(&dest, &bytes).map_err(|e| format!("Write error: {}", e))?;

    // Reveal file selected in Windows Explorer
    let revealed = std::process::Command::new("explorer")
        .arg(format!("/select,{}", dest.display()))
        .spawn()
        .is_ok();

    Ok(SavedAttachment {
        file_name: dest
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&safe_filename)
            .to_string(),
        revealed,
    })
}

/// Returns raw base64 data — used for image thumbnail preview in the frontend.
#[tauri::command]
pub async fn fetch_attachment_data(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    email_id: String,
    account_id: String,
    attachment_db_id: String,
) -> Result<String, String> {
    crate::require_command_window(&window, &["main"])?;
    let (bytes, _filename, _mime) =
        get_attachment_bytes(&app, &email_id, &account_id, &attachment_db_id).await?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&bytes))
}

#[cfg(test)]
mod tests {
    use super::{
        build_raw_mime, build_reply_references, generate_outbound_message_id,
        safe_attachment_filename, validate_draft_recipient_header,
        validate_optional_recipient_header, validate_outbound_message_id,
        validate_recipient_header, AttachmentPayload,
    };

    #[test]
    fn drafts_allow_an_empty_recipient_but_validate_non_empty_values() {
        assert!(validate_optional_recipient_header("").is_ok());
        assert!(validate_optional_recipient_header("person@example.test").is_ok());
        assert!(validate_optional_recipient_header("not-an-address").is_err());
    }

    #[test]
    fn draft_recipient_fields_accept_incomplete_addresses_but_reject_header_injection() {
        assert!(validate_draft_recipient_header("To", "unfinished-address").is_ok());
        assert!(validate_draft_recipient_header("To", "person@example.test").is_ok());
        assert!(validate_draft_recipient_header(
            "To",
            "person@example.test\r\nBcc: attacker@example.test"
        )
        .is_err());
    }

    #[test]
    fn reply_reference_chain_appends_the_selected_message_once() {
        assert_eq!(
            build_reply_references("<root@example.test>", "<child@example.test>"),
            "<root@example.test> <child@example.test>"
        );
        assert_eq!(
            build_reply_references(
                "<root@example.test> <child@example.test>",
                "<child@example.test>"
            ),
            "<root@example.test> <child@example.test>"
        );
    }

    #[test]
    fn generated_outbound_message_ids_are_unique_and_strictly_validated() {
        let first = generate_outbound_message_id();
        let second = generate_outbound_message_id();

        assert_ne!(first, second);
        assert!(validate_outbound_message_id(&first).is_ok());
        assert!(validate_outbound_message_id(&second).is_ok());
        assert!(validate_outbound_message_id("<attacker@example.test>").is_err());
        assert!(validate_outbound_message_id(
            "<fursoy-0000000000000000000000000000000000000000000000000000000000000000@mail.invalid> extra"
        )
        .is_err());
    }

    #[test]
    fn attachment_filename_drops_path_components() {
        assert_eq!(
            safe_attachment_filename(r"..\..\Desktop\report.pdf"),
            "report.pdf"
        );
        assert_eq!(
            safe_attachment_filename(r"C:\Windows\System32\hosts"),
            "hosts"
        );
    }

    #[test]
    fn attachment_filename_replaces_windows_unsafe_characters() {
        assert_eq!(
            safe_attachment_filename("invoice:2026?.pdf"),
            "invoice_2026_.pdf"
        );
        assert_eq!(safe_attachment_filename("NUL.txt"), "NUL_file.txt");
        assert_eq!(safe_attachment_filename("..."), "attachment");
    }

    #[test]
    fn outbound_headers_reject_line_break_injection() {
        assert!(
            validate_recipient_header("alice@example.test\r\nBcc: hidden@example.test").is_err()
        );
    }

    #[test]
    fn outbound_mime_keeps_cc_and_bcc_headers() {
        let raw = build_raw_mime(
            &[
                ("To", "to@example.test".to_string()),
                ("Cc", "copy@example.test".to_string()),
                ("Bcc", "hidden@example.test".to_string()),
                ("Subject", "Status".to_string()),
            ],
            "<p>Hello</p>",
            &[],
        )
        .expect("build MIME with optional recipients");

        assert!(raw.contains("\r\nCc: copy@example.test\r\n"));
        assert!(raw.contains("\r\nBcc: hidden@example.test\r\n"));
    }

    #[test]
    fn outbound_attachments_are_revalidated_in_rust() {
        let invalid_mime = AttachmentPayload {
            filename: "report.pdf".to_string(),
            mime_type: "application/pdf\r\nBcc: hidden@example.test".to_string(),
            data: "aGVsbG8=".to_string(),
        };
        assert!(build_raw_mime(
            &[("To", "alice@example.test".to_string())],
            "<p>Hello</p>",
            &[invalid_mime],
        )
        .is_err());

        let blocked_file = AttachmentPayload {
            filename: "payload.js".to_string(),
            mime_type: "application/octet-stream".to_string(),
            data: "aGVsbG8=".to_string(),
        };
        assert!(build_raw_mime(
            &[("To", "alice@example.test".to_string())],
            "<p>Hello</p>",
            &[blocked_file],
        )
        .is_err());
    }
}

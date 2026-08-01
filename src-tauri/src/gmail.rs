use crate::db::{
    complete_full_sync, delete_emails_by_ids, delete_gmail_label_local, finalize_full_sync,
    get_account_cache_generation, get_active_full_sync, get_all_mailbox_sync_states,
    get_history_id, get_mailbox_cursor_state, get_mailbox_sync_state, gmail_label_exists,
    has_pending_mailbox_pages, load_tokens, next_full_sync_generation, replace_gmail_labels,
    set_gmail_inbox_unread_stats, set_history_id, set_mailbox_cursor, set_mailbox_sync_state,
    set_thread_gmail_label_local, upsert_gmail_label, upsert_sync_mail_batch, Email, EmailSummary,
    GmailLabel,
};
use base64::Engine;
use futures::stream::{self, StreamExt, TryStreamExt};
use reqwest::{header::RETRY_AFTER, Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

const RATE_LIMIT_BACKOFF_SECS: i64 = 60;
const GMAIL_GET_MAX_ATTEMPTS: u32 = 3;
const GMAIL_GET_BASE_BACKOFF_MS: u64 = 400;
const GMAIL_GET_MAX_BACKOFF_MS: u64 = 5_000;
const GMAIL_GET_MAX_RETRY_AFTER_SECS: u64 = 30;

fn is_retryable_gmail_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn parse_retry_after_delay(value: &str) -> Option<std::time::Duration> {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .map(|seconds| std::time::Duration::from_secs(seconds.min(GMAIL_GET_MAX_RETRY_AFTER_SECS)))
}

fn retry_after_delay(response: &Response) -> Option<std::time::Duration> {
    response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after_delay)
}

fn gmail_retry_delay(
    failed_attempt: u32,
    retry_after: Option<std::time::Duration>,
) -> std::time::Duration {
    let exponential_cap = GMAIL_GET_BASE_BACKOFF_MS
        .saturating_mul(1_u64 << failed_attempt.min(10))
        .min(GMAIL_GET_MAX_BACKOFF_MS);
    // Equal jitter keeps a meaningful delay while preventing concurrent account
    // syncs from retrying in lockstep.
    let half = exponential_cap / 2;
    let jittered_ms = half + rand::random::<u64>() % (exponential_cap - half + 1);
    std::time::Duration::from_millis(jittered_ms).max(retry_after.unwrap_or_default())
}

fn is_retryable_gmail_transport_error(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect() || error.is_request()
}

/// Retry only idempotent Gmail GETs. Mutating POST/DELETE requests deliberately
/// stay on their existing single-attempt path so an ambiguous response cannot
/// duplicate a user action.
async fn gmail_get_with_retry(
    client: &Client,
    access_token: &str,
    url: String,
    error_context: &str,
) -> Result<Response, String> {
    for attempt in 0..GMAIL_GET_MAX_ATTEMPTS {
        match client.get(&url).bearer_auth(access_token).send().await {
            Ok(response)
                if is_retryable_gmail_status(response.status())
                    && attempt + 1 < GMAIL_GET_MAX_ATTEMPTS =>
            {
                let delay = gmail_retry_delay(attempt, retry_after_delay(&response));
                tokio::time::sleep(delay).await;
            }
            Ok(response) => return Ok(response),
            Err(error)
                if is_retryable_gmail_transport_error(&error)
                    && attempt + 1 < GMAIL_GET_MAX_ATTEMPTS =>
            {
                tokio::time::sleep(gmail_retry_delay(attempt, None)).await;
            }
            Err(_) => return Err(format!("{error_context}: network request failed")),
        }
    }

    unreachable!("Gmail GET retry loop always returns on its final attempt")
}

fn unix_timestamp_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn mailbox_failure_status(error: &str) -> (&'static str, Option<i64>) {
    let normalized = error.to_ascii_lowercase();
    if normalized.contains("429")
        || normalized.contains("ratelimit")
        || normalized.contains("rate limit")
        || normalized.contains("userratelimitexceeded")
    {
        return (
            "rate_limited",
            Some(unix_timestamp_secs() + RATE_LIMIT_BACKOFF_SECS),
        );
    }
    if normalized.contains("401")
        || normalized.contains("invalid credentials")
        || normalized.contains("unauthenticated")
    {
        return ("relogin_required", None);
    }
    ("error", Some(unix_timestamp_secs() + 15))
}

fn persist_mailbox_failure(
    app: &AppHandle,
    account_id: &str,
    account_generation: i64,
    error: &str,
) {
    if error == "Account is no longer available" {
        return;
    }
    let (status, retry_after) = mailbox_failure_status(error);
    if set_mailbox_sync_state(
        app,
        account_id,
        account_generation,
        status,
        Some(error),
        retry_after,
    )
    .is_err()
    {
        eprintln!("[SYNC] could not save mailbox status");
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct AttachmentPayload {
    pub filename: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    pub data: String, // base64-encoded file content
}

// ── History API types ──

#[derive(Deserialize, Debug)]
struct HistoryListResponse {
    history: Option<Vec<HistoryRecord>>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
    #[serde(rename = "historyId")]
    history_id: Option<String>,
}

#[derive(Deserialize, Debug)]
struct HistoryRecord {
    #[serde(rename = "messagesAdded")]
    messages_added: Option<Vec<HistoryMessage>>,
    #[serde(rename = "messagesDeleted")]
    messages_deleted: Option<Vec<HistoryMessage>>,
    #[serde(rename = "labelsAdded")]
    labels_added: Option<Vec<HistoryLabelChange>>,
    #[serde(rename = "labelsRemoved")]
    labels_removed: Option<Vec<HistoryLabelChange>>,
}

#[derive(Deserialize, Debug)]
struct HistoryMessage {
    message: HistoryMessageRef,
}

#[derive(Deserialize, Debug)]
struct HistoryMessageRef {
    id: String,
    #[serde(rename = "labelIds")]
    label_ids: Option<Vec<String>>,
}

#[derive(Deserialize, Debug)]
struct HistoryLabelChange {
    message: HistoryMessageRef,
    #[serde(rename = "labelIds")]
    label_ids: Option<Vec<String>>,
}

#[derive(Deserialize, Debug)]
struct ProfileResponse {
    #[serde(rename = "historyId")]
    history_id: String,
}

#[derive(Deserialize, Debug, PartialEq, Eq)]
struct GmailLabelStats {
    #[serde(rename = "messagesUnread")]
    messages_unread: i64,
    #[serde(rename = "threadsUnread")]
    threads_unread: i64,
}

// ── Get current historyId from Gmail profile ──
async fn get_profile_history_id(client: &Client, access_token: &str) -> Result<String, String> {
    let res = gmail_get_with_retry(
        client,
        access_token,
        "https://gmail.googleapis.com/gmail/v1/users/me/profile".to_string(),
        "Profile fetch error",
    )
    .await?;

    if !res.status().is_success() {
        return Err(format!("Profile API error: {}", res.status()));
    }

    let profile: ProfileResponse = res.json().await.map_err(|e| e.to_string())?;
    Ok(profile.history_id)
}

/// Gmail exposes both message and thread counts. The product badge intentionally
/// uses messagesUnread, matching the user's Gmail unread-message count.
async fn get_inbox_unread_stats(
    client: &Client,
    access_token: &str,
) -> Result<GmailLabelStats, String> {
    let res = gmail_get_with_retry(
        client,
        access_token,
        "https://gmail.googleapis.com/gmail/v1/users/me/labels/INBOX".to_string(),
        "Inbox label fetch error",
    )
    .await?;

    if !res.status().is_success() {
        return Err(format!("Inbox label API error: {}", res.status()));
    }

    res.json().await.map_err(|e| e.to_string())
}

async fn sync_gmail_labels(
    app: &AppHandle,
    account_id: &str,
    account_generation: i64,
    client: &Client,
    access_token: &str,
) -> Result<(), String> {
    let res = gmail_get_with_retry(
        client,
        access_token,
        "https://gmail.googleapis.com/gmail/v1/users/me/labels".to_string(),
        "Label list fetch error",
    )
    .await?;
    if !res.status().is_success() {
        return Err(format!("Label list API error: {}", res.status()));
    }
    let response: GmailLabelListResponse = res.json().await.map_err(|e| e.to_string())?;
    let labels: Vec<GmailLabel> = response
        .labels
        .unwrap_or_default()
        .into_iter()
        .filter(|label| label.label_type.as_deref() == Some("user"))
        .map(|label| GmailLabel {
            id: label.id,
            account_id: account_id.to_string(),
            name: label.name,
            background_color: label
                .color
                .as_ref()
                .and_then(|color| color.background_color.clone()),
            text_color: label.color.and_then(|color| color.text_color),
        })
        .collect();
    replace_gmail_labels(app, account_id, account_generation, &labels)
}

fn window_state_allows_label_sync(
    visible: Option<bool>,
    minimized: Option<bool>,
    focused: Option<bool>,
) -> bool {
    visible.unwrap_or(true) && !minimized.unwrap_or(false) && focused.unwrap_or(true)
}

fn should_sync_gmail_labels(app: &AppHandle) -> bool {
    let Some(window) = app.get_webview_window("main") else {
        return true;
    };
    window_state_allows_label_sync(
        window.is_visible().ok(),
        window.is_minimized().ok(),
        window.is_focused().ok(),
    )
}

// ── Fetch history changes since a given historyId ──
struct HistoryPage {
    fetch_ids: Vec<String>,
    delete_ids: Vec<String>,
    next_page_token: Option<String>,
    latest_history_id: Option<String>,
}

#[derive(Deserialize)]
struct GmailLabelListResponse {
    labels: Option<Vec<GmailLabelApi>>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GmailLabelApi {
    id: String,
    name: String,
    #[serde(rename = "type")]
    label_type: Option<String>,
    color: Option<GmailLabelColor>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct GmailLabelColor {
    background_color: Option<String>,
    text_color: Option<String>,
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

fn gmail_label_from_api(account_id: &str, label: GmailLabelApi) -> GmailLabel {
    GmailLabel {
        id: label.id,
        account_id: account_id.to_string(),
        name: label.name,
        background_color: label
            .color
            .as_ref()
            .and_then(|color| color.background_color.clone()),
        text_color: label.color.and_then(|color| color.text_color),
    }
}

fn gmail_label_color_allowed(background_color: &str, text_color: &str) -> bool {
    GMAIL_LABEL_COLOR_PAIRS
        .iter()
        .any(|pair| pair.0 == background_color && pair.1 == text_color)
}

async fn fetch_history_page(
    client: &Client,
    access_token: &str,
    start_history_id: &str,
    page_token: Option<&str>,
) -> Result<HistoryPage, String> {
    let mut added_ids = std::collections::HashSet::new();
    let mut deleted_ids = std::collections::HashSet::new();
    let mut changed_ids = std::collections::HashSet::new();
    validate_gmail_identifier("history ID", start_history_id)?;
    let mut url = reqwest::Url::parse("https://gmail.googleapis.com/gmail/v1/users/me/history")
        .map_err(|e| e.to_string())?;
    {
        let mut params = url.query_pairs_mut();
        params
            .append_pair("startHistoryId", start_history_id)
            .append_pair("maxResults", "500");
        if let Some(token) = page_token {
            params.append_pair("pageToken", token);
        }
    }

    let res =
        gmail_get_with_retry(client, access_token, url.to_string(), "History fetch error").await?;

    let status = res.status();
    if status.as_u16() == 404 {
        return Err("HISTORY_EXPIRED".to_string());
    }
    if !status.is_success() {
        let body = res.text().await.unwrap_or_default();
        if body.contains("notFound") || body.contains("Start history id is too old") {
            return Err("HISTORY_EXPIRED".to_string());
        }
        return Err(format!("History API error (HTTP {status})"));
    }

    let data: HistoryListResponse = res.json().await.map_err(|e| e.to_string())?;
    let latest_history_id = data.history_id.clone();

    if let Some(records) = data.history {
        for record in records {
            if let Some(added) = record.messages_added {
                for msg in added {
                    added_ids.insert(msg.message.id);
                }
            }
            if let Some(deleted) = record.messages_deleted {
                for msg in deleted {
                    deleted_ids.insert(msg.message.id);
                }
            }
            if let Some(label_adds) = record.labels_added {
                for change in label_adds {
                    changed_ids.insert(change.message.id);
                }
            }
            if let Some(label_removes) = record.labels_removed {
                for change in label_removes {
                    changed_ids.insert(change.message.id);
                }
            }
        }
    }

    for did in &deleted_ids {
        added_ids.remove(did);
        changed_ids.remove(did);
    }

    let mut fetch_ids: Vec<String> = added_ids.into_iter().collect();
    fetch_ids.extend(changed_ids);

    Ok(HistoryPage {
        fetch_ids,
        delete_ids: deleted_ids.into_iter().collect(),
        next_page_token: data.next_page_token,
        latest_history_id,
    })
}

// ── Incremental sync using History API ──
async fn do_incremental_sync(
    app: &AppHandle,
    account_id: &str,
    account_generation: i64,
    access_token: &str,
    start_history_id: &str,
) -> Result<(), String> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_default();

    const DETAIL_BATCH_SIZE: usize = 10;
    const DELETE_BATCH_SIZE: usize = 400;
    let mut page_token: Option<String> = None;
    let mut seen_page_tokens = std::collections::HashSet::new();
    let mut total_fetched = 0usize;
    let mut total_deleted = 0usize;
    let mut latest_history_id: Option<String> = None;

    let new_history_id = loop {
        let page = fetch_history_page(
            &client,
            access_token,
            start_history_id,
            page_token.as_deref(),
        )
        .await?;
        if page.latest_history_id.is_some() {
            latest_history_id = page.latest_history_id.clone();
        }
        total_fetched += page.fetch_ids.len();
        total_deleted += page.delete_ids.len();

        for ids in page.fetch_ids.chunks(DETAIL_BATCH_SIZE) {
            cache_message_details(
                app,
                account_id,
                &client,
                access_token,
                ids.iter().cloned().map(|id| MessageId { id }).collect(),
                None,
                account_generation,
            )
            .await
            .map_err(|error| {
                format!(
                    "Incremental sync detail fetch failed; history checkpoint was not advanced: {error}"
                )
            })?;
        }

        for ids in page.delete_ids.chunks(DELETE_BATCH_SIZE) {
            let app_clone = app.clone();
            let account_for_delete = account_id.to_string();
            let ids = ids.to_vec();
            tokio::task::spawn_blocking(move || {
                delete_emails_by_ids(&app_clone, &account_for_delete, account_generation, &ids)
            })
            .await
            .map_err(|e| format!("DB delete task failed: {e}"))??;
        }

        let Some(next_page_token) = page.next_page_token else {
            break latest_history_id.unwrap_or_else(|| start_history_id.to_string());
        };
        if !seen_page_tokens.insert(next_page_token.clone()) {
            return Err("Gmail history pagination cursor repeated".to_string());
        }
        page_token = Some(next_page_token);
    };

    eprintln!(
        "[SYNC] incremental: {} fetched, {} deleted",
        total_fetched, total_deleted
    );

    // Advancing this checkpoint is the final step: a failed sync must be retried
    // from the same history ID so Gmail does not permanently skip any change.
    if get_account_cache_generation(app, account_id)? != account_generation {
        return Err("Account is no longer available".to_string());
    }
    set_history_id(app, account_id, account_generation, &new_history_id)?;

    Ok(())
}

// ── Full sync ──
async fn do_sync(
    app: &AppHandle,
    account_id: &str,
    account_generation: i64,
    access_token: &str,
) -> Result<(), String> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();

    let profile_history_id = get_profile_history_id(&client, access_token).await?;
    let sync_generation = next_full_sync_generation(app, account_id)?;

    let mut all_ids = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();
    let mut cursors = Vec::new();

    for label in ["inbox", "sent", "spam", "trash", "archive"] {
        let page = fetch_message_page(
            &client,
            access_token,
            mailbox_query(label).expect("known mailbox label"),
            None,
            100,
        )
        .await?;
        for msg in page.messages {
            if seen_ids.insert(msg.id.clone()) {
                all_ids.push(msg.id);
            }
        }
        cursors.push((label.to_string(), page.next_page_token));
    }

    eprintln!("[SYNC] full sync: {} messages to fetch", all_ids.len());

    cache_message_details(
        app,
        account_id,
        &client,
        access_token,
        all_ids.into_iter().map(|id| MessageId { id }).collect(),
        Some(sync_generation),
        account_generation,
    )
    .await?;

    if get_account_cache_generation(app, account_id)? != account_generation {
        return Err("Account is no longer available".to_string());
    }
    complete_full_sync(
        app,
        account_id,
        account_generation,
        &cursors,
        &profile_history_id,
        sync_generation,
    )?;
    eprintln!("[SYNC] full sync done");

    Ok(())
}

#[derive(Deserialize, Debug)]
struct MessageListResponse {
    messages: Option<Vec<MessageId>>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Deserialize, Debug)]
struct MessageId {
    id: String,
}

struct MessagePage {
    messages: Vec<MessageId>,
    next_page_token: Option<String>,
}

#[derive(Deserialize, Debug)]
struct MessageDetail {
    id: String,
    #[serde(rename = "threadId")]
    thread_id: Option<String>,
    snippet: String,
    payload: Payload,
    #[serde(rename = "internalDate")]
    internal_date: String,
    #[serde(rename = "labelIds")]
    label_ids: Option<Vec<String>>,
}

#[derive(Deserialize, Debug)]
struct Payload {
    headers: Vec<Header>,
    parts: Option<Vec<MessagePart>>,
    body: Option<MessageBody>,
}

#[derive(Deserialize, Debug)]
struct Header {
    name: String,
    value: String,
}

#[derive(Deserialize, Debug)]
struct MessagePart {
    #[serde(rename = "mimeType")]
    mime_type: String,
    #[serde(rename = "partId")]
    part_id: Option<String>,
    filename: Option<String>,
    headers: Option<Vec<Header>>,
    body: Option<MessageBody>,
    parts: Option<Vec<MessagePart>>,
}

#[derive(Deserialize, Debug)]
struct MessageBody {
    data: Option<String>,
    #[serde(rename = "attachmentId")]
    attachment_id: Option<String>,
    size: Option<i64>,
}

/// Determine the label for an email based on Gmail label IDs
fn determine_label(label_ids: &[String]) -> String {
    if label_ids.iter().any(|label| label == "DRAFT") {
        "draft".to_string()
    } else if label_ids.iter().any(|label| label == "SPAM") {
        "spam".to_string()
    } else if label_ids.iter().any(|label| label == "TRASH") {
        "trash".to_string()
    } else if label_ids.iter().any(|label| label == "SENT")
        && !label_ids.iter().any(|label| label == "INBOX")
    {
        "sent".to_string()
    } else if label_ids.iter().any(|label| label == "INBOX") {
        "inbox".to_string()
    } else {
        "archive".to_string()
    }
}

/// Parse a single Gmail message detail into our Email struct + attachment list
fn parse_message_detail(detail: MessageDetail) -> (Email, Vec<crate::db::Attachment>) {
    let mut sender = "Unknown Sender".to_string();
    let mut recipient = String::new();
    let mut cc = String::new();
    let mut reply_to = String::new();
    let mut message_id = String::new();
    let mut references = String::new();
    let mut subject = "No Subject".to_string();

    for header in &detail.payload.headers {
        if header.name.eq_ignore_ascii_case("from") {
            sender = header.value.clone();
        } else if header.name.eq_ignore_ascii_case("to") {
            recipient = header.value.clone();
        } else if header.name.eq_ignore_ascii_case("cc") {
            cc = header.value.clone();
        } else if header.name.eq_ignore_ascii_case("reply-to") {
            reply_to = header.value.clone();
        } else if header.name.eq_ignore_ascii_case("message-id") {
            message_id = header.value.clone();
        } else if header.name.eq_ignore_ascii_case("references") {
            references = header.value.clone();
        } else if header.name.eq_ignore_ascii_case("subject") {
            subject = header.value.clone();
        }
    }

    // Parse HTML/Text body (base64url encoded)
    let mut body_html = String::new();

    if let Some(parts) = &detail.payload.parts {
        if let Some(data) = find_part_data(parts, "text/html") {
            body_html = decode_base64_url(data);
        }
    }

    if body_html.is_empty() {
        if let Some(parts) = &detail.payload.parts {
            if let Some(data) = find_part_data(parts, "text/plain") {
                body_html = decode_base64_url(data);
            }
        }
    }

    if body_html.is_empty() {
        if let Some(body) = &detail.payload.body {
            if let Some(data) = &body.data {
                body_html = decode_base64_url(data);
            }
        }
    }

    // Resolve Inline Images (CID)
    if let Some(parts) = &detail.payload.parts {
        let mut cids = std::collections::HashMap::new();
        collect_inline_images(parts, &mut cids);

        for (cid, data_uri) in cids {
            let cid_target = format!("cid:{}", cid);
            body_html = body_html.replace(&cid_target, &data_uri);
        }
    }

    let labels = detail.label_ids.unwrap_or_default();
    let is_unread = labels.contains(&"UNREAD".to_string());
    let label = determine_label(&labels);
    let date_i64 = detail.internal_date.parse::<i64>().unwrap_or(0);

    let attachments = if let Some(parts) = &detail.payload.parts {
        collect_attachments(parts, &detail.id, "")
    } else {
        vec![]
    };

    let email = Email {
        id: detail.id,
        thread_id: detail.thread_id.unwrap_or_default(),
        sender,
        recipient,
        cc,
        reply_to,
        message_id,
        references,
        subject,
        snippet: detail.snippet,
        body_html,
        date: date_i64,
        unread: is_unread,
        label,
        gmail_label_ids: labels,
    };

    (email, attachments)
}

fn mailbox_query(label: &str) -> Result<&'static str, String> {
    match label {
        "inbox" => Ok("in:inbox"),
        "sent" => Ok("in:sent"),
        "spam" => Ok("in:spam"),
        "trash" => Ok("in:trash"),
        "archive" => Ok("-in:inbox -in:sent -in:spam -in:trash -in:drafts"),
        _ => Err("Unknown mailbox label".to_string()),
    }
}

/// Fetch one Gmail result page. We persist its continuation token so a later
/// scroll can continue where the initial cache stopped.
async fn fetch_message_page(
    client: &Client,
    access_token: &str,
    query: &str,
    page_token: Option<&str>,
    max_results: u32,
) -> Result<MessagePage, String> {
    let mut url = reqwest::Url::parse("https://gmail.googleapis.com/gmail/v1/users/me/messages")
        .map_err(|e| e.to_string())?;
    {
        let mut params = url.query_pairs_mut();
        params.append_pair("maxResults", &max_results.min(100).to_string());
        params.append_pair("q", query);
        if let Some(token) = page_token {
            params.append_pair("pageToken", token);
        }
    }

    let res =
        gmail_get_with_retry(client, access_token, url.to_string(), "List fetch error").await?;

    if !res.status().is_success() {
        let status = res.status();
        return Err(format!("Gmail API error (HTTP {status})"));
    }

    let list_data: MessageListResponse = res.json().await.map_err(|e| e.to_string())?;
    Ok(MessagePage {
        messages: list_data.messages.unwrap_or_default(),
        next_page_token: list_data.next_page_token,
    })
}

/// Fetch full details of a single message
async fn fetch_message_detail(
    client: &Client,
    access_token: &str,
    msg_id: &str,
) -> Result<MessageDetail, String> {
    validate_gmail_identifier("message ID", msg_id)?;
    let url = format!(
        "https://gmail.googleapis.com/gmail/v1/users/me/messages/{}?format=full",
        msg_id
    );
    let res = gmail_get_with_retry(client, access_token, url, "Detail fetch error").await?;

    if !res.status().is_success() {
        return Err(format!("Detail fetch API error: {}", res.status()));
    }

    res.json::<MessageDetail>().await.map_err(|e| e.to_string())
}

async fn backfill_mailbox(
    app: &AppHandle,
    account_id: &str,
    account_generation: i64,
    access_token: &str,
) -> Result<(), String> {
    let full_sync = get_active_full_sync(app, account_id)?;
    let sync_generation = full_sync.as_ref().map(|sync| sync.generation);
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();

    for label in ["inbox", "sent", "spam", "trash", "archive"] {
        let mut seen_page_tokens = std::collections::HashSet::new();
        loop {
            let Some(Some(page_token)) = get_mailbox_cursor_state(app, account_id, label)? else {
                break;
            };
            if !seen_page_tokens.insert(page_token.clone()) {
                return Err(format!("Gmail pagination cursor repeated for {label}"));
            }
            let page = fetch_message_page(
                &client,
                access_token,
                mailbox_query(label)?,
                Some(&page_token),
                100,
            )
            .await?;
            let next_page_token = page.next_page_token;
            if next_page_token.as_deref() == Some(page_token.as_str()) {
                return Err(format!(
                    "Gmail pagination cursor did not advance for {label}"
                ));
            }
            if get_account_cache_generation(app, account_id)? != account_generation {
                return Err("Account is no longer available".to_string());
            }
            cache_message_details(
                app,
                account_id,
                &client,
                access_token,
                page.messages,
                sync_generation,
                account_generation,
            )
            .await?;
            if get_account_cache_generation(app, account_id)? != account_generation {
                return Err("Account is no longer available".to_string());
            }
            set_mailbox_cursor(
                app,
                account_id,
                account_generation,
                label,
                next_page_token.as_deref(),
            )?;

            if next_page_token.is_none() {
                break;
            }
        }
    }

    if let Some(full_sync) = full_sync {
        if get_account_cache_generation(app, account_id)? != account_generation {
            return Err("Account is no longer available".to_string());
        }
        let removed =
            finalize_full_sync(app, account_id, account_generation, full_sync.generation)?;
        eprintln!(
            "[SYNC] full mailbox rebuild complete; removed {} stale local messages",
            removed
        );
    }

    set_mailbox_sync_state(app, account_id, account_generation, "completed", None, None)?;

    Ok(())
}

async fn run_sync_cycle(
    app: &AppHandle,
    account_id: &str,
    account_generation: i64,
    initial_token: String,
) -> Result<String, String> {
    let mut token = initial_token;
    loop {
        if get_active_full_sync(app, account_id)?.is_some() {
            return Ok(token);
        }
        // Mail and notification synchronization must keep running in the tray,
        // but refreshing the user-label catalog is only useful while the main
        // window can display it.
        if should_sync_gmail_labels(app) {
            let label_client = Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap_or_default();
            sync_gmail_labels(app, account_id, account_generation, &label_client, &token).await?;
        }
        let history_id = get_history_id(app, account_id);
        if let Some(history_id) = history_id {
            match do_incremental_sync(app, account_id, account_generation, &token, &history_id)
                .await
            {
                Ok(()) => {}
                Err(error) if error == "HISTORY_EXPIRED" => {
                    eprintln!("[SYNC] history expired, full sync");
                    do_sync(app, account_id, account_generation, &token).await?;
                }
                Err(error) => return Err(error),
            }
        } else {
            do_sync(app, account_id, account_generation, &token).await?;
        }

        // Keep the displayed badge on Gmail's message-count semantics. This is
        // reconciliation data: direct UI actions still update the badge
        // optimistically instead of waiting for Gmail's label-stat propagation.
        let stats_client = Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        match get_inbox_unread_stats(&stats_client, &token).await {
            Ok(stats) => set_gmail_inbox_unread_stats(
                app,
                account_id,
                account_generation,
                stats.messages_unread,
                stats.threads_unread,
            )?,
            Err(_) => eprintln!("[SYNC] inbox unread stats unavailable"),
        }

        let run_again = {
            let state = app.state::<crate::SyncState>();
            let mut workers = state
                .workers
                .lock()
                .map_err(|_| "Sync worker lock poisoned")?;
            workers.take_resync_request(account_id, account_generation)
        };
        if !run_again {
            return Ok(token);
        }

        if let Some(tokens) = load_tokens(account_id) {
            token = tokens.access_token;
        }
    }
}

async fn run_background_backfill_worker(
    app: AppHandle,
    account_id: String,
    account_generation: i64,
    mut token: String,
) {
    loop {
        let started = {
            let state = app.state::<crate::SyncState>();
            let started = match state.workers.lock() {
                Ok(mut workers) => workers.set_backfilling(&account_id, account_generation),
                Err(_) => false,
            };
            started
        };
        if !started {
            return;
        }

        if set_mailbox_sync_state(&app, &account_id, account_generation, "running", None, None)
            .is_err()
        {
            eprintln!("[SYNC] could not save mailbox status");
        }

        let result = backfill_mailbox(&app, &account_id, account_generation, &token).await;
        if let Err(error) = &result {
            eprintln!("[SYNC] background mailbox download paused");
            persist_mailbox_failure(&app, &account_id, account_generation, error);
        }

        let run_again = {
            let state = app.state::<crate::SyncState>();
            let run_again = match state.workers.lock() {
                Ok(mut workers) => workers.take_resync_or_release(&account_id, account_generation),
                Err(_) => false,
            };
            run_again
        };
        if !run_again {
            return;
        }

        if let Some(tokens) = load_tokens(&account_id) {
            token = tokens.access_token;
        }
        match run_sync_cycle(&app, &account_id, account_generation, token).await {
            Ok(fresh_token) => token = fresh_token,
            Err(error) => {
                eprintln!("[SYNC] queued sync failed");
                persist_mailbox_failure(&app, &account_id, account_generation, &error);
                if let Ok(mut workers) = app.state::<crate::SyncState>().workers.lock() {
                    workers.release(&account_id, account_generation);
                }
                return;
            }
        }
    }
}

fn start_background_backfill(
    app: &AppHandle,
    account_id: String,
    account_generation: i64,
    access_token: String,
) {
    let app = app.clone();
    let task_app = app.clone();
    let task_account_id = account_id.clone();
    let (start_sender, start_receiver) = tokio::sync::oneshot::channel();
    let (cancel_sender, mut cancel_receiver) = tokio::sync::oneshot::channel();
    let task = tauri::async_runtime::spawn(async move {
        if start_receiver.await.is_err() {
            return;
        }
        tokio::select! {
            _ = &mut cancel_receiver => return,
            _ = run_background_backfill_worker(
                task_app.clone(),
                task_account_id.clone(),
                account_generation,
                access_token,
            ) => {}
        }
        if let Ok(mut workers) = task_app.state::<crate::SyncState>().workers.lock() {
            workers.finish_backfill_task(&task_account_id, account_generation);
        }
    });
    match app.state::<crate::SyncState>().workers.lock() {
        Ok(mut workers) => {
            workers.register_backfill_task(&account_id, account_generation, cancel_sender);
            let _ = start_sender.send(());
        }
        Err(_) => task.abort(),
    };
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
    if crate::db::get_account_provider(&app, &account_id)? == "imap" {
        return crate::mail_account::sync_imap_account(&app, &account_id).await;
    }
    let access_token = crate::db::load_account_access_token(&account_id)?;
    let account_generation = get_account_cache_generation(&app, &account_id)?;
    if !force.unwrap_or(false) {
        if let Some(state) = get_mailbox_sync_state(&app, &account_id)? {
            if state
                .retry_after
                .is_some_and(|at| at > unix_timestamp_secs())
            {
                return Ok(());
            }
        }
    }
    let state = app.state::<crate::SyncState>();

    let owns_worker = state
        .workers
        .lock()
        .map_err(|_| "Sync worker lock poisoned")?
        .claim_or_request_resync(&account_id, account_generation);
    if !owns_worker {
        return Ok(());
    }

    if let Err(error) =
        set_mailbox_sync_state(&app, &account_id, account_generation, "running", None, None)
    {
        if let Ok(mut workers) = state.workers.lock() {
            workers.release(&account_id, account_generation);
        }
        return Err(error);
    }

    let token = match run_sync_cycle(&app, &account_id, account_generation, access_token).await {
        Ok(token) => token,
        Err(error) => {
            if let Ok(mut workers) = state.workers.lock() {
                workers.release(&account_id, account_generation);
            }
            persist_mailbox_failure(&app, &account_id, account_generation, &error);
            return Err(error);
        }
    };

    start_background_backfill(&app, account_id, account_generation, token);
    Ok(())
}

/// Refreshes exactly one open message without making the mail list wait for
/// Gmail or restarting mailbox pagination.
#[tauri::command]
pub async fn refresh_email_from_gmail(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    message_id: String,
) -> Result<EmailSummary, String> {
    crate::require_command_window(&window, &["main"])?;
    let access_token = crate::db::load_account_access_token(&account_id)?;
    let account_generation = get_account_cache_generation(&app, &account_id)?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_default();
    let detail = fetch_message_detail(&client, &access_token, &message_id).await?;
    let (email, mut attachments) = parse_message_detail(detail);
    let summary = EmailSummary {
        id: email.id.clone(),
        thread_id: email.thread_id.clone(),
        sender: email.sender.clone(),
        recipient: email.recipient.clone(),
        cc: email.cc.clone(),
        reply_to: email.reply_to.clone(),
        message_id: email.message_id.clone(),
        references: email.references.clone(),
        subject: email.subject.clone(),
        snippet: email.snippet.clone(),
        date: email.date,
        unread: email.unread,
        label: email.label.clone(),
        account_id: account_id.clone(),
    };
    for attachment in &mut attachments {
        attachment.account_id = account_id.clone();
    }

    let app_for_db = app.clone();
    let account_for_db = account_id.clone();
    tokio::task::spawn_blocking(move || {
        upsert_sync_mail_batch(
            &app_for_db,
            &account_for_db,
            account_generation,
            None,
            vec![email],
            attachments,
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("Message refresh DB task failed: {e}"))??;

    Ok(summary)
}

async fn cache_message_details(
    app: &AppHandle,
    account_id: &str,
    client: &Client,
    access_token: &str,
    ids: Vec<MessageId>,
    sync_generation: Option<i64>,
    account_generation: i64,
) -> Result<(), String> {
    if get_account_cache_generation(app, account_id)? != account_generation {
        return Err("Account is no longer available".to_string());
    }
    let parsed: Vec<(Email, Vec<crate::db::Attachment>)> = stream::iter(ids)
        .map(|message| async move {
            fetch_message_detail(client, access_token, &message.id)
                .await
                .map(parse_message_detail)
        })
        .buffer_unordered(10)
        .try_collect()
        .await
        .map_err(|e| {
            format!("Message detail fetch failed; mailbox cursor was not advanced: {e}")
        })?;

    if parsed.is_empty() {
        return Ok(());
    }

    let account_id = account_id.to_string();
    let (emails, mut attachments): (Vec<Email>, Vec<Vec<crate::db::Attachment>>) =
        parsed.into_iter().unzip();
    let attachments: Vec<crate::db::Attachment> = attachments
        .iter_mut()
        .flat_map(|items| {
            items.iter_mut().map(|attachment| {
                attachment.account_id = account_id.clone();
                attachment.clone()
            })
        })
        .collect();
    let app = app.clone();
    let account_for_db = account_id.clone();
    tokio::task::spawn_blocking(move || {
        upsert_sync_mail_batch(
            &app,
            &account_for_db,
            account_generation,
            sync_generation,
            emails,
            attachments,
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("DB task failed: {}", e))??;

    Ok(())
}

#[tauri::command]
pub async fn archive_email(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    thread_id: String,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    if crate::db::get_account_provider(&app, &account_id)? == "imap" {
        return crate::mail_account::move_imap_thread(
            &app,
            &account_id,
            &thread_id,
            "archive",
        )
        .await;
    }
    let access_token = crate::db::load_account_access_token(&account_id)?;
    validate_gmail_identifier("thread ID", &thread_id)?;
    // The reader toolbar represents the whole conversation, matching Gmail.
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    let url = format!(
        "https://gmail.googleapis.com/gmail/v1/users/me/threads/{}/modify",
        thread_id
    );
    let body = serde_json::json!({
        "removeLabelIds": ["INBOX"]
    });

    let res = client
        .post(&url)
        .bearer_auth(&access_token)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !res.status().is_success() {
        return Err(format!("Gmail archive error (HTTP {}).", res.status()));
    }

    if let Err(error) = crate::db::archive_thread_local(&app, &thread_id, &account_id) {
        if let Err(sync_error) = sync_emails(window, app, account_id, Some(true)).await {
            eprintln!("[MAIL_CACHE] archive reconciliation failed: {sync_error}");
        }
        return Err(format!(
            "Gmail arşivlendi ancak yerel önbellek güncellenemedi: {error}"
        ));
    }

    Ok(())
}

#[tauri::command]
pub async fn create_gmail_label(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    name: String,
) -> Result<GmailLabel, String> {
    crate::require_command_window(&window, &["main"])?;
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 225 {
        return Err("Label name must contain between 1 and 225 characters.".to_string());
    }
    let access_token = crate::db::load_account_access_token(&account_id)?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    let response = client
        .post("https://gmail.googleapis.com/gmail/v1/users/me/labels")
        .bearer_auth(&access_token)
        .json(&serde_json::json!({
            "name": name,
            "labelListVisibility": "labelShow",
            "messageListVisibility": "show"
        }))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Gmail label creation error (HTTP {}).",
            response.status()
        ));
    }
    let created: GmailLabelApi = response.json().await.map_err(|error| error.to_string())?;
    let label = gmail_label_from_api(&account_id, created);
    upsert_gmail_label(&app, &label)?;
    Ok(label)
}

#[tauri::command]
pub async fn rename_gmail_label(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    label_id: String,
    name: String,
) -> Result<GmailLabel, String> {
    crate::require_command_window(&window, &["main"])?;
    validate_gmail_identifier("label ID", &label_id)?;
    if !gmail_label_exists(&app, &account_id, &label_id)? {
        return Err("The selected Gmail label is not available for this account.".to_string());
    }
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 225 {
        return Err("Label name must contain between 1 and 225 characters.".to_string());
    }
    let access_token = crate::db::load_account_access_token(&account_id)?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    let response = client
        .patch(format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/labels/{}",
            label_id
        ))
        .bearer_auth(&access_token)
        .json(&serde_json::json!({ "name": name }))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Gmail label rename error (HTTP {}).",
            response.status()
        ));
    }
    let updated = gmail_label_from_api(
        &account_id,
        response
            .json::<GmailLabelApi>()
            .await
            .map_err(|error| error.to_string())?,
    );
    upsert_gmail_label(&app, &updated)?;
    Ok(updated)
}

#[tauri::command]
pub async fn set_gmail_label_color(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    label_id: String,
    background_color: Option<String>,
    text_color: Option<String>,
) -> Result<GmailLabel, String> {
    crate::require_command_window(&window, &["main"])?;
    validate_gmail_identifier("label ID", &label_id)?;
    if !gmail_label_exists(&app, &account_id, &label_id)? {
        return Err("The selected Gmail label is not available for this account.".to_string());
    }
    let color = match (background_color, text_color) {
        (None, None) => serde_json::Value::Null,
        (Some(background), Some(text)) if gmail_label_color_allowed(&background, &text) => {
            serde_json::json!({ "backgroundColor": background, "textColor": text })
        }
        _ => return Err("Unsupported Gmail label color.".to_string()),
    };
    let access_token = crate::db::load_account_access_token(&account_id)?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    let response = client
        .patch(format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/labels/{}",
            label_id
        ))
        .bearer_auth(&access_token)
        .json(&serde_json::json!({ "color": color }))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Gmail label color error (HTTP {}).",
            response.status()
        ));
    }
    let updated = gmail_label_from_api(
        &account_id,
        response
            .json::<GmailLabelApi>()
            .await
            .map_err(|error| error.to_string())?,
    );
    upsert_gmail_label(&app, &updated)?;
    Ok(updated)
}

#[tauri::command]
pub async fn delete_gmail_label(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    label_id: String,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    validate_gmail_identifier("label ID", &label_id)?;
    if !gmail_label_exists(&app, &account_id, &label_id)? {
        return Err("The selected Gmail label is not available for this account.".to_string());
    }
    let access_token = crate::db::load_account_access_token(&account_id)?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    let response = client
        .delete(format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/labels/{}",
            label_id
        ))
        .bearer_auth(&access_token)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Gmail label delete error (HTTP {}).",
            response.status()
        ));
    }
    delete_gmail_label_local(&app, &account_id, &label_id)
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
    if crate::db::get_account_provider(&app, &account_id)? == "imap" {
        if label_id != "STARRED" {
            return Err("mail_account_label_not_supported".to_string());
        }
        crate::mail_account::set_imap_message_flag(
            &app,
            &account_id,
            &thread_id,
            true,
            imap_rs::client::Flag::Flagged,
            applied,
        )
        .await?;
        return set_thread_gmail_label_local(&app, &account_id, &thread_id, &label_id, applied);
    }
    validate_gmail_identifier("thread ID", &thread_id)?;
    validate_gmail_identifier("label ID", &label_id)?;
    if label_id != "STARRED" && !gmail_label_exists(&app, &account_id, &label_id)? {
        return Err("The selected Gmail label is not available for this account.".to_string());
    }
    let access_token = crate::db::load_account_access_token(&account_id)?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    let body = if applied {
        serde_json::json!({ "addLabelIds": [label_id] })
    } else {
        serde_json::json!({ "removeLabelIds": [label_id] })
    };
    let response = client
        .post(format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/threads/{}/modify",
            thread_id
        ))
        .bearer_auth(&access_token)
        .json(&body)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Gmail label update error (HTTP {}).",
            response.status()
        ));
    }
    set_thread_gmail_label_local(&app, &account_id, &thread_id, &label_id, applied)
}

#[tauri::command]
pub async fn report_spam(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    thread_id: String,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    if crate::db::get_account_provider(&app, &account_id)? == "imap" {
        return crate::mail_account::move_imap_thread(&app, &account_id, &thread_id, "junk")
            .await;
    }
    let access_token = crate::db::load_account_access_token(&account_id)?;
    validate_gmail_identifier("thread ID", &thread_id)?;
    // Gmail applies spam classification to the whole conversation.
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    let url = format!(
        "https://gmail.googleapis.com/gmail/v1/users/me/threads/{}/modify",
        thread_id
    );
    let body = serde_json::json!({
        "addLabelIds": ["SPAM"],
        "removeLabelIds": ["INBOX"]
    });
    let response = client
        .post(&url)
        .bearer_auth(&access_token)
        .json(&body)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Gmail spam report error (HTTP {}).",
            response.status()
        ));
    }
    if let Err(error) = crate::db::spam_thread_local(&app, &thread_id, &account_id) {
        if let Err(sync_error) = sync_emails(window, app, account_id, Some(true)).await {
            eprintln!("[MAIL_CACHE] spam reconciliation failed: {sync_error}");
        }
        return Err(format!(
            "Gmail marked the conversation as spam, but the local cache could not be updated: {error}"
        ));
    }
    Ok(())
}

fn gmail_trash_request(client: &Client, url: &str, access_token: &str) -> reqwest::RequestBuilder {
    client
        .post(url)
        .bearer_auth(access_token)
        // Reqwest may omit Content-Length for an empty body. Gmail's action
        // endpoint requires the header even when the payload is zero bytes.
        .header(reqwest::header::CONTENT_LENGTH, "0")
        .body(Vec::<u8>::new())
}

#[tauri::command]
pub async fn trash_email(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    thread_id: String,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    if crate::db::get_account_provider(&app, &account_id)? == "imap" {
        return crate::mail_account::move_imap_thread(&app, &account_id, &thread_id, "trash")
            .await;
    }
    let access_token = crate::db::load_account_access_token(&account_id)?;
    validate_gmail_identifier("thread ID", &thread_id)?;
    // Trash the whole conversation, matching Gmail's top toolbar.
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    let url = format!(
        "https://gmail.googleapis.com/gmail/v1/users/me/threads/{}/trash",
        thread_id
    );

    let res = gmail_trash_request(&client, &url, &access_token)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !res.status().is_success() {
        return Err(format!("Gmail trash error (HTTP {}).", res.status()));
    }

    if let Err(error) = crate::db::trash_thread_local(&app, &thread_id, &account_id) {
        if let Err(sync_error) = sync_emails(window, app, account_id, Some(true)).await {
            eprintln!("[MAIL_CACHE] trash reconciliation failed: {sync_error}");
        }
        return Err(format!(
            "Gmail çöp kutusuna taşıdı ancak yerel önbellek güncellenemedi: {error}"
        ));
    }

    Ok(())
}

#[tauri::command]
pub async fn move_to_inbox(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    thread_id: String,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    if crate::db::get_account_provider(&app, &account_id)? == "imap" {
        return crate::mail_account::move_imap_thread(&app, &account_id, &thread_id, "inbox")
            .await;
    }
    let access_token = crate::db::load_account_access_token(&account_id)?;
    validate_gmail_identifier("thread ID", &thread_id)?;
    // Restore the whole conversation, matching the reader toolbar scope.
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    let url = format!(
        "https://gmail.googleapis.com/gmail/v1/users/me/threads/{}/modify",
        thread_id
    );
    let body = serde_json::json!({
        "addLabelIds": ["INBOX"],
        "removeLabelIds": ["SPAM", "TRASH"]
    });

    let res = client
        .post(&url)
        .bearer_auth(&access_token)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !res.status().is_success() {
        return Err(format!("Gmail move error (HTTP {}).", res.status()));
    }

    if let Err(error) = crate::db::move_thread_to_inbox_local(&app, &thread_id, &account_id) {
        if let Err(sync_error) = sync_emails(window, app, account_id, Some(true)).await {
            eprintln!("[MAIL_CACHE] inbox reconciliation failed: {sync_error}");
        }
        return Err(format!(
            "Gmail gelen kutusuna taşıdı ancak yerel önbellek güncellenemedi: {error}"
        ));
    }

    Ok(())
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

#[derive(Deserialize, Debug)]
struct DraftListResponse {
    drafts: Option<Vec<DraftReference>>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Deserialize, Debug)]
struct DraftReference {
    id: String,
    message: Option<DraftReferenceMessage>,
}

#[derive(Deserialize, Debug)]
struct DraftReferenceMessage {
    id: String,
}

#[derive(Deserialize, Debug)]
struct GmailDraft {
    id: String,
    message: MessageDetail,
}

#[derive(Deserialize, Debug)]
struct SavedGmailDraft {
    id: String,
    message: SavedDraftMessage,
}

#[derive(Deserialize, Debug)]
struct SavedDraftMessage {
    id: String,
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

fn validate_gmail_identifier(name: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 4_096
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(format!("Invalid {name}"));
    }
    Ok(())
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

enum SendAttempt {
    Confirmed(Response),
    OutcomeUnknown,
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

async fn execute_send(request: reqwest::RequestBuilder) -> Result<SendAttempt, String> {
    match request.send().await {
        Ok(response) => Ok(SendAttempt::Confirmed(response)),
        Err(error) if is_retryable_gmail_transport_error(&error) => Ok(SendAttempt::OutcomeUnknown),
        Err(_) => Err("Send request failed before Gmail responded.".to_string()),
    }
}

async fn cache_confirmed_sent_message(
    app: &AppHandle,
    account_id: &str,
    client: &Client,
    access_token: &str,
    gmail_message_id: &str,
) -> Result<(), String> {
    validate_gmail_identifier("message ID", gmail_message_id)?;
    let detail = fetch_message_detail(client, access_token, gmail_message_id).await?;
    let (email, mut attachments) = parse_message_detail(detail);
    for attachment in &mut attachments {
        attachment.account_id = account_id.to_string();
    }
    let account_generation = get_account_cache_generation(app, account_id)?;
    let app = app.clone();
    let account_id = account_id.to_string();
    tokio::task::spawn_blocking(move || {
        upsert_sync_mail_batch(
            &app,
            &account_id,
            account_generation,
            None,
            vec![email],
            attachments,
        )
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|_| "Sent-message cache task was interrupted.".to_string())?
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
    let is_imap = crate::db::get_account_provider(&app, &account_id)? == "imap";
    let atts = attachments.unwrap_or_default();
    validate_recipient_header(&to)?;
    validate_optional_recipient_header(&cc)?;
    validate_header_value("Subject", &subject, MAX_SUBJECT_BYTES)?;
    if !is_imap {
        validate_gmail_identifier("thread ID", &thread_id)?;
    }
    let outbound_message_id = generate_outbound_message_id();

    let clean_subject = subject
        .trim_start_matches("Re: ")
        .trim_start_matches("re: ");
    let mut headers = vec![
        ("To", to.clone()),
        (
            "Subject",
            format!("Re: {}", mime_encode_header(clean_subject)),
        ),
    ];
    if !cc.trim().is_empty() {
        headers.push(("Cc", cc.clone()));
    }
    if is_imap {
        headers.insert(0, ("From", account_id.clone()));
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

    if is_imap {
        crate::mail_account::send_smtp_raw(&app, &account_id, &to, &cc, "", raw_email).await?;
        return Ok(SendOutcome {
            status: "sent",
            message_id: outbound_message_id,
        });
    }

    let access_token = crate::db::load_account_access_token(&account_id)?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|_| "Gmail send client could not be created.".to_string())?;

    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw_email.as_bytes());

    let send_body = serde_json::json!({
        "raw": encoded,
        "threadId": thread_id
    });

    let url = "https://gmail.googleapis.com/gmail/v1/users/me/messages/send";

    let res =
        match execute_send(client.post(url).bearer_auth(&access_token).json(&send_body)).await? {
            SendAttempt::Confirmed(response) => response,
            SendAttempt::OutcomeUnknown => {
                return Ok(SendOutcome {
                    status: "outcome_unknown",
                    message_id: outbound_message_id,
                });
            }
        };

    if !res.status().is_success() {
        return Err(format!("Gmail send error (HTTP {}).", res.status()));
    }

    let cache_result = match res.json::<serde_json::Value>().await {
        Ok(sent_message) => match sent_message["id"].as_str() {
            Some(sent_id) => {
                cache_confirmed_sent_message(&app, &account_id, &client, &access_token, sent_id)
                    .await
            }
            None => Err("Gmail send response did not include a message ID.".to_string()),
        },
        Err(_) => Err("Gmail send response could not be read.".to_string()),
    };
    if let Err(error) = cache_result {
        eprintln!("[SEND_CACHE] confirmed reply could not be cached: {error}");
        // The remote send is already confirmed. Reconcile the local Sent cache
        // without turning this into a retryable send failure.
        if let Err(sync_error) = sync_emails(window, app, account_id, Some(true)).await {
            eprintln!("[SEND_CACHE] reply cache reconciliation failed: {sync_error}");
        }
    }

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
    let is_imap = crate::db::get_account_provider(&app, &account_id)? == "imap";
    let atts = attachments.unwrap_or_default();
    validate_recipient_header(&to)?;
    validate_optional_recipient_header(&cc)?;
    validate_optional_recipient_header(&bcc)?;
    validate_header_value("Subject", &subject, MAX_SUBJECT_BYTES)?;
    let outbound_message_id = generate_outbound_message_id();

    let mut headers = vec![("To", to.clone())];
    if !cc.trim().is_empty() {
        headers.push(("Cc", cc.clone()));
    }
    if !is_imap && !bcc.trim().is_empty() {
        headers.push(("Bcc", bcc.clone()));
    }
    if is_imap {
        headers.insert(0, ("From", account_id.clone()));
    }
    headers.push(("Subject", mime_encode_header(&subject)));
    headers.push(("Message-ID", outbound_message_id.clone()));
    let raw_email = build_raw_mime(&headers, &body, &atts)?;

    if is_imap {
        crate::mail_account::send_smtp_raw(&app, &account_id, &to, &cc, &bcc, raw_email).await?;
        return Ok(SendOutcome {
            status: "sent",
            message_id: outbound_message_id,
        });
    }

    let access_token = crate::db::load_account_access_token(&account_id)?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|_| "Gmail send client could not be created.".to_string())?;

    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw_email.as_bytes());

    let send_body = serde_json::json!({
        "raw": encoded
    });

    let url = "https://gmail.googleapis.com/gmail/v1/users/me/messages/send";

    let res =
        match execute_send(client.post(url).bearer_auth(&access_token).json(&send_body)).await? {
            SendAttempt::Confirmed(response) => response,
            SendAttempt::OutcomeUnknown => {
                return Ok(SendOutcome {
                    status: "outcome_unknown",
                    message_id: outbound_message_id,
                });
            }
        };

    if !res.status().is_success() {
        return Err(format!("Gmail send error (HTTP {}).", res.status()));
    }

    let cache_result = match res.json::<serde_json::Value>().await {
        Ok(sent_message) => match sent_message["id"].as_str() {
            Some(sent_id) => {
                cache_confirmed_sent_message(&app, &account_id, &client, &access_token, sent_id)
                    .await
            }
            None => Err("Gmail send response did not include a message ID.".to_string()),
        },
        Err(_) => Err("Gmail send response could not be read.".to_string()),
    };
    if let Err(error) = cache_result {
        eprintln!("[SEND_CACHE] confirmed message could not be cached: {error}");
        if let Err(sync_error) = sync_emails(window, app, account_id, Some(true)).await {
            eprintln!("[SEND_CACHE] sent cache reconciliation failed: {sync_error}");
        }
    }

    Ok(SendOutcome {
        status: "sent",
        message_id: outbound_message_id,
    })
}

fn draft_header(detail: &MessageDetail, name: &str) -> String {
    detail
        .payload
        .headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.clone())
        .unwrap_or_default()
}

fn draft_body(detail: &MessageDetail) -> String {
    if let Some(parts) = &detail.payload.parts {
        if let Some(data) = find_part_data(parts, "text/html") {
            return decode_base64_url(data);
        }
        if let Some(data) = find_part_data(parts, "text/plain") {
            return decode_base64_url(data);
        }
    }
    detail
        .payload
        .body
        .as_ref()
        .and_then(|body| body.data.as_deref())
        .map(decode_base64_url)
        .unwrap_or_default()
}

fn draft_summary(draft: &GmailDraft) -> DraftSummary {
    DraftSummary {
        id: draft.id.clone(),
        message_id: draft.message.id.clone(),
        rfc_message_id: draft_header(&draft.message, "message-id"),
        thread_id: draft.message.thread_id.clone().unwrap_or_default(),
        in_reply_to: draft_header(&draft.message, "in-reply-to"),
        references: draft_header(&draft.message, "references"),
        to: draft_header(&draft.message, "to"),
        cc: draft_header(&draft.message, "cc"),
        bcc: draft_header(&draft.message, "bcc"),
        subject: draft_header(&draft.message, "subject"),
        snippet: draft.message.snippet.clone(),
        updated_at: draft.message.internal_date.parse::<i64>().unwrap_or(0),
    }
}

async fn fetch_draft_attachments(
    client: &Client,
    access_token: &str,
    message: &MessageDetail,
) -> Result<Vec<AttachmentPayload>, String> {
    let attachments = message
        .payload
        .parts
        .as_deref()
        .map(|parts| collect_attachments(parts, &message.id, ""))
        .unwrap_or_default();
    let mut result = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        let url_safe_data = if let Some(data) = attachment.data.filter(|data| !data.is_empty()) {
            data
        } else if let Some(attachment_id) = attachment.attachment_id {
            validate_gmail_identifier("attachment ID", &attachment_id)?;
            let url = format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages/{}/attachments/{}",
                message.id, attachment_id
            );
            let response = gmail_get_with_retry(
                client,
                access_token,
                url,
                "Draft attachment could not be loaded",
            )
            .await?;
            if !response.status().is_success() {
                return Err(format!(
                    "Draft attachment could not be loaded (HTTP {}).",
                    response.status()
                ));
            }
            #[derive(Deserialize)]
            struct AttachmentResponse {
                data: String,
            }
            response
                .json::<AttachmentResponse>()
                .await
                .map_err(|_| "Draft attachment response could not be read.".to_string())?
                .data
        } else {
            continue;
        };
        let bytes = base64::engine::general_purpose::URL_SAFE
            .decode(url_safe_data.replace(['\n', '\r'], "").as_bytes())
            .map_err(|_| "Draft attachment data is invalid.".to_string())?;
        result.push(AttachmentPayload {
            filename: attachment.filename,
            mime_type: attachment.mime_type,
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
        });
    }
    Ok(result)
}

async fn reconcile_created_draft(
    client: &Client,
    access_token: &str,
    verification_message_id: &str,
) -> Option<(String, String)> {
    for delay_ms in [200_u64, 600, 1_200] {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        let mut url =
            reqwest::Url::parse("https://gmail.googleapis.com/gmail/v1/users/me/drafts").ok()?;
        url.query_pairs_mut()
            .append_pair("maxResults", "1")
            .append_pair("q", &format!("rfc822msgid:{verification_message_id}"));
        let Ok(response) = gmail_get_with_retry(
            client,
            access_token,
            url.to_string(),
            "Draft reconciliation failed",
        )
        .await
        else {
            continue;
        };
        if !response.status().is_success() {
            continue;
        }
        let Ok(list) = response.json::<DraftListResponse>().await else {
            continue;
        };
        if let Some(draft) = list.drafts.unwrap_or_default().into_iter().next() {
            return Some((
                draft.id,
                draft.message.map(|message| message.id).unwrap_or_default(),
            ));
        }
    }
    None
}

#[tauri::command]
pub async fn list_drafts(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    account_id: String,
    page_token: Option<String>,
) -> Result<DraftPage, String> {
    crate::require_command_window(&window, &["main"])?;
    if crate::db::get_account_provider(&app, &account_id)? == "imap" {
        return crate::mail_account::list_imap_drafts(&app, &account_id).await;
    }
    let access_token = crate::db::load_account_access_token(&account_id)?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|_| "Gmail draft client could not be created.".to_string())?;
    let mut url = reqwest::Url::parse("https://gmail.googleapis.com/gmail/v1/users/me/drafts")
        .map_err(|_| "Draft list URL could not be created.".to_string())?;
    url.query_pairs_mut().append_pair("maxResults", "20");
    if let Some(token) = page_token.filter(|token| !token.is_empty()) {
        if token.len() > 4_096 || token.chars().any(char::is_control) {
            return Err("Invalid draft page token".to_string());
        }
        url.query_pairs_mut().append_pair("pageToken", &token);
    }
    let response =
        gmail_get_with_retry(&client, &access_token, url.to_string(), "Draft list failed").await?;
    if !response.status().is_success() {
        return Err(format!("Draft list failed (HTTP {}).", response.status()));
    }
    let list = response
        .json::<DraftListResponse>()
        .await
        .map_err(|_| "Draft list response could not be read.".to_string())?;
    let draft_refs = list.drafts.unwrap_or_default();
    let mut summaries = stream::iter(draft_refs)
        .map(|draft_ref| {
            let client = client.clone();
            let access_token = access_token.clone();
            async move {
                validate_gmail_identifier("draft ID", &draft_ref.id)?;
                let url = format!(
                    "https://gmail.googleapis.com/gmail/v1/users/me/drafts/{}?format=metadata&metadataHeaders=To&metadataHeaders=Cc&metadataHeaders=Bcc&metadataHeaders=Subject&metadataHeaders=Message-ID&metadataHeaders=In-Reply-To&metadataHeaders=References",
                    draft_ref.id
                );
                let response = gmail_get_with_retry(&client, &access_token, url, "Draft could not be loaded").await?;
                if !response.status().is_success() {
                    return Err(format!("Draft could not be loaded (HTTP {}).", response.status()));
                }
                let draft = response
                    .json::<GmailDraft>()
                    .await
                    .map_err(|_| "Draft response could not be read.".to_string())?;
                Ok::<DraftSummary, String>(draft_summary(&draft))
            }
        })
        .buffer_unordered(4)
        .try_collect::<Vec<_>>()
        .await?;
    summaries.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Ok(DraftPage {
        drafts: summaries,
        next_page_token: list.next_page_token,
    })
}

#[tauri::command]
pub async fn get_draft(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    account_id: String,
    draft_id: String,
) -> Result<DraftContent, String> {
    crate::require_command_window(&window, &["main"])?;
    if crate::db::get_account_provider(&app, &account_id)? == "imap" {
        return crate::mail_account::get_imap_draft(&app, &account_id, &draft_id).await;
    }
    validate_gmail_identifier("draft ID", &draft_id)?;
    let access_token = crate::db::load_account_access_token(&account_id)?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|_| "Gmail draft client could not be created.".to_string())?;
    let url = format!(
        "https://gmail.googleapis.com/gmail/v1/users/me/drafts/{}?format=full",
        draft_id
    );
    let response =
        gmail_get_with_retry(&client, &access_token, url, "Draft could not be loaded").await?;
    if !response.status().is_success() {
        return Err(format!(
            "Draft could not be loaded (HTTP {}).",
            response.status()
        ));
    }
    let draft = response
        .json::<GmailDraft>()
        .await
        .map_err(|_| "Draft response could not be read.".to_string())?;
    let attachments = fetch_draft_attachments(&client, &access_token, &draft.message).await?;
    Ok(DraftContent {
        id: draft.id,
        message_id: draft.message.id.clone(),
        rfc_message_id: draft_header(&draft.message, "message-id"),
        thread_id: draft.message.thread_id.clone().unwrap_or_default(),
        in_reply_to: draft_header(&draft.message, "in-reply-to"),
        references: draft_header(&draft.message, "references"),
        to: draft_header(&draft.message, "to"),
        cc: draft_header(&draft.message, "cc"),
        bcc: draft_header(&draft.message, "bcc"),
        subject: draft_header(&draft.message, "subject"),
        body: draft_body(&draft.message),
        updated_at: draft.message.internal_date.parse::<i64>().unwrap_or(0),
        attachments,
    })
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
    let is_imap = crate::db::get_account_provider(&app, &account_id)? == "imap";
    // Drafts deliberately accept incomplete addresses. They are validated
    // strictly only when the user sends the message.
    validate_draft_recipient_header("To", &to)?;
    validate_draft_recipient_header("Cc", &cc)?;
    validate_draft_recipient_header("Bcc", &bcc)?;
    validate_header_value("Subject", &subject, MAX_SUBJECT_BYTES)?;
    if !is_imap {
        if let Some(id) = &draft_id {
            validate_gmail_identifier("draft ID", id)?;
        }
    }
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
    if is_imap {
        return crate::mail_account::save_imap_draft(
            &app,
            &account_id,
            draft_id.as_deref(),
            raw_email,
            verification_message_id,
        )
        .await;
    }
    let access_token = crate::db::load_account_access_token(&account_id)?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|_| "Gmail draft client could not be created.".to_string())?;
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw_email.as_bytes());
    let mut message = serde_json::json!({ "raw": encoded });
    if let Some(thread_id) = thread_id.filter(|value| !value.trim().is_empty()) {
        validate_gmail_identifier("thread ID", &thread_id)?;
        message["threadId"] = serde_json::Value::String(thread_id);
    }
    let payload = serde_json::json!({ "message": message });
    let existing_draft_id = draft_id.clone();
    let request = if let Some(id) = draft_id {
        client
            .put(format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/drafts/{id}"
            ))
            .bearer_auth(&access_token)
            .json(&payload)
    } else {
        client
            .post("https://gmail.googleapis.com/gmail/v1/users/me/drafts")
            .bearer_auth(&access_token)
            .json(&payload)
    };
    let response = match request.send().await {
        Ok(response) => response,
        Err(_) if existing_draft_id.is_none() => {
            if let Some((id, message_id)) =
                reconcile_created_draft(&client, &access_token, &verification_message_id).await
            {
                return Ok(SavedDraft {
                    id,
                    message_id,
                    verification_message_id,
                    updated_at: unix_timestamp_secs() * 1000,
                });
            }
            return Err(
                "Draft save outcome is unknown. Refresh drafts before retrying.".to_string(),
            );
        }
        Err(_) => {
            return Err(
                "Draft save outcome is unknown. Refresh drafts before retrying.".to_string(),
            );
        }
    };
    if !response.status().is_success() {
        return Err(format!(
            "Draft could not be saved (HTTP {}).",
            response.status()
        ));
    }
    let saved = match response.json::<SavedGmailDraft>().await {
        Ok(saved) => saved,
        Err(_) => {
            if let Some(id) = existing_draft_id {
                return Ok(SavedDraft {
                    id,
                    message_id: String::new(),
                    verification_message_id,
                    updated_at: unix_timestamp_secs() * 1000,
                });
            }
            if let Some((id, message_id)) =
                reconcile_created_draft(&client, &access_token, &verification_message_id).await
            {
                return Ok(SavedDraft {
                    id,
                    message_id,
                    verification_message_id,
                    updated_at: unix_timestamp_secs() * 1000,
                });
            }
            return Err(
                "Draft save outcome is unknown. Refresh drafts before retrying.".to_string(),
            );
        }
    };
    Ok(SavedDraft {
        id: saved.id,
        message_id: saved.message.id,
        verification_message_id,
        updated_at: unix_timestamp_secs() * 1000,
    })
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
    if crate::db::get_account_provider(&app, &account_id)? == "imap" {
        validate_outbound_message_id(&verification_message_id)?;
        crate::mail_account::send_imap_draft(&app, &account_id, &draft_id).await?;
        return Ok(SendOutcome {
            status: "sent",
            message_id: verification_message_id,
        });
    }
    validate_gmail_identifier("draft ID", &draft_id)?;
    validate_outbound_message_id(&verification_message_id)?;
    let access_token = crate::db::load_account_access_token(&account_id)?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|_| "Gmail draft send client could not be created.".to_string())?;
    let response = match execute_send(
        client
            .post("https://gmail.googleapis.com/gmail/v1/users/me/drafts/send")
            .bearer_auth(&access_token)
            .json(&serde_json::json!({ "id": draft_id })),
    )
    .await?
    {
        SendAttempt::Confirmed(response) => response,
        SendAttempt::OutcomeUnknown => {
            return Ok(SendOutcome {
                status: "outcome_unknown",
                message_id: verification_message_id,
            });
        }
    };
    if !response.status().is_success() {
        return Err(format!(
            "Gmail draft send error (HTTP {}).",
            response.status()
        ));
    }
    let cache_result = match response.json::<serde_json::Value>().await {
        Ok(sent_message) => match sent_message["id"].as_str() {
            Some(sent_id) => {
                cache_confirmed_sent_message(&app, &account_id, &client, &access_token, sent_id)
                    .await
            }
            None => Err("Gmail draft send response did not include a message ID.".to_string()),
        },
        Err(_) => Err("Gmail draft send response could not be read.".to_string()),
    };
    if let Err(error) = cache_result {
        eprintln!("[SEND_CACHE] confirmed draft could not be cached: {error}");
        if let Err(sync_error) = sync_emails(window, app, account_id, Some(true)).await {
            eprintln!("[SEND_CACHE] draft cache reconciliation failed: {sync_error}");
        }
    }
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
    if crate::db::get_account_provider(&app, &account_id)? == "imap" {
        return crate::mail_account::delete_imap_draft(&app, &account_id, &draft_id).await;
    }
    validate_gmail_identifier("draft ID", &draft_id)?;
    let access_token = crate::db::load_account_access_token(&account_id)?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|_| "Gmail draft client could not be created.".to_string())?;
    let response = client
        .delete(format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/drafts/{draft_id}"
        ))
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|_| {
            "Draft delete outcome is unknown. Refresh drafts before retrying.".to_string()
        })?;
    if !response.status().is_success() {
        return Err(format!(
            "Draft could not be deleted (HTTP {}).",
            response.status()
        ));
    }
    Ok(())
}

#[tauri::command]
pub async fn verify_sent_message(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    account_id: String,
    message_id: String,
) -> Result<bool, String> {
    crate::require_command_window(&window, &["main"])?;
    validate_outbound_message_id(&message_id)?;
    let access_token = crate::db::load_account_access_token(&account_id)?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|_| "Gmail verification client could not be created.".to_string())?;
    let mut url = reqwest::Url::parse("https://gmail.googleapis.com/gmail/v1/users/me/messages")
        .map_err(|_| "Gmail verification URL could not be created.".to_string())?;
    url.query_pairs_mut()
        .append_pair("labelIds", "SENT")
        .append_pair("maxResults", "1")
        .append_pair("q", &format!("rfc822msgid:{message_id}"));

    let response = gmail_get_with_retry(
        &client,
        &access_token,
        url.to_string(),
        "Sent-message verification failed",
    )
    .await?;
    if !response.status().is_success() {
        return Err(format!(
            "Sent-message verification failed (HTTP {}).",
            response.status()
        ));
    }
    let result: MessageListResponse = response
        .json()
        .await
        .map_err(|_| "Sent-message verification response could not be read.".to_string())?;
    let Some(gmail_message_id) = result
        .messages
        .and_then(|messages| messages.into_iter().next())
        .map(|message| message.id)
    else {
        return Ok(false);
    };

    if let Err(error) =
        cache_confirmed_sent_message(&app, &account_id, &client, &access_token, &gmail_message_id)
            .await
    {
        eprintln!("[SEND_CACHE] verified sent message could not be cached: {error}");
        if let Err(sync_error) = sync_emails(window, app, account_id, Some(true)).await {
            eprintln!("[SEND_CACHE] verified message reconciliation failed: {sync_error}");
        }
    }
    Ok(true)
}

#[tauri::command]
pub async fn mark_as_read(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    message_id: String,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    if crate::db::get_account_provider(&app, &account_id)? == "imap" {
        crate::mail_account::set_imap_message_flag(
            &app,
            &account_id,
            &message_id,
            false,
            imap_rs::client::Flag::Seen,
            true,
        )
        .await?;
        return crate::db::mark_email_as_read_local(&app, &message_id, &account_id);
    }
    let access_token = crate::db::load_account_access_token(&account_id)?;
    validate_gmail_identifier("message ID", &message_id)?;
    // Notify Gmail before changing the local cache.
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    let url = format!(
        "https://gmail.googleapis.com/gmail/v1/users/me/messages/{}/modify",
        message_id
    );
    let body = serde_json::json!({
        "removeLabelIds": ["UNREAD"]
    });

    let res = client
        .post(&url)
        .bearer_auth(&access_token)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !res.status().is_success() {
        return Err(format!("Gmail mark as read error (HTTP {}).", res.status()));
    }

    if let Err(error) = crate::db::mark_email_as_read_local(&app, &message_id, &account_id) {
        if let Err(sync_error) = sync_emails(window, app, account_id, Some(true)).await {
            eprintln!("[MAIL_CACHE] read-state reconciliation failed: {sync_error}");
        }
        return Err(format!(
            "Gmail okundu durumunu değiştirdi ancak yerel önbellek güncellenemedi: {error}"
        ));
    }

    Ok(())
}

#[tauri::command]
pub async fn mark_thread_as_read(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    thread_id: String,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    if crate::db::get_account_provider(&app, &account_id)? == "imap" {
        crate::mail_account::set_imap_message_flag(
            &app,
            &account_id,
            &thread_id,
            true,
            imap_rs::client::Flag::Seen,
            true,
        )
        .await?;
        return crate::db::mark_thread_as_read_local(&app, &thread_id, &account_id);
    }
    let access_token = crate::db::load_account_access_token(&account_id)?;
    validate_gmail_identifier("thread ID", &thread_id)?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    let url = format!(
        "https://gmail.googleapis.com/gmail/v1/users/me/threads/{}/modify",
        thread_id
    );
    let body = serde_json::json!({
        "removeLabelIds": ["UNREAD"]
    });

    let res = client
        .post(&url)
        .bearer_auth(&access_token)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !res.status().is_success() {
        return Err(format!(
            "Gmail mark thread as read error (HTTP {}).",
            res.status()
        ));
    }

    if let Err(error) = crate::db::mark_thread_as_read_local(&app, &thread_id, &account_id) {
        if let Err(sync_error) = sync_emails(window, app, account_id, Some(true)).await {
            eprintln!("[MAIL_CACHE] thread read-state reconciliation failed: {sync_error}");
        }
        return Err(format!(
            "Gmail konuşmayı okundu yaptı ancak yerel önbellek güncellenemedi: {error}"
        ));
    }

    Ok(())
}

#[tauri::command]
pub async fn mark_as_unread(
    window: tauri::WebviewWindow,
    app: AppHandle,
    account_id: String,
    thread_id: String,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    if crate::db::get_account_provider(&app, &account_id)? == "imap" {
        crate::mail_account::set_imap_message_flag(
            &app,
            &account_id,
            &thread_id,
            true,
            imap_rs::client::Flag::Seen,
            false,
        )
        .await?;
        return crate::db::mark_thread_as_unread_local(&app, &thread_id, &account_id);
    }
    let access_token = crate::db::load_account_access_token(&account_id)?;
    validate_gmail_identifier("thread ID", &thread_id)?;
    // Notify Gmail before changing the local cache.
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    let url = format!(
        "https://gmail.googleapis.com/gmail/v1/users/me/threads/{}/modify",
        thread_id
    );
    let body = serde_json::json!({
        "addLabelIds": ["UNREAD"]
    });

    let res = client
        .post(&url)
        .bearer_auth(&access_token)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !res.status().is_success() {
        return Err(format!(
            "Gmail mark as unread error (HTTP {}).",
            res.status()
        ));
    }

    if let Err(error) = crate::db::mark_thread_as_unread_local(&app, &thread_id, &account_id) {
        if let Err(sync_error) = sync_emails(window, app, account_id, Some(true)).await {
            eprintln!("[MAIL_CACHE] unread-state reconciliation failed: {sync_error}");
        }
        return Err(format!(
            "Gmail okunmadı durumunu değiştirdi ancak yerel önbellek güncellenemedi: {error}"
        ));
    }

    Ok(())
}

fn is_inline_part(part: &MessagePart) -> bool {
    part.headers.as_ref().map_or(false, |hdrs| {
        hdrs.iter().any(|h| {
            h.name.eq_ignore_ascii_case("Content-Disposition")
                && h.value.to_lowercase().starts_with("inline")
        })
    })
}

fn collect_attachments(
    parts: &[MessagePart],
    email_id: &str,
    account_id: &str,
) -> Vec<crate::db::Attachment> {
    let mut result = Vec::new();
    for part in parts {
        let filename = part.filename.as_deref().unwrap_or("").trim().to_string();
        if !filename.is_empty() && !is_inline_part(part) {
            if let Some(body) = &part.body {
                let size = body.size.unwrap_or(0);
                let part_key = part.part_id.as_deref().unwrap_or(&filename);
                let id = format!("{}_{}", email_id, part_key);
                result.push(crate::db::Attachment {
                    id,
                    email_id: email_id.to_string(),
                    account_id: account_id.to_string(),
                    filename,
                    mime_type: part.mime_type.clone(),
                    size,
                    attachment_id: body.attachment_id.clone(),
                    data: body.data.clone(),
                });
            }
        }
        if let Some(subparts) = &part.parts {
            result.extend(collect_attachments(subparts, email_id, account_id));
        }
    }
    result
}

fn collect_inline_images(
    parts: &[MessagePart],
    cids: &mut std::collections::HashMap<String, String>,
) {
    for part in parts {
        if part.mime_type.starts_with("image/") {
            if let Some(headers) = &part.headers {
                let mut content_id = String::new();
                for header in headers {
                    if header.name.eq_ignore_ascii_case("Content-ID") {
                        content_id = header.value.replace("<", "").replace(">", "");
                        break;
                    }
                }

                if !content_id.is_empty() {
                    if let Some(body) = &part.body {
                        if let Some(data) = &body.data {
                            let standard_b64 = data.replace("-", "+").replace("_", "/");
                            let data_uri =
                                format!("data:{};base64,{}", part.mime_type, standard_b64);
                            cids.insert(content_id, data_uri);
                        }
                    }
                }
            }
        }

        if let Some(subparts) = &part.parts {
            collect_inline_images(subparts, cids);
        }
    }
}

fn find_part_data<'a>(parts: &'a [MessagePart], mime_type: &str) -> Option<&'a String> {
    for part in parts {
        if part.mime_type == mime_type {
            if let Some(body) = &part.body {
                if let Some(data) = &body.data {
                    return Some(data);
                }
            }
        }
        if let Some(subparts) = &part.parts {
            if let Some(data) = find_part_data(subparts, mime_type) {
                return Some(data);
            }
        }
    }
    None
}

fn decode_base64_url(data: &str) -> String {
    let engine = base64::engine::general_purpose::URL_SAFE;
    if let Ok(decoded) = engine.decode(data) {
        String::from_utf8(decoded).unwrap_or_else(|_| "Decode Error".to_string())
    } else {
        "Base64 Error".to_string()
    }
}

/// Fetches attachment data (from DB for small files, Gmail API for large ones).
async fn get_attachment_bytes(
    app: &tauri::AppHandle,
    email_id: &str,
    account_id: &str,
    attachment_db_id: &str,
    access_token: &str,
) -> Result<(Vec<u8>, String, String), String> {
    let is_imap = crate::db::get_account_provider(app, account_id)? == "imap";
    if !is_imap {
        validate_gmail_identifier("message ID", email_id)?;
    }
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

    let b64 = if let Some(data) = att.data.filter(|d| !d.is_empty()) {
        data
    } else {
        if is_imap {
            return Err("mail_account_attachment_data_missing".to_string());
        }
        let gmail_att_id = att
            .attachment_id
            .ok_or_else(|| "No attachment ID".to_string())?;
        validate_gmail_identifier("attachment ID", &gmail_att_id)?;
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .unwrap_or_default();
        let url = format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/{}/attachments/{}",
            email_id, gmail_att_id
        );
        let res = gmail_get_with_retry(&client, access_token, url, "Fetch error").await?;
        if !res.status().is_success() {
            let status = res.status();
            return Err(format!("Gmail API error (HTTP {status})"));
        }
        #[derive(serde::Deserialize)]
        struct AttachmentResponse {
            data: String,
        }
        let body: AttachmentResponse = res.json().await.map_err(|e| e.to_string())?;
        body.data
    };

    // Gmail uses URL-safe base64
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
    let access_token = if crate::db::get_account_provider(&app, &account_id)? == "imap" {
        String::new()
    } else {
        crate::db::load_account_access_token(&account_id)?
    };
    let (bytes, filename, _mime) = get_attachment_bytes(
        &app,
        &email_id,
        &account_id,
        &attachment_db_id,
        &access_token,
    )
    .await?;

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

#[cfg(test)]
mod tests {
    use super::{
        build_raw_mime, build_reply_references, determine_label, execute_send,
        generate_outbound_message_id, gmail_get_with_retry, gmail_label_color_allowed,
        gmail_retry_delay, gmail_trash_request, is_retryable_gmail_status, mailbox_failure_status,
        parse_message_detail, parse_retry_after_delay, safe_attachment_filename,
        validate_draft_recipient_header, validate_optional_recipient_header,
        validate_outbound_message_id, validate_recipient_header, window_state_allows_label_sync,
        AttachmentPayload, DraftListResponse, GmailLabelStats, MessageDetail, SendAttempt,
        GMAIL_GET_MAX_RETRY_AFTER_SECS,
    };
    use reqwest::{Client, StatusCode};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn drafts_allow_an_empty_recipient_but_validate_non_empty_values() {
        assert!(validate_optional_recipient_header("").is_ok());
        assert!(validate_optional_recipient_header("person@example.test").is_ok());
        assert!(validate_optional_recipient_header("not-an-address").is_err());
    }

    #[test]
    fn label_colors_are_limited_to_supported_gmail_pairs() {
        assert!(gmail_label_color_allowed("#4a86e8", "#ffffff"));
        assert!(gmail_label_color_allowed("#fad165", "#000000"));
        assert!(!gmail_label_color_allowed("#4a86e8", "#000000"));
        assert!(!gmail_label_color_allowed("#123456", "#ffffff"));
    }

    #[test]
    fn label_catalog_sync_runs_only_for_an_active_window() {
        assert!(window_state_allows_label_sync(
            Some(true),
            Some(false),
            Some(true)
        ));
        assert!(!window_state_allows_label_sync(
            Some(false),
            Some(false),
            Some(true)
        ));
        assert!(!window_state_allows_label_sync(
            Some(true),
            Some(true),
            Some(true)
        ));
        assert!(!window_state_allows_label_sync(
            Some(true),
            Some(false),
            Some(false)
        ));
        assert!(window_state_allows_label_sync(None, None, None));
    }

    #[test]
    fn gmail_drafts_are_not_classified_as_sent_or_archived_messages() {
        assert_eq!(determine_label(&["DRAFT".to_string()]), "draft");
        assert_eq!(
            determine_label(&["DRAFT".to_string(), "SENT".to_string()]),
            "draft"
        );
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
    fn message_parser_preserves_rfc_reply_headers() {
        let detail: MessageDetail = serde_json::from_str(
            r#"{
          "id":"gmail-message", "threadId":"gmail-thread", "snippet":"hello",
          "internalDate":"123", "labelIds":["INBOX","Label_9"],
          "payload":{"headers":[
            {"name":"From","value":"Alice <alice@example.test>"},
            {"name":"To","value":"Me <me@example.test>"},
            {"name":"Reply-To","value":"Help <help@example.test>"},
            {"name":"Message-ID","value":"<child@example.test>"},
            {"name":"References","value":"<root@example.test>"},
            {"name":"Subject","value":"Hello"}
          ],"body":{}}
        }"#,
        )
        .expect("message detail");
        let (email, _) = parse_message_detail(detail);
        assert_eq!(email.reply_to, "Help <help@example.test>");
        assert_eq!(email.message_id, "<child@example.test>");
        assert_eq!(email.references, "<root@example.test>");
        assert_eq!(email.gmail_label_ids, vec!["INBOX", "Label_9"]);
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
    fn draft_list_keeps_pagination_and_message_ids() {
        let page: DraftListResponse = serde_json::from_str(
            r#"{"drafts":[{"id":"draft-1","message":{"id":"message-1"}}],"nextPageToken":"next-page"}"#,
        )
        .expect("parse draft page");

        assert_eq!(page.next_page_token.as_deref(), Some("next-page"));
        let draft = page.drafts.expect("drafts").remove(0);
        assert_eq!(draft.id, "draft-1");
        assert_eq!(draft.message.expect("draft message").id, "message-1");
    }

    #[test]
    fn gmail_trash_post_has_explicit_zero_content_length() {
        let request = gmail_trash_request(
            &Client::new(),
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/test/trash",
            "test-token",
        )
        .build()
        .expect("build trash request");

        assert_eq!(
            request
                .headers()
                .get(reqwest::header::CONTENT_LENGTH)
                .unwrap(),
            "0"
        );
    }

    #[tokio::test]
    async fn gmail_get_retries_a_transient_response_then_succeeds() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake Gmail endpoint");
        let address = listener.local_addr().expect("read fake endpoint address");
        let server = tokio::spawn(async move {
            for status_line in ["HTTP/1.1 503 Service Unavailable", "HTTP/1.1 200 OK"] {
                let (mut socket, _) = listener.accept().await.expect("accept request");
                let mut request = [0_u8; 1024];
                socket.read(&mut request).await.expect("read request");
                let response =
                    format!("{status_line}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                socket
                    .write_all(response.as_bytes())
                    .await
                    .expect("write response");
            }
        });

        let response = gmail_get_with_retry(
            &Client::new(),
            "test-token",
            format!("http://{address}/gmail/v1/users/me/profile"),
            "Fake Gmail fetch error",
        )
        .await
        .expect("retry fake Gmail request");

        assert_eq!(response.status(), StatusCode::OK);
        server.await.expect("finish fake Gmail endpoint");
    }

    #[tokio::test]
    async fn disconnected_send_is_reported_as_outcome_unknown_without_retrying() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake Gmail send endpoint");
        let address = listener.local_addr().expect("read fake endpoint address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept send request");
            let mut request = [0_u8; 2048];
            socket.read(&mut request).await.expect("read send request");
            // Dropping the socket after reading models a connection loss after
            // Gmail may already have accepted the request.
        });

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .expect("build test client");
        let outcome = execute_send(
            client
                .post(format!("http://{address}/messages/send"))
                .body("raw-message"),
        )
        .await
        .expect("classify disconnected send");

        assert!(matches!(outcome, SendAttempt::OutcomeUnknown));
        server.await.expect("finish fake Gmail send endpoint");
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
    fn gmail_get_retries_only_rate_limits_and_server_failures() {
        assert!(is_retryable_gmail_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable_gmail_status(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(is_retryable_gmail_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(!is_retryable_gmail_status(StatusCode::UNAUTHORIZED));
        assert!(!is_retryable_gmail_status(StatusCode::NOT_FOUND));
    }

    #[test]
    fn gmail_get_backoff_is_exponential_with_equal_jitter() {
        let first = gmail_retry_delay(0, None);
        assert!(first.as_millis() >= 200 && first.as_millis() <= 400);

        let second = gmail_retry_delay(1, None);
        assert!(second.as_millis() >= 400 && second.as_millis() <= 800);
    }

    #[test]
    fn gmail_get_honors_and_caps_numeric_retry_after() {
        assert_eq!(parse_retry_after_delay("7").unwrap().as_secs(), 7);
        assert_eq!(
            parse_retry_after_delay("999").unwrap().as_secs(),
            GMAIL_GET_MAX_RETRY_AFTER_SECS
        );
        assert!(parse_retry_after_delay("Wed, 21 Oct 2015 07:28:00 GMT").is_none());

        let delayed = gmail_retry_delay(0, parse_retry_after_delay("7"));
        assert_eq!(delayed.as_secs(), 7);
    }

    #[test]
    fn mailbox_failures_distinguish_rate_limits_and_relogin() {
        let (rate_limited, retry_after) =
            mailbox_failure_status("Gmail API Error: 429 rateLimitExceeded");
        assert_eq!(rate_limited, "rate_limited");
        assert!(retry_after.is_some());

        let (relogin_required, retry_after) =
            mailbox_failure_status("Gmail API Error: 401 unauthenticated");
        assert_eq!(relogin_required, "relogin_required");
        assert_eq!(retry_after, None);

        let (error, retry_after) = mailbox_failure_status("List fetch error: network unavailable");
        assert_eq!(error, "error");
        assert!(retry_after.is_some());
    }

    #[test]
    fn inbox_label_stats_keep_message_and_thread_counts_distinct() {
        let stats: GmailLabelStats =
            serde_json::from_str(r#"{"messagesUnread": 1046, "threadsUnread": 810}"#)
                .expect("parse Gmail inbox label stats");

        assert_eq!(stats.messages_unread, 1046);
        assert_eq!(stats.threads_unread, 810);
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
    let access_token = if crate::db::get_account_provider(&app, &account_id)? == "imap" {
        String::new()
    } else {
        crate::db::load_account_access_token(&account_id)?
    };
    let (bytes, _filename, _mime) = get_attachment_bytes(
        &app,
        &email_id,
        &account_id,
        &attachment_db_id,
        &access_token,
    )
    .await?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&bytes))
}

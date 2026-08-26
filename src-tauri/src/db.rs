use keyring::Entry;
use rusqlite::{params, Connection, InterruptHandle, OptionalExtension, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

// ── Per-account keyring ────────────────────────────────────────────────────────

const KEYRING_SERVICE: &str = "fursoy-mail";

fn database_error(_: rusqlite::Error) -> String {
    "Local database operation failed.".to_string()
}

#[derive(Clone, Serialize, Deserialize)]
pub struct StoredTokens {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub expires_at: Option<i64>,
}

fn account_key(email: &str) -> String {
    format!("oauth-{}", email)
}

fn account_part_key(email: &str, index: usize) -> String {
    format!("oauth-{}-part-{index}", email)
}

/// Windows stores one credential in at most 2560 bytes, and it stores them as
/// UTF-16, so the real ceiling is 1280 characters. Google's tokens fit with
/// room to spare; a single Microsoft access token can be twice the whole
/// budget, which is why saving a Microsoft session failed *after* the mailbox
/// had already signed in — the sign-in worked and the account was never
/// created. Anything too long is written as numbered parts, each under the
/// limit, and the main entry then holds only their count.
const MAX_CREDENTIAL_CHARS: usize = 1000;
/// The most parts one session may be split into. Ten times the limit is far
/// more than any provider issues, and it bounds what a delete has to sweep.
const MAX_CREDENTIAL_PARTS: usize = 10;

#[derive(Serialize, Deserialize)]
struct StoredTokenParts {
    parts: usize,
}

fn split_credential(data: &str, limit: usize) -> Vec<String> {
    data.chars()
        .collect::<Vec<char>>()
        .chunks(limit)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

fn write_credential(key: &str, value: &str) -> Result<(), String> {
    Entry::new(KEYRING_SERVICE, key)
        .and_then(|entry| entry.set_password(value))
        .map_err(|error| format!("Token could not be saved: {error}"))
}

fn read_credential(key: &str) -> Option<String> {
    Entry::new(KEYRING_SERVICE, key).ok()?.get_password().ok()
}

fn delete_credential(key: &str) -> Result<(), String> {
    let entry = Entry::new(KEYRING_SERVICE, key)
        .map_err(|error| format!("Session credential could not be opened: {error}"))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(format!("Session credential could not be removed: {error}")),
    }
}

pub fn save_tokens(email: &str, tokens: &StoredTokens) -> Result<(), String> {
    let data =
        serde_json::to_string(tokens).map_err(|e| format!("Token could not be serialized: {e}"))?;
    if data.chars().count() <= MAX_CREDENTIAL_CHARS {
        write_credential(&account_key(email), &data)?;
        clear_token_parts(email, 0);
        return Ok(());
    }
    let parts = split_credential(&data, MAX_CREDENTIAL_CHARS);
    if parts.len() > MAX_CREDENTIAL_PARTS {
        return Err("Token could not be saved: it is longer than this app stores.".to_string());
    }
    // The parts go in before the entry that points at them, so an interrupted
    // save never leaves a count referring to something that was never written.
    for (index, part) in parts.iter().enumerate() {
        write_credential(&account_part_key(email, index), part)?;
    }
    let header = serde_json::to_string(&StoredTokenParts { parts: parts.len() })
        .map_err(|e| format!("Token could not be serialized: {e}"))?;
    write_credential(&account_key(email), &header)?;
    clear_token_parts(email, parts.len());
    Ok(())
}

/// Removes the parts of an older, longer session that the current one no longer
/// uses. A leftover part is never read — the count decides that — so failing to
/// remove one is not worth failing a save that already succeeded.
fn clear_token_parts(email: &str, keep: usize) {
    for index in keep..MAX_CREDENTIAL_PARTS {
        let _ = delete_credential(&account_part_key(email, index));
    }
}

pub fn load_tokens(email: &str) -> Option<StoredTokens> {
    let stored = read_credential(&account_key(email))?;
    let json = match serde_json::from_str::<StoredTokens>(&stored) {
        Ok(tokens) => {
            return (!tokens.access_token.is_empty()).then_some(tokens);
        }
        Err(_) => {
            let header: StoredTokenParts = serde_json::from_str(&stored).ok()?;
            let mut data = String::new();
            for index in 0..header.parts.min(MAX_CREDENTIAL_PARTS) {
                data.push_str(&read_credential(&account_part_key(email, index))?);
            }
            data
        }
    };
    let tokens: StoredTokens = serde_json::from_str(&json).ok()?;
    if tokens.access_token.is_empty() {
        return None;
    }
    Some(tokens)
}

/// Records that the stored refresh token was rejected for good. The account
/// keeps its entry, so it stays an OAuth account the user can sign in to again,
/// but the credential is emptied and the access token is dated out: nothing
/// tries to use it, and nothing spends a round trip renewing it.
pub fn mark_oauth_session_revoked(email: &str) -> Result<(), String> {
    let Some(tokens) = load_tokens(email) else {
        return Ok(());
    };
    save_tokens(
        email,
        &StoredTokens {
            access_token: tokens.access_token,
            refresh_token: String::new(),
            expires_at: Some(0),
        },
    )
}

pub fn delete_tokens(email: &str) -> Result<(), String> {
    clear_token_parts(email, 0);
    delete_credential(&account_key(email))
}

/// How early a stored access token is treated as gone. It has to cover the
/// whole operation the token is handed to — an IMAP session runs for minutes —
/// plus any drift between this machine's clock and the token endpoint's.
const ACCESS_TOKEN_EXPIRY_SKEW_SECS: i64 = 300;

pub fn load_account_access_token(account_id: &str) -> Result<String, String> {
    let tokens = load_tokens(account_id)
        .ok_or_else(|| "No session found. Please sign in again.".to_string())?;
    if let Some(expires_at) = tokens.expires_at {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "System clock is invalid.".to_string())?
            .as_secs() as i64;
        if expires_at <= now.saturating_add(ACCESS_TOKEN_EXPIRY_SKEW_SECS) {
            return Err("401: Mail account session expired.".to_string());
        }
    }
    Ok(tokens.access_token)
}

// Legacy single-account keyring (for one-time migration)
fn load_legacy_tokens() -> Option<(String, String)> {
    let json = Entry::new(KEYRING_SERVICE, "oauth-tokens")
        .ok()?
        .get_password()
        .ok()?;
    let val: serde_json::Value = serde_json::from_str(&json).ok()?;
    let access = val["access_token"].as_str()?.to_string();
    let refresh = val["refresh_token"].as_str()?.to_string();
    if access.is_empty() {
        return None;
    }
    Some((access, refresh))
}

fn delete_legacy_tokens() {
    if let Ok(entry) = Entry::new(KEYRING_SERVICE, "oauth-tokens") {
        let _ = entry.delete_credential();
    }
}

// ── Structs ────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Account {
    pub id: String, // same as email
    pub email: String,
    pub picture: String,
    pub display_order: i32,
    pub provider: String,
}

#[derive(Debug, Clone)]
pub struct ImapAccountSettings {
    pub account_id: String,
    pub username: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_security: String,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_security: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Attachment {
    pub id: String,
    pub email_id: String,
    pub account_id: String,
    pub filename: String,
    pub mime_type: String,
    pub size: i64,
    pub attachment_id: Option<String>, // Gmail attachment ID for on-demand fetch
    pub data: Option<String>,          // base64 data for small inline attachments
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Email {
    pub id: String,
    pub thread_id: String,
    pub sender: String,
    pub recipient: String,
    pub cc: String,
    pub reply_to: String,
    pub message_id: String,
    pub references: String,
    pub subject: String,
    pub snippet: String,
    pub body_html: String,
    pub date: i64,
    pub unread: bool,
    pub label: String,
    #[serde(default)]
    pub gmail_label_ids: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct GmailLabel {
    pub id: String,
    pub account_id: String,
    pub name: String,
    pub background_color: Option<String>,
    pub text_color: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EmailSummary {
    pub id: String,
    pub thread_id: String,
    pub sender: String,
    pub recipient: String,
    pub cc: String,
    pub reply_to: String,
    pub message_id: String,
    pub references: String,
    pub subject: String,
    pub snippet: String,
    pub date: i64,
    pub unread: bool,
    pub label: String,
    pub account_id: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ThreadGroup {
    pub latest_email: EmailSummary,
    pub has_unread: bool,
    pub unread_count: u32,
    pub count: u32,
    pub participants: Vec<String>,
    pub label_ids: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AdvancedSearchCriteria {
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub includes: String,
    #[serde(default)]
    pub excludes: String,
    pub after_date: Option<i64>,
    pub before_date: Option<i64>,
    #[serde(default = "default_search_location")]
    pub location: String,
    #[serde(default)]
    pub has_attachment: bool,
    #[serde(default)]
    pub unread: bool,
    #[serde(default)]
    pub starred: bool,
}

fn default_search_location() -> String {
    "all".to_string()
}

impl Default for AdvancedSearchCriteria {
    fn default() -> Self {
        Self {
            from: String::new(),
            to: String::new(),
            subject: String::new(),
            includes: String::new(),
            excludes: String::new(),
            after_date: None,
            before_date: None,
            location: default_search_location(),
            has_attachment: false,
            unread: false,
            starred: false,
        }
    }
}

impl AdvancedSearchCriteria {
    fn is_active(&self) -> bool {
        !self.from.trim().is_empty()
            || !self.to.trim().is_empty()
            || !self.subject.trim().is_empty()
            || !self.includes.trim().is_empty()
            || !self.excludes.trim().is_empty()
            || self.after_date.is_some()
            || self.before_date.is_some()
            || self.location != "all"
            || self.has_attachment
            || self.unread
            || self.starred
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AuthInfo {
    pub authenticated: bool,
    pub expires_at: Option<i64>,
    pub email: String,
    pub picture: String,
}

/// Counts local cache rows which cannot safely be assigned to an account.
/// This intentionally returns no message or attachment content.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct OrphanedCacheCounts {
    pub emails: i64,
    pub inbox_unread: i64,
    pub attachments: i64,
}

// ── DB path ────────────────────────────────────────────────────────────────────

pub fn get_db_path(app: &AppHandle) -> std::path::PathBuf {
    let app_dir = app.path().app_data_dir().unwrap();
    if !app_dir.exists() {
        fs::create_dir_all(&app_dir).unwrap();
    }
    app_dir.join("mailapp.db")
}

#[derive(Clone, Default)]
pub struct SearchCoordinator {
    inner: Arc<SearchCoordinatorInner>,
}

#[derive(Default)]
struct SearchCoordinatorInner {
    generation: AtomicU64,
    active: Mutex<Option<(u64, InterruptHandle)>>,
}

impl SearchCoordinator {
    fn reserve(&self) -> u64 {
        let generation = self.inner.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let previous = self
            .inner
            .active
            .lock()
            .ok()
            .and_then(|mut active| active.take());
        if let Some((_, handle)) = previous {
            handle.interrupt();
        }
        generation
    }

    fn register(&self, generation: u64, handle: InterruptHandle) -> bool {
        let Ok(mut active) = self.inner.active.lock() else {
            handle.interrupt();
            return false;
        };
        if self.inner.generation.load(Ordering::SeqCst) != generation {
            handle.interrupt();
            return false;
        }
        *active = Some((generation, handle));
        true
    }

    fn finish(&self, generation: u64) {
        if let Ok(mut active) = self.inner.active.lock() {
            if active
                .as_ref()
                .map(|(active_generation, _)| *active_generation)
                == Some(generation)
            {
                active.take();
            }
        }
    }

    fn cancel(&self) {
        self.inner.generation.fetch_add(1, Ordering::SeqCst);
        let active = self
            .inner
            .active
            .lock()
            .ok()
            .and_then(|mut active| active.take());
        if let Some((_, handle)) = active {
            handle.interrupt();
        }
    }
}

fn has_account_scoped_primary_key(conn: &Connection, table: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let keys: std::collections::HashMap<String, i64> = stmt
        .query_map([], |row| Ok((row.get(1)?, row.get(5)?)))?
        .filter_map(Result::ok)
        .collect();
    Ok(keys.get("account_id") == Some(&1) && keys.get("id") == Some(&2))
}

fn migrate_account_scoped_primary_keys(conn: &Connection) -> Result<()> {
    if !has_account_scoped_primary_key(conn, "emails")? {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE emails_rebuilt (
               id TEXT NOT NULL, thread_id TEXT NOT NULL DEFAULT '',
               thread_key TEXT NOT NULL DEFAULT '', sender TEXT NOT NULL,
               recipient TEXT NOT NULL DEFAULT '', cc TEXT NOT NULL DEFAULT '',
               reply_to TEXT NOT NULL DEFAULT '', message_id TEXT NOT NULL DEFAULT '',
               references_header TEXT NOT NULL DEFAULT '', subject TEXT NOT NULL,
               snippet TEXT NOT NULL, body_html TEXT NOT NULL, body_text TEXT, date INTEGER NOT NULL,
               unread BOOLEAN NOT NULL, label TEXT NOT NULL DEFAULT 'inbox', account_id TEXT NOT NULL DEFAULT '',
               sync_generation INTEGER NOT NULL DEFAULT 0,
               PRIMARY KEY (account_id, id)
             );
             INSERT INTO emails_rebuilt SELECT id, thread_id,
               CASE WHEN thread_key = '' THEN CASE WHEN thread_id = '' THEN id ELSE thread_id END ELSE thread_key END,
               sender, recipient, cc, reply_to, message_id, references_header, subject, snippet, body_html, body_text, date, unread, label, account_id, sync_generation FROM emails;
             DROP TABLE emails;
             ALTER TABLE emails_rebuilt RENAME TO emails;
             COMMIT;",
        )?;
    }
    if !has_account_scoped_primary_key(conn, "attachments")? {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE attachments_rebuilt (
               id TEXT NOT NULL, email_id TEXT NOT NULL, account_id TEXT NOT NULL,
               filename TEXT NOT NULL, mime_type TEXT NOT NULL, size INTEGER NOT NULL DEFAULT 0,
               attachment_id TEXT, data TEXT, PRIMARY KEY (account_id, id)
             );
             INSERT INTO attachments_rebuilt SELECT id, email_id, account_id, filename, mime_type, size, attachment_id, data FROM attachments;
             DROP TABLE attachments;
             ALTER TABLE attachments_rebuilt RENAME TO attachments;
             COMMIT;",
        )?;
    }
    Ok(())
}

/// Remove attachment rows left behind by older deletion paths. Account scope is
/// part of the relationship because Gmail message IDs can appear in more than
/// one signed-in account.
fn purge_orphaned_attachments_from_conn(conn: &Connection) -> Result<usize> {
    conn.execute(
        "DELETE FROM attachments AS attachment
         WHERE NOT EXISTS (
             SELECT 1 FROM emails AS email
             WHERE email.account_id = attachment.account_id
               AND email.id = attachment.email_id
         )",
        [],
    )
}

// ── init_db ────────────────────────────────────────────────────────────────────

pub fn init_db(app: &AppHandle) -> Result<()> {
    let db_path = get_db_path(app);
    let mut conn = Connection::open(db_path)?;
    conn.busy_timeout(Duration::from_secs(2))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;

    // accounts table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS accounts (
            id TEXT PRIMARY KEY,
            email TEXT NOT NULL UNIQUE,
            picture TEXT NOT NULL DEFAULT '',
            display_order INTEGER NOT NULL DEFAULT 0,
            cache_generation INTEGER NOT NULL DEFAULT 1,
            provider TEXT NOT NULL DEFAULT 'google'
        )",
        [],
    )?;
    let account_generation_column_exists: bool = conn
        .prepare("SELECT COUNT(*) FROM pragma_table_info('accounts') WHERE name='cache_generation'")
        .and_then(|mut s| s.query_row([], |r| r.get::<_, i64>(0)))
        .map(|count| count > 0)
        .unwrap_or(false);
    if !account_generation_column_exists {
        conn.execute(
            "ALTER TABLE accounts ADD COLUMN cache_generation INTEGER NOT NULL DEFAULT 1",
            [],
        )?;
    }
    let account_provider_column_exists: bool = conn
        .prepare("SELECT COUNT(*) FROM pragma_table_info('accounts') WHERE name='provider'")
        .and_then(|mut statement| statement.query_row([], |row| row.get::<_, i64>(0)))
        .map(|count| count > 0)
        .unwrap_or(false);
    if !account_provider_column_exists {
        conn.execute(
            "ALTER TABLE accounts ADD COLUMN provider TEXT NOT NULL DEFAULT 'google'",
            [],
        )?;
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS imap_account_settings (
             account_id TEXT PRIMARY KEY,
             username TEXT NOT NULL,
             imap_host TEXT NOT NULL,
             imap_port INTEGER NOT NULL,
             imap_security TEXT NOT NULL,
             smtp_host TEXT NOT NULL,
             smtp_port INTEGER NOT NULL,
             smtp_security TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS imap_mailboxes (
             account_id TEXT NOT NULL,
             role TEXT NOT NULL,
             mailbox TEXT NOT NULL,
             PRIMARY KEY (account_id, role)
         );",
    )?;
    // Checkpoints let a sync skip a mailbox the server reports as unchanged
    // instead of enumerating every UID. A zero default means "never synced",
    // which forces one full pass and then starts the incremental path.
    for (column, definition) in [
        ("uid_validity", "INTEGER NOT NULL DEFAULT 0"),
        ("uid_next", "INTEGER NOT NULL DEFAULT 0"),
        ("exists_count", "INTEGER NOT NULL DEFAULT 0"),
        ("reconciled_at", "INTEGER NOT NULL DEFAULT 0"),
        ("highest_mod_seq", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        let column_exists: bool = conn
            .prepare("SELECT COUNT(*) FROM pragma_table_info('imap_mailboxes') WHERE name=?1")
            .and_then(|mut statement| {
                statement.query_row(params![column], |row| row.get::<_, i64>(0))
            })
            .map(|count| count > 0)
            .unwrap_or(false);
        if !column_exists {
            conn.execute(
                &format!("ALTER TABLE imap_mailboxes ADD COLUMN {column} {definition}"),
                [],
            )?;
        }
    }
    conn.execute(
        "CREATE TABLE IF NOT EXISTS account_generations (
            account_id TEXT PRIMARY KEY,
            generation INTEGER NOT NULL
        )",
        [],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO account_generations (account_id, generation)
         SELECT id, cache_generation FROM accounts",
        [],
    )?;

    // attachments table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS attachments (
            id TEXT NOT NULL,
            email_id TEXT NOT NULL,
            account_id TEXT NOT NULL,
            filename TEXT NOT NULL,
            mime_type TEXT NOT NULL,
            size INTEGER NOT NULL DEFAULT 0,
            attachment_id TEXT,
            data TEXT,
            PRIMARY KEY (account_id, id)
        )",
        [],
    )?;

    // emails table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS emails (
            id TEXT NOT NULL,
            thread_id TEXT NOT NULL DEFAULT '',
            thread_key TEXT NOT NULL DEFAULT '',
            sender TEXT NOT NULL,
            recipient TEXT NOT NULL DEFAULT '',
            cc TEXT NOT NULL DEFAULT '',
            reply_to TEXT NOT NULL DEFAULT '',
            message_id TEXT NOT NULL DEFAULT '',
            references_header TEXT NOT NULL DEFAULT '',
            subject TEXT NOT NULL,
            snippet TEXT NOT NULL,
            body_html TEXT NOT NULL,
            body_text TEXT,
            date INTEGER NOT NULL,
            unread BOOLEAN NOT NULL,
            label TEXT NOT NULL DEFAULT 'inbox',
            account_id TEXT NOT NULL DEFAULT '',
            sync_generation INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (account_id, id)
        )",
        [],
    )?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS gmail_labels (
             account_id TEXT NOT NULL,
             id TEXT NOT NULL,
             name TEXT NOT NULL,
             background_color TEXT,
             text_color TEXT,
             PRIMARY KEY (account_id, id)
         );
         CREATE TABLE IF NOT EXISTS email_labels (
             account_id TEXT NOT NULL,
             email_id TEXT NOT NULL,
             label_id TEXT NOT NULL,
             PRIMARY KEY (account_id, email_id, label_id)
         );
         CREATE INDEX IF NOT EXISTS idx_email_labels_account_label
             ON email_labels(account_id, label_id, email_id);
         CREATE TRIGGER IF NOT EXISTS email_labels_cleanup
         AFTER DELETE ON emails BEGIN
             DELETE FROM email_labels
             WHERE account_id = old.account_id AND email_id = old.id;
         END;",
    )?;

    // Which side a label came from. Only a label the server listed may be
    // pruned when the server stops listing it; one made here has no server
    // record until it is first applied, and must survive the wait.
    let origin_exists: bool = conn
        .prepare("SELECT COUNT(*) FROM pragma_table_info('gmail_labels') WHERE name='origin'")
        .and_then(|mut statement| statement.query_row([], |row| row.get::<_, i64>(0)))
        .map(|count| count > 0)
        .unwrap_or(false);
    if !origin_exists {
        conn.execute(
            "ALTER TABLE gmail_labels ADD COLUMN origin TEXT NOT NULL DEFAULT 'local'",
            [],
        )?;
    }

    // Migration: add missing columns to emails
    let mut thread_id_was_missing = false;
    let mut reply_metadata_was_missing = false;
    for (col, ddl) in [
        (
            "label",
            "ALTER TABLE emails ADD COLUMN label TEXT NOT NULL DEFAULT 'inbox'",
        ),
        (
            "recipient",
            "ALTER TABLE emails ADD COLUMN recipient TEXT NOT NULL DEFAULT ''",
        ),
        (
            "thread_id",
            "ALTER TABLE emails ADD COLUMN thread_id TEXT NOT NULL DEFAULT ''",
        ),
        (
            "thread_key",
            "ALTER TABLE emails ADD COLUMN thread_key TEXT NOT NULL DEFAULT ''",
        ),
        (
            "cc",
            "ALTER TABLE emails ADD COLUMN cc TEXT NOT NULL DEFAULT ''",
        ),
        (
            "reply_to",
            "ALTER TABLE emails ADD COLUMN reply_to TEXT NOT NULL DEFAULT ''",
        ),
        (
            "message_id",
            "ALTER TABLE emails ADD COLUMN message_id TEXT NOT NULL DEFAULT ''",
        ),
        (
            "references_header",
            "ALTER TABLE emails ADD COLUMN references_header TEXT NOT NULL DEFAULT ''",
        ),
        ("body_text", "ALTER TABLE emails ADD COLUMN body_text TEXT"),
        (
            "account_id",
            "ALTER TABLE emails ADD COLUMN account_id TEXT NOT NULL DEFAULT ''",
        ),
        (
            "sync_generation",
            "ALTER TABLE emails ADD COLUMN sync_generation INTEGER NOT NULL DEFAULT 0",
        ),
    ] {
        let exists: bool = conn
            .prepare(&format!(
                "SELECT COUNT(*) FROM pragma_table_info('emails') WHERE name='{}'",
                col
            ))
            .and_then(|mut s| s.query_row([], |r| r.get::<_, i64>(0)))
            .map(|c| c > 0)
            .unwrap_or(false);
        if !exists {
            conn.execute(ddl, [])?;
            if col == "thread_id" {
                thread_id_was_missing = true;
            }
            if matches!(col, "reply_to" | "message_id" | "references_header") {
                reply_metadata_was_missing = true;
            }
        }
    }

    migrate_account_scoped_primary_keys(&conn)?;
    conn.execute(
        "UPDATE emails
         SET thread_key = CASE WHEN thread_id = '' THEN id ELSE thread_id END
         WHERE thread_key = '' OR thread_key != CASE WHEN thread_id = '' THEN id ELSE thread_id END",
        [],
    )?;
    // Idempotent maintenance migration for rows created before mail deletion
    // became attachment-aware.
    purge_orphaned_attachments_from_conn(&conn)?;

    // If thread_id column was just added, all existing rows have thread_id=''.
    // Also handle the case where emails exist with empty thread_ids from old syncs.
    // Reset sync_state so the next startup does a full re-sync and re-fetches thread_ids.
    if thread_id_was_missing || reply_metadata_was_missing {
        conn.execute("DELETE FROM sync_state", []).ok();
    } else {
        let empty_thread_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM emails WHERE thread_id = ''",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if empty_thread_count > 0 {
            conn.execute("DELETE FROM sync_state", []).ok();
        }
    }

    // sync_state: migrate to per-account schema
    let sync_has_account_id: bool = conn
        .prepare("SELECT COUNT(*) FROM pragma_table_info('sync_state') WHERE name='account_id'")
        .and_then(|mut s| s.query_row([], |r| r.get::<_, i64>(0)))
        .map(|c| c > 0)
        .unwrap_or(false);
    if !sync_has_account_id {
        conn.execute("DROP TABLE IF EXISTS sync_state", [])?;
    }
    conn.execute(
        "CREATE TABLE IF NOT EXISTS sync_state (
            account_id TEXT PRIMARY KEY,
            history_id TEXT,
            last_full_sync_generation INTEGER NOT NULL DEFAULT 0,
            active_full_sync_generation INTEGER,
            pending_full_history_id TEXT,
            gmail_inbox_messages_unread INTEGER,
            gmail_inbox_threads_unread INTEGER,
            mailbox_sync_status TEXT NOT NULL DEFAULT 'completed',
            mailbox_sync_error TEXT,
            mailbox_sync_retry_after INTEGER
        )",
        [],
    )?;
    for (col, ddl) in [
        (
            "last_full_sync_generation",
            "ALTER TABLE sync_state ADD COLUMN last_full_sync_generation INTEGER NOT NULL DEFAULT 0",
        ),
        (
            "active_full_sync_generation",
            "ALTER TABLE sync_state ADD COLUMN active_full_sync_generation INTEGER",
        ),
        (
            "pending_full_history_id",
            "ALTER TABLE sync_state ADD COLUMN pending_full_history_id TEXT",
        ),
        (
            "gmail_inbox_messages_unread",
            "ALTER TABLE sync_state ADD COLUMN gmail_inbox_messages_unread INTEGER",
        ),
        (
            "gmail_inbox_threads_unread",
            "ALTER TABLE sync_state ADD COLUMN gmail_inbox_threads_unread INTEGER",
        ),
        (
            "mailbox_sync_status",
            "ALTER TABLE sync_state ADD COLUMN mailbox_sync_status TEXT NOT NULL DEFAULT 'completed'",
        ),
        (
            "mailbox_sync_error",
            "ALTER TABLE sync_state ADD COLUMN mailbox_sync_error TEXT",
        ),
        (
            "mailbox_sync_retry_after",
            "ALTER TABLE sync_state ADD COLUMN mailbox_sync_retry_after INTEGER",
        ),
    ] {
        let exists: bool = conn
            .prepare(&format!(
                "SELECT COUNT(*) FROM pragma_table_info('sync_state') WHERE name='{}'",
                col
            ))
            .and_then(|mut s| s.query_row([], |r| r.get::<_, i64>(0)))
            .map(|count| count > 0)
            .unwrap_or(false);
        if !exists {
            conn.execute(ddl, [])?;
        }
    }

    // The Gmail page token that follows the locally cached newest messages for
    // each account/folder. This lets the UI load older mail on demand instead
    // of downloading an entire mailbox during the first sync.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS mailbox_cursors (
            account_id TEXT NOT NULL,
            label TEXT NOT NULL,
            next_page_token TEXT,
            PRIMARY KEY (account_id, label)
        )",
        [],
    )?;

    // Indexes for common queries
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_emails_label_date ON emails(label, date DESC)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_emails_account_label_date ON emails(account_id, label, date DESC)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_emails_inbox_unread ON emails(label, unread, account_id)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_emails_account_generation ON emails(account_id, sync_generation)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_emails_account_label_thread_date ON emails(account_id, label, thread_id, date DESC)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_emails_account_thread_date ON emails(account_id, thread_id, date DESC)",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_emails_account_thread_key_date
         ON emails(account_id, thread_key, date DESC)
         WHERE label != 'draft'",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_attachments_account_email ON attachments(account_id, email_id)",
        [],
    )?;

    let search_index_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'email_search')",
            [],
            |row| row.get(0),
        )
        .unwrap_or(false);
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS email_search USING fts5(
             sender, recipient, cc, subject, snippet, body_text,
             content='emails', content_rowid='rowid',
             tokenize='unicode61 remove_diacritics 2'
         );
         CREATE TRIGGER IF NOT EXISTS email_search_insert AFTER INSERT ON emails BEGIN
             INSERT INTO email_search(rowid, sender, recipient, cc, subject, snippet, body_text)
             VALUES (new.rowid, new.sender, new.recipient, new.cc, new.subject, new.snippet, new.body_text);
         END;
         CREATE TRIGGER IF NOT EXISTS email_search_delete AFTER DELETE ON emails BEGIN
             INSERT INTO email_search(email_search, rowid, sender, recipient, cc, subject, snippet, body_text)
             VALUES ('delete', old.rowid, old.sender, old.recipient, old.cc, old.subject, old.snippet, old.body_text);
         END;
         DROP TRIGGER IF EXISTS email_search_update;
         CREATE TRIGGER email_search_update
         AFTER UPDATE OF sender, recipient, cc, subject, snippet, body_text ON emails BEGIN
             INSERT INTO email_search(email_search, rowid, sender, recipient, cc, subject, snippet, body_text)
             VALUES ('delete', old.rowid, old.sender, old.recipient, old.cc, old.subject, old.snippet, old.body_text);
             INSERT INTO email_search(rowid, sender, recipient, cc, subject, snippet, body_text)
             VALUES (new.rowid, new.sender, new.recipient, new.cc, new.subject, new.snippet, new.body_text);
         END;",
    )?;
    if !search_index_exists {
        conn.execute(
            "INSERT INTO email_search(email_search) VALUES('rebuild')",
            [],
        )?;
    }

    let thread_summaries_exist: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'thread_summaries')",
            [],
            |row| row.get(0),
        )
        .unwrap_or(false);
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS thread_summaries (
             account_id TEXT NOT NULL,
             thread_key TEXT NOT NULL,
             latest_email_id TEXT NOT NULL,
             latest_date INTEGER NOT NULL,
             unread_count INTEGER NOT NULL,
             message_count INTEGER NOT NULL,
             participants TEXT NOT NULL DEFAULT '',
             PRIMARY KEY (account_id, thread_key)
         );
         CREATE INDEX IF NOT EXISTS idx_thread_summaries_latest
         ON thread_summaries(latest_date DESC, account_id, thread_key);",
    )?;
    if !thread_summaries_exist {
        conn.execute_batch(
            "INSERT INTO thread_summaries (
                 account_id, thread_key, latest_email_id, latest_date,
                 unread_count, message_count, participants
             )
             SELECT e.account_id, e.thread_key,
                    (SELECT latest.id
                     FROM emails latest
                     WHERE latest.account_id = e.account_id
                       AND latest.thread_key = e.thread_key
                       AND latest.label != 'draft'
                     ORDER BY latest.date DESC, latest.id ASC
                     LIMIT 1),
                    MAX(e.date),
                    SUM(CASE WHEN e.unread THEN 1 ELSE 0 END),
                    COUNT(*),
                    GROUP_CONCAT(e.sender, char(31))
             FROM emails e
             WHERE e.account_id != '' AND e.label != 'draft'
             GROUP BY e.account_id, e.thread_key;",
        )?;
    }

    // Legacy auth table (kept for migration only)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS auth (
            id INTEGER PRIMARY KEY,
            access_token TEXT NOT NULL DEFAULT '',
            refresh_token TEXT NOT NULL DEFAULT '',
            email TEXT NOT NULL DEFAULT '',
            picture TEXT NOT NULL DEFAULT ''
        )",
        [],
    )?;

    // One-time migration: auth row → accounts table. Keep this retryable even
    // after the account row exists: the credential-store write may have failed
    // during an earlier startup.
    let legacy: Option<(String, String, String, String)> = {
        let mut stmt = conn
            .prepare("SELECT access_token, refresh_token, email, picture FROM auth WHERE id = 1")
            .ok();
        stmt.as_mut().and_then(|s| {
            s.query_row([], |r| {
                Ok((
                    r.get::<_, String>(0).unwrap_or_default(),
                    r.get::<_, String>(1).unwrap_or_default(),
                    r.get::<_, String>(2).unwrap_or_default(),
                    r.get::<_, String>(3).unwrap_or_default(),
                ))
            })
            .ok()
        })
    };

    let mut credentials_preserved = true;
    if let Some((sql_access, sql_refresh, email, picture)) = legacy {
        if !email.is_empty() {
            conn.execute(
                "INSERT OR IGNORE INTO accounts (id, email, picture, display_order) VALUES (?1, ?2, ?3, 0)",
                params![email, email, picture],
            )?;

            let legacy_keyring_tokens = load_legacy_tokens();
            let (access, refresh) = legacy_keyring_tokens
                .clone()
                .or_else(|| {
                    (!sql_access.is_empty()).then_some((sql_access.clone(), sql_refresh.clone()))
                })
                .unwrap_or_default();

            if !access.is_empty() {
                credentials_preserved = save_tokens(
                    &email,
                    &StoredTokens {
                        access_token: access,
                        refresh_token: refresh,
                        expires_at: None,
                    },
                )
                .is_ok();

                // Remove the old key only after confirming its replacement.
                if credentials_preserved && legacy_keyring_tokens.is_some() {
                    delete_legacy_tokens();
                }
            }

            conn.execute(
                "UPDATE emails SET account_id = ?1 WHERE account_id = ''",
                params![email],
            )?;
        }
    }

    // Never destroy the only recoverable copy when the credential store is
    // temporarily unavailable. A later startup will retry the migration.
    if credentials_preserved {
        conn.execute("UPDATE auth SET access_token = '', refresh_token = ''", [])?;
    }

    // Any remaining rows without an owner cannot safely be assigned to an
    // account. They are local cache only, so discard them rather than allowing
    // them to affect all-account lists or unread counts.
    purge_orphaned_cache_from_conn(&mut conn)?;

    Ok(())
}

// ── Account CRUD ────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn get_accounts(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<Vec<Account>, String> {
    crate::require_command_window(&window, &["main"])?;
    let db_path = get_db_path(&app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    let mut stmt = conn
        .prepare(
            "SELECT id, email, picture, display_order, provider \
             FROM accounts ORDER BY display_order ASC, id ASC",
        )
        .map_err(database_error)?;
    let iter = stmt
        .query_map([], |row| {
            Ok(Account {
                id: row.get(0)?,
                email: row.get(1)?,
                picture: row.get(2)?,
                display_order: row.get(3)?,
                provider: row.get(4)?,
            })
        })
        .map_err(database_error)?;
    Ok(iter.filter_map(|r| r.ok()).collect())
}

/// Account ids alone, for background work that has no window to be called from.
pub fn list_account_ids(app: &AppHandle) -> Result<Vec<String>, String> {
    let db_path = get_db_path(app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    let mut stmt = conn
        .prepare("SELECT id FROM accounts")
        .map_err(database_error)?;
    let ids = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(database_error)?;
    Ok(ids.filter_map(|id| id.ok()).collect())
}

pub fn upsert_imap_account(
    app: &AppHandle,
    settings: &ImapAccountSettings,
) -> Result<Account, String> {
    let db_path = get_db_path(app);
    let mut conn = Connection::open(db_path).map_err(database_error)?;
    let tx = conn.transaction().map_err(database_error)?;
    let existing: Option<(i32, String)> = tx
        .query_row(
            "SELECT display_order, provider FROM accounts WHERE id = ?1",
            params![settings.account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(database_error)?;
    let display_order = existing.as_ref().map(|value| value.0).unwrap_or_else(|| {
        tx.query_row(
            "SELECT COALESCE(MAX(display_order), -1) + 1 FROM accounts",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0)
    });
    let replacing_provider = existing
        .as_ref()
        .is_some_and(|value| value.1.as_str() != "imap");

    if replacing_provider {
        tx.execute(
            "INSERT INTO account_generations (account_id, generation) VALUES (?1, 2)
             ON CONFLICT(account_id) DO UPDATE SET generation = generation + 1",
            params![settings.account_id],
        )
        .map_err(database_error)?;
        for table in [
            "attachments",
            "email_labels",
            "gmail_labels",
            "imap_mailboxes",
            "emails",
            "thread_summaries",
            "sync_state",
            "mailbox_cursors",
        ] {
            if tx
                .prepare(&format!("SELECT 1 FROM {table} LIMIT 1"))
                .is_ok()
            {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE account_id = ?1"),
                    params![settings.account_id],
                )
                .map_err(database_error)?;
            }
        }
    } else {
        tx.execute(
            "INSERT INTO account_generations (account_id, generation) VALUES (?1, 1)
             ON CONFLICT(account_id) DO NOTHING",
            params![settings.account_id],
        )
        .map_err(database_error)?;
    }
    let generation: i64 = tx
        .query_row(
            "SELECT generation FROM account_generations WHERE account_id = ?1",
            params![settings.account_id],
            |row| row.get(0),
        )
        .map_err(database_error)?;

    tx.execute(
        "INSERT INTO accounts (id, email, picture, display_order, cache_generation, provider)
         VALUES (?1, ?1, '', ?2, ?3, 'imap')
         ON CONFLICT(id) DO UPDATE SET
             email = excluded.email,
             picture = '',
             cache_generation = excluded.cache_generation,
             provider = 'imap'",
        params![settings.account_id, display_order, generation],
    )
    .map_err(database_error)?;
    tx.execute(
        "INSERT INTO imap_account_settings (
             account_id, username, imap_host, imap_port, imap_security,
             smtp_host, smtp_port, smtp_security
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(account_id) DO UPDATE SET
             username = excluded.username,
             imap_host = excluded.imap_host,
             imap_port = excluded.imap_port,
             imap_security = excluded.imap_security,
             smtp_host = excluded.smtp_host,
             smtp_port = excluded.smtp_port,
             smtp_security = excluded.smtp_security",
        params![
            settings.account_id,
            settings.username,
            settings.imap_host,
            settings.imap_port,
            settings.imap_security,
            settings.smtp_host,
            settings.smtp_port,
            settings.smtp_security,
        ],
    )
    .map_err(database_error)?;
    tx.commit().map_err(database_error)?;

    Ok(Account {
        id: settings.account_id.clone(),
        email: settings.account_id.clone(),
        picture: String::new(),
        display_order,
        provider: "imap".to_string(),
    })
}

pub fn get_imap_account_settings(
    app: &AppHandle,
    account_id: &str,
) -> Result<ImapAccountSettings, String> {
    let db_path = get_db_path(app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    conn.query_row(
        "SELECT account_id, username, imap_host, imap_port, imap_security,
                smtp_host, smtp_port, smtp_security
         FROM imap_account_settings WHERE account_id = ?1",
        params![account_id],
        |row| {
            Ok(ImapAccountSettings {
                account_id: row.get(0)?,
                username: row.get(1)?,
                imap_host: row.get(2)?,
                imap_port: row.get(3)?,
                imap_security: row.get(4)?,
                smtp_host: row.get(5)?,
                smtp_port: row.get(6)?,
                smtp_security: row.get(7)?,
            })
        },
    )
    .map_err(database_error)
}

/// One mailbox's synchronization checkpoint. `uid_validity` of 0 means the
/// mailbox has never completed a pass, so the next sync must reconcile it fully.
/// `highest_mod_seq` is the CONDSTORE checkpoint and stays 0 on servers without
/// it, which is what keeps those on the UID-range path.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct ImapMailboxState {
    pub uid_validity: u32,
    pub uid_next: u32,
    pub exists_count: u32,
    pub highest_mod_seq: u64,
    pub reconciled_at: i64,
}

/// Stores the discovered mailboxes without disturbing existing checkpoints.
/// A delete-and-reinsert would discard them on every sync and defeat the
/// incremental path.
/// Stores the mailbox layout the server just listed and returns the roles that
/// are no longer part of it, so the caller can drop what was cached under them.
pub fn replace_imap_mailboxes(
    app: &AppHandle,
    account_id: &str,
    mailboxes: &[(String, String)],
) -> Result<Vec<String>, String> {
    let db_path = get_db_path(app);
    let mut conn = Connection::open(db_path).map_err(database_error)?;
    let tx = conn.transaction().map_err(database_error)?;
    for (role, mailbox) in mailboxes {
        tx.execute(
            "INSERT INTO imap_mailboxes (account_id, role, mailbox) VALUES (?1, ?2, ?3)
             ON CONFLICT(account_id, role) DO UPDATE SET
                 mailbox = excluded.mailbox,
                 uid_validity = CASE
                     WHEN imap_mailboxes.mailbox = excluded.mailbox
                     THEN imap_mailboxes.uid_validity ELSE 0 END,
                 uid_next = CASE
                     WHEN imap_mailboxes.mailbox = excluded.mailbox
                     THEN imap_mailboxes.uid_next ELSE 0 END,
                 exists_count = CASE
                     WHEN imap_mailboxes.mailbox = excluded.mailbox
                     THEN imap_mailboxes.exists_count ELSE 0 END,
                 reconciled_at = CASE
                     WHEN imap_mailboxes.mailbox = excluded.mailbox
                     THEN imap_mailboxes.reconciled_at ELSE 0 END",
            params![account_id, role, mailbox],
        )
        .map_err(database_error)?;
    }
    let roles: Vec<&str> = mailboxes.iter().map(|(role, _)| role.as_str()).collect();
    let removed: Vec<String> = {
        let mut statement = tx
            .prepare("SELECT role FROM imap_mailboxes WHERE account_id = ?1")
            .map_err(database_error)?;
        let stored: Vec<String> = statement
            .query_map(params![account_id], |row| row.get(0))
            .map_err(database_error)?
            .filter_map(Result::ok)
            .collect();
        stored
            .into_iter()
            .filter(|role| !roles.iter().any(|kept| kept == role))
            .collect()
    };
    let placeholders = std::iter::repeat("?")
        .take(roles.len())
        .collect::<Vec<_>>()
        .join(",");
    let mut arguments: Vec<&dyn rusqlite::ToSql> = vec![&account_id];
    for role in &roles {
        arguments.push(role);
    }
    tx.execute(
        &format!(
            "DELETE FROM imap_mailboxes WHERE account_id = ?1
             AND role NOT IN ({})",
            if placeholders.is_empty() {
                "''"
            } else {
                &placeholders
            }
        ),
        arguments.as_slice(),
    )
    .map_err(database_error)?;
    tx.commit().map_err(database_error)?;
    Ok(removed)
}

pub fn get_imap_mailbox_state(
    app: &AppHandle,
    account_id: &str,
    role: &str,
) -> Result<ImapMailboxState, String> {
    let db_path = get_db_path(app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    get_imap_mailbox_state_from_conn(&conn, account_id, role)
}

fn get_imap_mailbox_state_from_conn(
    conn: &Connection,
    account_id: &str,
    role: &str,
) -> Result<ImapMailboxState, String> {
    conn.query_row(
        "SELECT uid_validity, uid_next, exists_count, highest_mod_seq, reconciled_at
         FROM imap_mailboxes WHERE account_id = ?1 AND role = ?2",
        params![account_id, role],
        |row| {
            Ok(ImapMailboxState {
                uid_validity: row.get::<_, i64>(0)? as u32,
                uid_next: row.get::<_, i64>(1)? as u32,
                exists_count: row.get::<_, i64>(2)? as u32,
                highest_mod_seq: row.get::<_, i64>(3)? as u64,
                reconciled_at: row.get(4)?,
            })
        },
    )
    .optional()
    .map_err(database_error)
    .map(Option::unwrap_or_default)
}

pub fn set_imap_mailbox_state(
    app: &AppHandle,
    account_id: &str,
    role: &str,
    state: ImapMailboxState,
) -> Result<(), String> {
    let db_path = get_db_path(app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    set_imap_mailbox_state_from_conn(&conn, account_id, role, state)
}

fn set_imap_mailbox_state_from_conn(
    conn: &Connection,
    account_id: &str,
    role: &str,
    state: ImapMailboxState,
) -> Result<(), String> {
    conn.execute(
        "UPDATE imap_mailboxes
         SET uid_validity = ?3, uid_next = ?4, exists_count = ?5,
             highest_mod_seq = ?6, reconciled_at = ?7
         WHERE account_id = ?1 AND role = ?2",
        params![
            account_id,
            role,
            state.uid_validity as i64,
            state.uid_next as i64,
            state.exists_count as i64,
            state.highest_mod_seq as i64,
            state.reconciled_at,
        ],
    )
    .map_err(database_error)?;
    Ok(())
}

/// How many messages the cache holds for a mailbox. Compared against the
/// server's EXISTS, this is what makes an expunge visible without listing every
/// remote UID, which is the one thing CONDSTORE cannot report on its own.
pub fn count_emails_for_label(
    app: &AppHandle,
    account_id: &str,
    label: &str,
) -> Result<u32, String> {
    let db_path = get_db_path(app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    conn.query_row(
        "SELECT COUNT(*) FROM emails WHERE account_id = ?1 AND label = ?2",
        params![account_id, label],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count.max(0) as u32)
    .map_err(database_error)
}

/// Which of `ids` the cache already holds. An incremental pass only asks about
/// the messages the server just reported, so it must not read the whole label.
pub fn filter_cached_email_ids(
    app: &AppHandle,
    account_id: &str,
    ids: &[String],
) -> Result<std::collections::HashSet<String>, String> {
    if ids.is_empty() {
        return Ok(std::collections::HashSet::new());
    }
    let db_path = get_db_path(app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    let mut cached = std::collections::HashSet::new();
    let mut statement = conn
        .prepare("SELECT id FROM emails WHERE account_id = ?1 AND id = ?2")
        .map_err(database_error)?;
    for id in ids {
        let found: Option<String> = statement
            .query_row(params![account_id, id], |row| row.get(0))
            .optional()
            .map_err(database_error)?;
        if let Some(found) = found {
            cached.insert(found);
        }
    }
    Ok(cached)
}

pub fn get_imap_mailboxes(
    app: &AppHandle,
    account_id: &str,
) -> Result<Vec<(String, String)>, String> {
    let db_path = get_db_path(app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    let mut statement = conn
        .prepare(
            "SELECT role, mailbox FROM imap_mailboxes
             WHERE account_id = ?1 ORDER BY role",
        )
        .map_err(database_error)?;
    let rows = statement
        .query_map(params![account_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(database_error)?;
    Ok(rows.filter_map(Result::ok).collect())
}

#[derive(Debug, Serialize, Clone)]
pub struct CustomMailbox {
    /// The label to pass to `get_emails_by_label`, e.g. `custom:Work`.
    pub role: String,
    /// Display name derived from the folder's own IMAP path.
    pub name: String,
}

/// User-named IMAP folders with no recognized role (not Inbox/Sent/Archive/
/// etc.) — these are still cached and syncable (see `mailbox_label` in
/// `mail_account.rs`), just not shown in the fixed system-folder list.
#[tauri::command]
pub fn get_custom_imap_mailboxes(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    account_id: String,
) -> Result<Vec<CustomMailbox>, String> {
    crate::require_command_window(&window, &["main"])?;
    // The stored name is the one the server answers to, which is modified
    // UTF-7: shown as it comes, "Önemli" reads as "&AMY-nemli".
    let display_name = |mailbox: &str| -> String {
        crate::mutf7::decode(mailbox.rsplit(['/', '.']).next().unwrap_or(mailbox))
    };
    Ok(get_imap_mailboxes(&app, &account_id)?
        .into_iter()
        .filter(|(role, _)| role.starts_with("custom:"))
        .map(|(role, mailbox)| CustomMailbox {
            name: display_name(&mailbox),
            role,
        })
        .collect())
}

pub fn get_thread_email_ids(
    app: &AppHandle,
    account_id: &str,
    thread_id: &str,
) -> Result<Vec<String>, String> {
    let db_path = get_db_path(app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    let mut statement = conn
        .prepare(
            "SELECT id FROM emails
             WHERE account_id = ?1 AND thread_key = ?2 AND label != 'draft'",
        )
        .map_err(database_error)?;
    let rows = statement
        .query_map(params![account_id, thread_id], |row| row.get(0))
        .map_err(database_error)?;
    Ok(rows.filter_map(Result::ok).collect())
}

pub fn get_email_ids_for_label(
    app: &AppHandle,
    account_id: &str,
    label: &str,
) -> Result<Vec<String>, String> {
    let db_path = get_db_path(app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    let mut statement = conn
        .prepare("SELECT id FROM emails WHERE account_id = ?1 AND label = ?2")
        .map_err(database_error)?;
    let rows = statement
        .query_map(params![account_id, label], |row| row.get(0))
        .map_err(database_error)?;
    Ok(rows.filter_map(Result::ok).collect())
}

pub fn get_account_picture(app: &AppHandle, email: &str) -> String {
    let db_path = get_db_path(app);
    let conn = match Connection::open(db_path) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };
    conn.query_row(
        "SELECT picture FROM accounts WHERE id = ?1",
        params![email],
        |r| r.get(0),
    )
    .unwrap_or_default()
}

pub fn set_account_picture(app: &AppHandle, account_id: &str, picture: &str) -> Result<(), String> {
    let db_path = get_db_path(app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    conn.execute(
        "UPDATE accounts SET picture = ?2 WHERE id = ?1",
        params![account_id, picture],
    )
    .map(|_| ())
    .map_err(database_error)
}

pub fn get_account_cache_generation(app: &AppHandle, account_id: &str) -> Result<i64, String> {
    let db_path = get_db_path(app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    conn.query_row(
        "SELECT cache_generation FROM accounts WHERE id = ?1",
        params![account_id],
        |row| row.get(0),
    )
    .map_err(|_| "Account is no longer available".to_string())
}

fn ensure_account_generation(
    conn: &Connection,
    account_id: &str,
    expected_generation: i64,
) -> Result<()> {
    let generation = conn
        .query_row(
            "SELECT cache_generation FROM accounts WHERE id = ?1",
            params![account_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if generation == Some(expected_generation) {
        Ok(())
    } else {
        Err(rusqlite::Error::QueryReturnedNoRows)
    }
}

pub fn remove_account_data(app: &tauri::AppHandle, account_id: &str) -> Result<(), String> {
    let previous_tokens = load_tokens(account_id);
    delete_tokens(account_id)?;

    let db_path = get_db_path(&app);
    let mut conn = Connection::open(db_path).map_err(database_error)?;
    if let Err(error) = remove_account_cache_from_conn(&mut conn, account_id) {
        if let Some(tokens) = previous_tokens {
            let _ = save_tokens(account_id, &tokens);
        }
        return Err(database_error(error));
    }

    if let Ok(mut workers) = app.state::<crate::SyncState>().workers.lock() {
        workers.invalidate_account(account_id);
    }

    Ok(())
}

fn remove_account_cache_from_conn(conn: &mut Connection, account_id: &str) -> Result<()> {
    let tx = conn.transaction()?;

    tx.execute(
        "INSERT INTO account_generations (account_id, generation) VALUES (?1, 2)
         ON CONFLICT(account_id) DO UPDATE SET generation = generation + 1",
        params![account_id],
    )?;
    tx.execute(
        "DELETE FROM attachments WHERE account_id = ?1",
        params![account_id],
    )?;
    if tx.prepare("SELECT 1 FROM email_labels LIMIT 1").is_ok() {
        tx.execute(
            "DELETE FROM email_labels WHERE account_id = ?1",
            params![account_id],
        )?;
    }
    if tx.prepare("SELECT 1 FROM gmail_labels LIMIT 1").is_ok() {
        tx.execute(
            "DELETE FROM gmail_labels WHERE account_id = ?1",
            params![account_id],
        )?;
    }
    tx.execute(
        "DELETE FROM emails WHERE account_id = ?1",
        params![account_id],
    )?;
    if tx.prepare("SELECT 1 FROM thread_summaries LIMIT 1").is_ok() {
        tx.execute(
            "DELETE FROM thread_summaries WHERE account_id = ?1",
            params![account_id],
        )?;
    }
    tx.execute(
        "DELETE FROM sync_state WHERE account_id = ?1",
        params![account_id],
    )?;
    tx.execute(
        "DELETE FROM mailbox_cursors WHERE account_id = ?1",
        params![account_id],
    )?;
    if tx
        .prepare("SELECT 1 FROM imap_account_settings LIMIT 1")
        .is_ok()
    {
        tx.execute(
            "DELETE FROM imap_account_settings WHERE account_id = ?1",
            params![account_id],
        )?;
    }
    if tx.prepare("SELECT 1 FROM imap_mailboxes LIMIT 1").is_ok() {
        tx.execute(
            "DELETE FROM imap_mailboxes WHERE account_id = ?1",
            params![account_id],
        )?;
    }
    tx.execute("DELETE FROM accounts WHERE id = ?1", params![account_id])?;
    tx.commit()
}

/// Removes only downloaded local mail data. Gmail messages, accounts and OAuth
/// credentials remain untouched; the next sync rebuilds this cache from Gmail.
fn reset_local_mail_cache_from_conn(
    conn: &mut Connection,
    account_id: Option<&str>,
) -> Result<Vec<String>> {
    let account_ids = match account_id {
        Some(id) => vec![id.to_string()],
        None => {
            let mut stmt = conn.prepare("SELECT id FROM accounts")?;
            let ids = stmt
                .query_map([], |row| row.get::<_, String>(0))?
                .filter_map(Result::ok)
                .collect();
            ids
        }
    };
    let tx = conn.transaction()?;

    match account_id {
        Some(id) => {
            tx.execute(
                "UPDATE account_generations SET generation = generation + 1 WHERE account_id = ?1",
                params![id],
            )?;
            tx.execute(
                "UPDATE accounts SET cache_generation = cache_generation + 1 WHERE id = ?1",
                params![id],
            )?;
            tx.execute("DELETE FROM attachments WHERE account_id = ?1", params![id])?;
            tx.execute("DELETE FROM emails WHERE account_id = ?1", params![id])?;
            if tx.prepare("SELECT 1 FROM thread_summaries LIMIT 1").is_ok() {
                tx.execute(
                    "DELETE FROM thread_summaries WHERE account_id = ?1",
                    params![id],
                )?;
            }
            tx.execute("DELETE FROM sync_state WHERE account_id = ?1", params![id])?;
            tx.execute(
                "DELETE FROM mailbox_cursors WHERE account_id = ?1",
                params![id],
            )?;
        }
        None => {
            // Bump every generation before deleting the cache so a worker that
            // was already in flight cannot write stale rows back after reset.
            tx.execute(
                "UPDATE account_generations SET generation = generation + 1",
                [],
            )?;
            tx.execute(
                "UPDATE accounts SET cache_generation = cache_generation + 1",
                [],
            )?;
            tx.execute("DELETE FROM attachments", [])?;
            tx.execute("DELETE FROM emails", [])?;
            if tx.prepare("SELECT 1 FROM thread_summaries LIMIT 1").is_ok() {
                tx.execute("DELETE FROM thread_summaries", [])?;
            }
            tx.execute("DELETE FROM sync_state", [])?;
            tx.execute("DELETE FROM mailbox_cursors", [])?;
        }
    }
    tx.commit()?;
    Ok(account_ids)
}

#[tauri::command]
pub fn reset_local_mail_cache(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    account_id: Option<String>,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    let db_path = get_db_path(&app);
    let mut conn = Connection::open(db_path).map_err(database_error)?;
    let invalidated_accounts = reset_local_mail_cache_from_conn(&mut conn, account_id.as_deref())
        .map_err(database_error)?;
    if let Ok(mut workers) = app.state::<crate::SyncState>().workers.lock() {
        for account_id in invalidated_accounts {
            workers.invalidate_account(&account_id);
        }
    }

    Ok(())
}

#[tauri::command]
pub fn reorder_accounts(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    ordered_ids: Vec<String>,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    let db_path = get_db_path(&app);
    let mut conn = Connection::open(db_path).map_err(database_error)?;
    reorder_accounts_from_conn(&mut conn, &ordered_ids).map_err(database_error)
}

pub fn email_belongs_to_account(
    app: &AppHandle,
    email_id: &str,
    account_id: &str,
) -> Result<bool, String> {
    let db_path = get_db_path(app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM emails WHERE id = ?1 AND account_id = ?2
         )",
        params![email_id, account_id],
        |row| row.get(0),
    )
    .map_err(database_error)
}

fn reorder_accounts_from_conn(conn: &mut Connection, ordered_ids: &[String]) -> Result<()> {
    let mut statement = conn.prepare("SELECT id FROM accounts")?;
    let existing_ids: std::collections::HashSet<String> = statement
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    drop(statement);
    let requested_ids: std::collections::HashSet<&str> =
        ordered_ids.iter().map(String::as_str).collect();
    if requested_ids.len() != ordered_ids.len()
        || ordered_ids.len() != existing_ids.len()
        || !ordered_ids.iter().all(|id| existing_ids.contains(id))
    {
        return Err(rusqlite::Error::InvalidParameterName(
            "ordered_ids must contain every account exactly once".to_string(),
        ));
    }

    let tx = conn.transaction()?;
    for (i, id) in ordered_ids.iter().enumerate() {
        tx.execute(
            "UPDATE accounts SET display_order = ?1 WHERE id = ?2",
            params![i as i32, id],
        )?;
    }
    tx.commit()
}

// ── Contact autocomplete ───────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct ContactSuggestion {
    pub name: String,
    pub email: String,
}

fn parse_contact(raw: &str) -> (String, String) {
    let s = raw.trim();
    if let Some(lt) = s.find('<') {
        if let Some(gt) = s.rfind('>') {
            let name = s[..lt].trim().trim_matches('"').to_string();
            let email = s[lt + 1..gt].trim().to_string();
            return (name, email);
        }
    }
    if s.contains('@') {
        return (String::new(), s.to_string());
    }
    (String::new(), String::new())
}

fn search_contacts_from_conn(
    conn: &Connection,
    query: &str,
    account_id: &str,
) -> Result<Vec<ContactSuggestion>> {
    if query.trim().is_empty() {
        return Ok(vec![]);
    }
    let like = format!("%{}%", query.to_lowercase());

    let mut raw_pairs: Vec<(String, i64)> = Vec::new();

    // Senders from received emails
    let mut stmt = conn.prepare(
        "SELECT sender, COUNT(*) FROM emails \
             WHERE account_id = ?2 AND label != 'sent' AND sender != '' AND LOWER(sender) LIKE ?1 \
             GROUP BY sender ORDER BY COUNT(*) DESC LIMIT 20",
    )?;
    let rows = stmt.query_map(params![like, account_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    for r in rows.flatten() {
        raw_pairs.push(r);
    }

    // Recipients from sent emails
    let mut stmt2 = conn
        .prepare(
            "SELECT recipient, COUNT(*) FROM emails \
             WHERE account_id = ?2 AND label = 'sent' AND recipient != '' AND LOWER(recipient) LIKE ?1 \
             GROUP BY recipient ORDER BY COUNT(*) DESC LIMIT 20",
        )?;
    let rows2 = stmt2.query_map(params![like, account_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    for r in rows2.flatten() {
        raw_pairs.push(r);
    }

    // Parse, dedupe by email, sort by count
    let q = query.to_lowercase();
    let mut counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let mut best: std::collections::HashMap<String, ContactSuggestion> =
        std::collections::HashMap::new();

    for (raw, count) in raw_pairs {
        for part in raw.split(',') {
            let (name, email) = parse_contact(part.trim());
            if email.is_empty() || !email.contains('@') {
                continue;
            }
            let el = email.to_lowercase();
            if !el.contains(&q) && !name.to_lowercase().contains(&q) {
                continue;
            }
            *counts.entry(el.clone()).or_insert(0) += count;
            best.entry(el).or_insert(ContactSuggestion { name, email });
        }
    }

    let mut result: Vec<(i64, ContactSuggestion)> = counts
        .into_iter()
        .filter_map(|(k, c)| best.remove(&k).map(|s| (c, s)))
        .collect();
    result.sort_by(|a, b| b.0.cmp(&a.0));

    Ok(result.into_iter().take(8).map(|(_, s)| s).collect())
}

#[tauri::command]
pub fn search_contacts(
    window: tauri::WebviewWindow,
    app: AppHandle,
    query: String,
    account_id: String,
) -> Result<Vec<ContactSuggestion>, String> {
    crate::require_command_window(&window, &["main"])?;
    let db_path = get_db_path(&app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    search_contacts_from_conn(&conn, &query, &account_id).map_err(database_error)
}

#[tauri::command]
pub fn get_account_auth(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    account_id: String,
) -> Result<Option<AuthInfo>, String> {
    crate::require_command_window(&window, &["main"])?;
    let db_path = get_db_path(&app);
    let conn = Connection::open(db_path).map_err(database_error)?;

    let row: Option<(String, String, String)> = conn
        .query_row(
            "SELECT email, picture, provider FROM accounts WHERE id = ?1",
            params![account_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .ok();

    let Some((email, picture, provider)) = row else {
        return Ok(None);
    };

    if provider == "imap" {
        if !crate::mail_account::has_stored_password(&account_id)
            && load_tokens(&account_id).is_none()
        {
            return Ok(None);
        }
        return Ok(Some(AuthInfo {
            authenticated: true,
            expires_at: None,
            email,
            picture,
        }));
    }

    let Some(tokens) = load_tokens(&email) else {
        return Ok(None);
    };

    Ok(Some(AuthInfo {
        authenticated: true,
        expires_at: tokens.expires_at,
        email,
        picture,
    }))
}

// ── Email CRUD ────────────────────────────────────────────────────────────────

const MAX_SEARCH_BODY_CHARS: usize = 1_000_000;

fn decode_search_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" | "#39" => Some('\''),
        "nbsp" => Some(' '),
        _ if entity.starts_with("#x") || entity.starts_with("#X") => {
            u32::from_str_radix(&entity[2..], 16)
                .ok()
                .and_then(char::from_u32)
        }
        _ if entity.starts_with('#') => entity[1..].parse::<u32>().ok().and_then(char::from_u32),
        _ => None,
    }
}

fn is_search_html_tag(tag: &str) -> bool {
    matches!(
        tag,
        "html"
            | "head"
            | "body"
            | "title"
            | "meta"
            | "link"
            | "style"
            | "script"
            | "noscript"
            | "div"
            | "span"
            | "p"
            | "br"
            | "a"
            | "img"
            | "picture"
            | "source"
            | "table"
            | "tbody"
            | "thead"
            | "tfoot"
            | "tr"
            | "td"
            | "th"
            | "ul"
            | "ol"
            | "li"
            | "blockquote"
            | "pre"
            | "code"
            | "strong"
            | "b"
            | "em"
            | "i"
            | "u"
            | "s"
            | "font"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "hr"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "main"
            | "center"
    )
}

pub(crate) fn email_body_to_search_text(source: &str) -> String {
    let mut output = String::with_capacity(source.len().min(MAX_SEARCH_BODY_CHARS));
    let mut cursor = 0;
    let mut skipped_element: Option<String> = None;
    while cursor < source.len() && output.len() < MAX_SEARCH_BODY_CHARS {
        let remaining = &source[cursor..];
        if remaining.starts_with('<') {
            let Some(end_offset) = remaining.find('>') else {
                break;
            };
            let tag_source = remaining[1..end_offset].trim();
            let closing = tag_source.starts_with('/');
            let tag_name = tag_source
                .trim_start_matches('/')
                .split(|character: char| character.is_whitespace() || character == '/')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if !is_search_html_tag(&tag_name) {
                if skipped_element.is_none() {
                    output.push('<');
                }
                cursor += 1;
                continue;
            }
            if matches!(tag_name.as_str(), "script" | "style" | "head" | "noscript") {
                if closing && skipped_element.as_deref() == Some(tag_name.as_str()) {
                    skipped_element = None;
                } else if !closing && skipped_element.is_none() {
                    skipped_element = Some(tag_name);
                }
            } else if skipped_element.is_none()
                && matches!(
                    tag_name.as_str(),
                    "br" | "p"
                        | "div"
                        | "li"
                        | "tr"
                        | "td"
                        | "th"
                        | "blockquote"
                        | "pre"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                )
            {
                output.push(' ');
            }
            cursor += end_offset + 1;
            continue;
        }
        let character = remaining.chars().next().unwrap_or_default();
        if skipped_element.is_none() && character == '&' {
            if let Some(entity_end) = remaining.get(1..).and_then(|value| value.find(';')) {
                let entity = &remaining[1..entity_end + 1];
                if let Some(decoded) = decode_search_entity(entity) {
                    output.push(decoded);
                    cursor += entity_end + 2;
                    continue;
                }
            }
        }
        if skipped_element.is_none() {
            output.push(character);
        }
        cursor += character.len_utf8();
    }
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn backfill_email_search_text(app: &AppHandle) -> Result<(), String> {
    let db_path = get_db_path(app);
    let mut conn = Connection::open(db_path).map_err(database_error)?;
    loop {
        let rows: Vec<(i64, String)> = {
            let mut statement = conn
                .prepare("SELECT rowid, body_html FROM emails WHERE body_text IS NULL LIMIT 50")
                .map_err(database_error)?;
            let collected = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(database_error)?
                .filter_map(Result::ok)
                .collect();
            collected
        };
        if rows.is_empty() {
            return Ok(());
        }
        let transaction = conn.transaction().map_err(database_error)?;
        for (rowid, body_html) in rows {
            let body_text = email_body_to_search_text(&body_html);
            transaction
                .execute(
                    "UPDATE emails SET body_text = ?1 WHERE rowid = ?2 AND body_text IS NULL",
                    params![body_text, rowid],
                )
                .map_err(database_error)?;
        }
        transaction.commit().map_err(database_error)?;
        std::thread::yield_now();
    }
}

fn refresh_thread_summary(conn: &Connection, account_id: &str, thread_key: &str) -> Result<()> {
    let summary_available: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'thread_summaries')",
        [],
        |row| row.get(0),
    )?;
    if !summary_available {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM thread_summaries WHERE account_id = ?1 AND thread_key = ?2",
        params![account_id, thread_key],
    )?;
    conn.execute(
        "INSERT INTO thread_summaries (
             account_id, thread_key, latest_email_id, latest_date,
             unread_count, message_count, participants
         )
         SELECT e.account_id, e.thread_key,
                (SELECT latest.id
                 FROM emails latest
                 WHERE latest.account_id = e.account_id
                   AND latest.thread_key = e.thread_key
                   AND latest.label != 'draft'
                 ORDER BY latest.date DESC, latest.id ASC
                 LIMIT 1),
                MAX(e.date),
                SUM(CASE WHEN e.unread THEN 1 ELSE 0 END),
                COUNT(*),
                GROUP_CONCAT(e.sender, char(31))
         FROM emails e
         WHERE e.account_id = ?1 AND e.thread_key = ?2 AND e.label != 'draft'
         GROUP BY e.account_id, e.thread_key",
        params![account_id, thread_key],
    )?;
    Ok(())
}

pub fn upsert_sync_mail_batch(
    app: &AppHandle,
    account_id: &str,
    account_generation: i64,
    sync_generation: Option<i64>,
    emails: Vec<Email>,
    attachments: Vec<Attachment>,
) -> Result<()> {
    let indexed_emails: Vec<(Email, String, String)> = emails
        .into_iter()
        .map(|email| {
            let body_text = email_body_to_search_text(&email.body_html);
            let thread_key = if email.thread_id.is_empty() {
                email.id.clone()
            } else {
                email.thread_id.clone()
            };
            (email, body_text, thread_key)
        })
        .collect();
    let mut affected_thread_keys: std::collections::HashSet<String> = indexed_emails
        .iter()
        .map(|(_, _, thread_key)| thread_key.clone())
        .collect();
    let db_path = get_db_path(app);
    let mut conn = Connection::open(db_path)?;
    let sync_generation = sync_generation.unwrap_or_else(|| {
        conn.query_row(
            "SELECT active_full_sync_generation FROM sync_state WHERE account_id = ?1",
            params![account_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()
        .ok()
        .flatten()
        .flatten()
        .unwrap_or(0)
    });
    let tx = conn.transaction()?;
    ensure_account_generation(&tx, account_id, account_generation)?;

    {
        let mut previous_thread =
            tx.prepare("SELECT thread_key FROM emails WHERE account_id = ?1 AND id = ?2")?;
        for (email, _, _) in &indexed_emails {
            if let Some(thread_key) = previous_thread
                .query_row(params![account_id, &email.id], |row| {
                    row.get::<_, String>(0)
                })
                .optional()?
            {
                affected_thread_keys.insert(thread_key);
            }
        }
    }

    {
        let mut statement = tx.prepare(
            "INSERT INTO emails (id, thread_id, thread_key, sender, recipient, cc, reply_to, message_id, references_header, subject, snippet, \
                                 body_html, body_text, date, unread, label, account_id, sync_generation)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
             ON CONFLICT(account_id, id) DO UPDATE SET
                thread_id = excluded.thread_id,
                thread_key = excluded.thread_key,
                sender = excluded.sender,
                recipient = excluded.recipient,
                cc = excluded.cc,
                reply_to = excluded.reply_to,
                message_id = excluded.message_id,
                references_header = excluded.references_header,
                subject = excluded.subject,
                snippet = excluded.snippet,
                body_html = excluded.body_html,
                body_text = excluded.body_text,
                date = excluded.date,
                unread = excluded.unread,
                label = excluded.label,
                account_id = excluded.account_id,
                sync_generation = excluded.sync_generation",
        )?;
        for (email, body_text, thread_key) in indexed_emails {
            let email_id = email.id.clone();
            let gmail_label_ids = email.gmail_label_ids.clone();
            statement.execute(params![
                email.id,
                email.thread_id,
                thread_key,
                email.sender,
                email.recipient,
                email.cc,
                email.reply_to,
                email.message_id,
                email.references,
                email.subject,
                email.snippet,
                email.body_html,
                body_text,
                email.date,
                email.unread,
                email.label,
                account_id,
                sync_generation,
            ])?;
            tx.execute(
                "DELETE FROM email_labels WHERE account_id = ?1 AND email_id = ?2",
                params![account_id, email_id],
            )?;
            for label_id in gmail_label_ids {
                tx.execute(
                    "INSERT OR IGNORE INTO email_labels (account_id, email_id, label_id)
                     SELECT ?1, ?2, ?3
                     WHERE ?3 = 'STARRED' OR EXISTS (
                         SELECT 1 FROM gmail_labels WHERE account_id = ?1 AND id = ?3
                     )",
                    params![account_id, email_id, label_id],
                )?;
            }
        }
    }

    for thread_key in affected_thread_keys {
        refresh_thread_summary(&tx, account_id, &thread_key)?;
    }

    {
        let mut statement = tx.prepare(
            "INSERT INTO attachments (id, email_id, account_id, filename, mime_type, size, attachment_id, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(account_id, id) DO UPDATE SET
                filename = excluded.filename,
                mime_type = excluded.mime_type,
                size = excluded.size,
                attachment_id = excluded.attachment_id,
                data = excluded.data",
        )?;
        for attachment in attachments {
            statement.execute(params![
                attachment.id,
                attachment.email_id,
                account_id,
                attachment.filename,
                attachment.mime_type,
                attachment.size,
                attachment.attachment_id,
                attachment.data,
            ])?;
        }
    }

    tx.commit()
}

fn map_summary_row(row: &rusqlite::Row) -> rusqlite::Result<EmailSummary> {
    Ok(EmailSummary {
        id: row.get(0)?,
        thread_id: row.get(1)?,
        sender: row.get(2)?,
        recipient: row.get(3)?,
        cc: row.get(4)?,
        reply_to: row.get(5)?,
        message_id: row.get(6)?,
        references: row.get(7)?,
        subject: row.get(8)?,
        snippet: row.get(9)?,
        date: row.get(10)?,
        unread: row.get(11)?,
        label: row.get(12)?,
        account_id: row.get(13)?,
    })
}

const SUMMARY_COLS: &str =
    "id, thread_id, sender, recipient, cc, reply_to, message_id, references_header, subject, snippet, date, unread, label, account_id";

fn sender_display_name(raw: &str) -> String {
    let candidate = raw
        .split('<')
        .next()
        .unwrap_or(raw)
        .replace('"', "")
        .trim()
        .to_string();
    if candidate.is_empty() {
        raw.trim().to_string()
    } else {
        candidate
    }
}

fn map_thread_group_row(row: &rusqlite::Row) -> rusqlite::Result<ThreadGroup> {
    let latest_email = map_summary_row(row)?;
    let participant_list: String = row.get(16)?;
    let mut participants = Vec::new();
    for raw in participant_list.split('\u{1f}') {
        let display = sender_display_name(raw);
        if !display.is_empty() && !participants.contains(&display) {
            participants.push(display);
        }
    }
    let unread_count = u32::try_from(row.get::<_, i64>(14)?).unwrap_or(u32::MAX);
    let mut label_ids = Vec::new();
    for id in row.get::<_, String>(17)?.split('\u{1f}') {
        if !id.is_empty() && !label_ids.iter().any(|existing| existing == id) {
            label_ids.push(id.to_string());
        }
    }
    Ok(ThreadGroup {
        latest_email,
        has_unread: unread_count > 0,
        unread_count,
        count: u32::try_from(row.get::<_, i64>(15)?).unwrap_or(u32::MAX),
        participants,
        label_ids,
    })
}

fn body_search_excerpt(body_text: &str, query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|term| {
            term.trim_matches(|character: char| !character.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|term| !term.is_empty())
        .collect();
    if terms.is_empty() {
        return None;
    }
    let words: Vec<&str> = body_text.split_whitespace().collect();
    let matched_index = words.iter().position(|word| {
        let normalized = word.to_lowercase();
        terms.iter().any(|term| normalized.contains(term))
    })?;
    let start = matched_index.saturating_sub(12);
    let end = (matched_index + 25).min(words.len());
    let mut excerpt = words[start..end].join(" ");
    if start > 0 {
        excerpt.insert_str(0, "… ");
    }
    if end < words.len() {
        excerpt.push_str(" …");
    }
    Some(excerpt)
}

fn search_thread_groups_from_conn(
    conn: &Connection,
    query: &str,
    filters: Option<&AdvancedSearchCriteria>,
    account_id: Option<&str>,
    limit: i64,
    cursor: Option<(i64, &str, &str)>,
) -> Result<Vec<ThreadGroup>> {
    use rusqlite::types::Value;

    let empty_filters = AdvancedSearchCriteria::default();
    let filters = filters.unwrap_or(&empty_filters);
    let mut positive_matches = Vec::new();
    for text in [query, filters.includes.as_str()] {
        let part = fts_match_query(text);
        if !part.is_empty() {
            positive_matches.push(format!("({part})"));
        }
    }
    for (column, text) in [
        ("sender", filters.from.as_str()),
        ("recipient", filters.to.as_str()),
        ("subject", filters.subject.as_str()),
    ] {
        let part = fts_scoped_match_query(column, text);
        if !part.is_empty() {
            positive_matches.push(format!("({part})"));
        }
    }
    let positive_match = positive_matches.join(" AND ");
    if positive_match.is_empty() && query.trim().is_empty() && !filters.is_active() {
        return Ok(Vec::new());
    }

    let mut values = Vec::new();
    let mut conditions = Vec::new();
    let from_clause = if positive_match.is_empty() {
        "FROM emails e"
    } else {
        values.push(Value::Text(positive_match));
        conditions.push("email_search MATCH ?".to_string());
        "FROM email_search JOIN emails e ON e.rowid = email_search.rowid"
    };
    let account_condition = if let Some(account_id) = account_id {
        values.push(Value::Text(account_id.to_string()));
        "e.account_id = ?"
    } else {
        "e.account_id != ''"
    };
    conditions.push(account_condition.to_string());
    conditions.push("e.label != 'draft'".to_string());

    let excluded_match = fts_any_match_query(&filters.excludes);
    if !excluded_match.is_empty() {
        values.push(Value::Text(excluded_match));
        conditions.push(
            "e.rowid NOT IN (SELECT rowid FROM email_search WHERE email_search MATCH ?)"
                .to_string(),
        );
    }
    if let Some(after_date) = filters.after_date {
        values.push(Value::Integer(after_date));
        conditions.push("e.date >= ?".to_string());
    }
    if let Some(before_date) = filters.before_date {
        values.push(Value::Integer(before_date));
        conditions.push("e.date < ?".to_string());
    }
    if filters.has_attachment {
        conditions.push(
            "EXISTS (SELECT 1 FROM attachments a WHERE a.account_id = e.account_id AND a.email_id = e.id)"
                .to_string(),
        );
    }
    if filters.unread {
        conditions.push("e.unread = 1".to_string());
    }
    if filters.starred {
        conditions.push(
            "EXISTS (SELECT 1 FROM email_labels starred WHERE starred.account_id = e.account_id AND starred.email_id = e.id AND starred.label_id = 'STARRED')"
                .to_string(),
        );
    }
    if let Some(label_id) = filters.location.strip_prefix("gmail:") {
        values.push(Value::Text(label_id.to_string()));
        conditions.push(
            "EXISTS (SELECT 1 FROM email_labels located WHERE located.account_id = e.account_id AND located.email_id = e.id AND located.label_id = ?)"
                .to_string(),
        );
    } else if matches!(
        filters.location.as_str(),
        "inbox" | "sent" | "archive" | "spam" | "trash"
    ) || filters.location.starts_with("custom:")
    {
        // A user folder is cached under its own label, so it filters exactly
        // like a system one. Leaving it out let a search scoped to a folder
        // quietly return the whole mailbox instead.
        values.push(Value::Text(filters.location.clone()));
        conditions.push("e.label = ?".to_string());
    } else if filters.location == "all" {
        conditions.push("e.label NOT IN ('spam', 'trash')".to_string());
    }

    // Read matching message keys in date order and collapse them into threads
    // in Rust. This avoids SQLite building and joining multiple materialized
    // temporary tables for very broad prefixes such as "a". Most threads have
    // one matching message, and iteration stops as soon as the visible page is
    // complete.
    let sql = format!(
        "SELECT e.rowid, e.account_id, e.thread_key, e.date
         {from_clause}
         WHERE {}
         ORDER BY e.date DESC, e.account_id ASC, e.thread_key ASC, e.rowid ASC",
        conditions.join(" AND ")
    );
    let mut statement = conn.prepare(&sql)?;
    let mut rows = statement.query(rusqlite::params_from_iter(values.iter()))?;
    let mut seen_threads = std::collections::HashSet::new();
    let mut selected_rowids = Vec::with_capacity(limit as usize);
    while let Some(row) = rows.next()? {
        let rowid: i64 = row.get(0)?;
        let row_account_id: String = row.get(1)?;
        let thread_key: String = row.get(2)?;
        let date: i64 = row.get(3)?;
        if !seen_threads.insert((row_account_id.clone(), thread_key.clone())) {
            continue;
        }
        let is_after_cursor =
            cursor.map_or(true, |(cursor_date, cursor_account, cursor_thread)| {
                date < cursor_date
                    || (date == cursor_date
                        && (row_account_id.as_str() > cursor_account
                            || (row_account_id == cursor_account
                                && thread_key.as_str() > cursor_thread)))
            });
        if is_after_cursor {
            selected_rowids.push(rowid);
            if selected_rowids.len() >= limit as usize {
                break;
            }
        }
    }
    drop(rows);
    drop(statement);

    let label_selection = if conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'email_labels')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .unwrap_or(false)
    {
        "COALESCE((
            SELECT GROUP_CONCAT(el.label_id, char(31))
            FROM emails tagged
            JOIN email_labels el
              ON el.account_id = tagged.account_id AND el.email_id = tagged.id
            WHERE tagged.account_id = e.account_id AND tagged.thread_key = e.thread_key
        ), '')"
    } else {
        "''"
    };
    let detail_sql = format!(
        "SELECT e.id, e.thread_id, e.sender, e.recipient, e.cc, e.reply_to,
                e.message_id, e.references_header, e.subject, e.snippet,
                e.date, e.unread, e.label, e.account_id,
                ts.unread_count, ts.message_count, ts.participants,
                {label_selection},
                COALESCE(e.body_text, '')
         FROM emails e
         JOIN thread_summaries ts
           ON ts.account_id = e.account_id AND ts.thread_key = e.thread_key
         WHERE e.rowid = ?1"
    );
    let mut detail_statement = conn.prepare(&detail_sql)?;
    let mut groups = Vec::with_capacity(selected_rowids.len());
    let excerpt_query = [query.trim(), filters.includes.trim()]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    for rowid in selected_rowids {
        let group = detail_statement.query_row(params![rowid], |row| {
            let mut group = map_thread_group_row(row)?;
            let body_text: String = row.get(18)?;
            if let Some(excerpt) = body_search_excerpt(&body_text, &excerpt_query) {
                group.latest_email.snippet = excerpt;
            }
            Ok(group)
        })?;
        groups.push(group);
    }
    Ok(groups)
}

fn get_thread_groups_from_conn(
    conn: &Connection,
    label: Option<&str>,
    query: Option<&str>,
    account_id: Option<&str>,
    limit: i64,
    cursor: Option<(i64, &str, &str)>,
) -> Result<Vec<ThreadGroup>> {
    use rusqlite::types::Value;

    if let Some(query) = query {
        return search_thread_groups_from_conn(conn, query, None, account_id, limit, cursor);
    }

    let mut conditions = Vec::new();
    let mut values = Vec::<Value>::new();
    if let Some(account_id) = account_id {
        conditions.push("account_id = ?".to_string());
        values.push(Value::Text(account_id.to_string()));
    } else {
        conditions.push("account_id != ''".to_string());
    }
    conditions.push("label != 'draft'".to_string());
    let email_labels_exist = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'email_labels')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .unwrap_or(false);
    if let Some(label) = label {
        if label == "all" {
            conditions.push("label NOT IN ('spam', 'trash')".to_string());
        } else if label == "starred" {
            if !email_labels_exist {
                return Ok(Vec::new());
            }
            conditions.push(
                "EXISTS (
                     SELECT 1 FROM emails starred_message
                     JOIN email_labels starred
                       ON starred.account_id = starred_message.account_id
                      AND starred.email_id = starred_message.id
                     WHERE starred_message.account_id = emails.account_id
                       AND starred_message.thread_key = CASE
                           WHEN emails.thread_id = '' THEN emails.id ELSE emails.thread_id
                       END
                       AND starred.label_id = 'STARRED'
                 )"
                .to_string(),
            );
        } else if let Some(gmail_label_id) = label.strip_prefix("gmail:") {
            if !email_labels_exist {
                return Ok(Vec::new());
            }
            conditions.push(
                "EXISTS (
                     SELECT 1 FROM emails tagged_filter
                     JOIN email_labels el
                       ON el.account_id = tagged_filter.account_id
                      AND el.email_id = tagged_filter.id
                     WHERE tagged_filter.account_id = emails.account_id
                       AND tagged_filter.thread_key = CASE
                           WHEN emails.thread_id = '' THEN emails.id ELSE emails.thread_id
                       END
                       AND el.label_id = ?
                 )"
                .to_string(),
            );
            values.push(Value::Text(gmail_label_id.to_string()));
        } else {
            conditions.push("label = ?".to_string());
            values.push(Value::Text(label.to_string()));
        }
    }
    let cursor_clause = if let Some((date, account_id, thread_id)) = cursor {
        values.push(Value::Integer(date));
        values.push(Value::Text(account_id.to_string()));
        values.push(Value::Text(thread_id.to_string()));
        "WHERE g.latest_date < ? OR (
             g.latest_date = ? AND (
               g.account_id > ? OR (g.account_id = ? AND g.thread_key > ?)
             )
           )"
    } else {
        ""
    };
    if cursor.is_some() {
        // The date and account values participate twice in the keyset predicate.
        let tail = values.split_off(values.len() - 3);
        values.extend([
            tail[0].clone(),
            tail[0].clone(),
            tail[1].clone(),
            tail[1].clone(),
            tail[2].clone(),
        ]);
    }
    values.push(Value::Integer(limit));

    let label_selection = if email_labels_exist {
        "COALESCE((
            SELECT GROUP_CONCAT(el.label_id, char(31))
            FROM emails tagged
            JOIN email_labels el
              ON el.account_id = tagged.account_id AND el.email_id = tagged.id
            WHERE tagged.account_id = r.account_id AND tagged.thread_key = g.thread_key
        ), '')"
    } else {
        "''"
    };
    let sql = format!(
        "WITH filtered AS (
           SELECT id, account_id, sender, date, unread,
                  CASE WHEN thread_id = '' THEN id ELSE thread_id END AS thread_key
           FROM emails
           WHERE {}
         ), grouped AS (
           SELECT account_id, thread_key, MAX(date) AS latest_date,
                  SUM(CASE WHEN unread THEN 1 ELSE 0 END) AS unread_count,
                  COUNT(*) AS message_count,
                  GROUP_CONCAT(sender, char(31)) AS participants
           FROM filtered
           GROUP BY account_id, thread_key
         ), paged AS (
           SELECT g.*
           FROM grouped g
           {cursor_clause}
           ORDER BY g.latest_date DESC, g.account_id ASC, g.thread_key ASC
           LIMIT ?
         ), ranked AS (
           SELECT f.id, f.account_id, f.thread_key,
                  ROW_NUMBER() OVER (
                    PARTITION BY f.account_id, f.thread_key ORDER BY f.date DESC, f.id ASC
                  ) AS row_number
           FROM filtered f
           JOIN paged g ON g.account_id = f.account_id AND g.thread_key = f.thread_key
         )
         SELECT r.id, r.thread_id, r.sender, r.recipient, r.cc, r.reply_to, r.message_id,
                r.references_header, r.subject, r.snippet, r.date, r.unread, r.label, r.account_id,
                g.unread_count, g.message_count, COALESCE(g.participants, ''),
                {label_selection}
         FROM paged g
         JOIN ranked latest ON latest.account_id = g.account_id
                           AND latest.thread_key = g.thread_key
                           AND latest.row_number = 1
         JOIN emails r ON r.account_id = latest.account_id AND r.id = latest.id
         ORDER BY g.latest_date DESC, g.account_id ASC, g.thread_key ASC
         ",
        conditions.join(" AND ")
    );
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(
        rusqlite::params_from_iter(values.iter()),
        map_thread_group_row,
    )?;
    Ok(rows.filter_map(Result::ok).collect())
}

fn orphaned_cache_counts_from_conn(conn: &Connection) -> Result<OrphanedCacheCounts> {
    Ok(OrphanedCacheCounts {
        emails: conn.query_row(
            "SELECT COUNT(*) FROM emails WHERE account_id = ''",
            [],
            |row| row.get(0),
        )?,
        inbox_unread: conn.query_row(
            "SELECT COUNT(*) FROM emails WHERE account_id = '' AND label = 'inbox' AND unread = 1",
            [],
            |row| row.get(0),
        )?,
        attachments: conn.query_row(
            "SELECT COUNT(*) FROM attachments WHERE account_id = ''",
            [],
            |row| row.get(0),
        )?,
    })
}

fn purge_orphaned_cache_from_conn(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM attachments WHERE account_id = ''", [])?;
    tx.execute("DELETE FROM emails WHERE account_id = ''", [])?;
    tx.execute("DELETE FROM sync_state WHERE account_id = ''", [])?;
    tx.execute("DELETE FROM mailbox_cursors WHERE account_id = ''", [])?;
    tx.commit()
}

/// Safe local-cache diagnosis for legacy rows with no account owner.
/// No mail content is returned and no data is changed.
#[tauri::command]
pub fn get_orphaned_cache_counts(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
) -> Result<OrphanedCacheCounts, String> {
    crate::require_command_window(&window, &["main"])?;
    let db_path = get_db_path(&app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    orphaned_cache_counts_from_conn(&conn).map_err(database_error)
}

#[tauri::command]
pub fn get_emails_by_label(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    label: String,
    account_id: Option<String>,
    limit: Option<u32>,
    before_date: Option<i64>,
    before_account_id: Option<String>,
    before_id: Option<String>,
) -> Result<Vec<EmailSummary>, String> {
    crate::require_command_window(&window, &["main"])?;
    let db_path = get_db_path(&app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    let limit = i64::from(limit.unwrap_or(100).clamp(1, 5_000));

    match account_id {
        Some(id) => {
            let (sql, cursor_id) = match (before_date, before_id) {
                (Some(date), Some(cursor_id)) => (
                    format!(
                        "SELECT {SUMMARY_COLS} FROM emails
                         WHERE label = ?1 AND account_id = ?2
                           AND (date < ?3 OR (date = ?3 AND id > ?4))
                         ORDER BY date DESC, id ASC LIMIT ?5"
                    ),
                    Some((date, cursor_id)),
                ),
                _ => (
                    format!(
                        "SELECT {SUMMARY_COLS} FROM emails WHERE label = ?1 AND account_id = ?2
                         ORDER BY date DESC, id ASC LIMIT ?3"
                    ),
                    None,
                ),
            };
            let mut stmt = conn.prepare(&sql).map_err(database_error)?;
            let rows: Vec<EmailSummary> = if let Some((date, cursor_id)) = cursor_id {
                stmt.query_map(params![label, id, date, cursor_id, limit], map_summary_row)
                    .map_err(database_error)?
                    .filter_map(|r| r.ok())
                    .collect()
            } else {
                stmt.query_map(params![label, id, limit], map_summary_row)
                    .map_err(database_error)?
                    .filter_map(|r| r.ok())
                    .collect()
            };
            Ok(rows)
        }
        None => {
            let cursor = match (before_date, before_account_id, before_id) {
                (Some(date), Some(account_id), Some(id)) => Some((date, account_id, id)),
                _ => None,
            };
            let sql = if cursor.is_some() {
                format!(
                    "SELECT {SUMMARY_COLS} FROM emails
                     WHERE label = ?1 AND account_id != ''
                       AND (date < ?2 OR (date = ?2 AND (account_id > ?3 OR (account_id = ?3 AND id > ?4))))
                     ORDER BY date DESC, account_id ASC, id ASC LIMIT ?5"
                )
            } else {
                format!(
                    "SELECT {SUMMARY_COLS} FROM emails WHERE label = ?1 AND account_id != ''
                     ORDER BY date DESC, account_id ASC, id ASC LIMIT ?2"
                )
            };
            let mut stmt = conn.prepare(&sql).map_err(database_error)?;
            let rows: Vec<EmailSummary> = if let Some((date, account_id, id)) = cursor {
                stmt.query_map(params![label, date, account_id, id, limit], map_summary_row)
                    .map_err(database_error)?
                    .filter_map(|r| r.ok())
                    .collect()
            } else {
                stmt.query_map(params![label, limit], map_summary_row)
                    .map_err(database_error)?
                    .filter_map(|r| r.ok())
                    .collect()
            };
            Ok(rows)
        }
    }
}

#[tauri::command]
pub fn get_thread_groups_by_label(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    label: String,
    account_id: Option<String>,
    limit: Option<u32>,
    before_date: Option<i64>,
    before_account_id: Option<String>,
    before_thread_id: Option<String>,
) -> Result<Vec<ThreadGroup>, String> {
    crate::require_command_window(&window, &["main"])?;
    let db_path = get_db_path(&app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    let limit = i64::from(limit.unwrap_or(100).clamp(1, 500));
    let cursor = match (
        before_date,
        before_account_id.as_deref(),
        before_thread_id.as_deref(),
    ) {
        (Some(date), Some(account_id), Some(thread_id)) => Some((date, account_id, thread_id)),
        _ => None,
    };
    get_thread_groups_from_conn(
        &conn,
        Some(&label),
        None,
        account_id.as_deref(),
        limit,
        cursor,
    )
    .map_err(database_error)
}

/// Just enough of a message to recognise it again. The notification baseline
/// needs every id the inbox already holds and nothing else: reading them as
/// full summaries meant megabytes of subjects and snippets crossing the bridge
/// on the first sync of every account.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailKey {
    pub account_id: String,
    pub id: String,
}

#[tauri::command]
pub fn get_inbox_email_keys(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    account_id: Option<String>,
    limit: Option<u32>,
) -> Result<Vec<EmailKey>, String> {
    crate::require_command_window(&window, &["main"])?;
    let db_path = get_db_path(&app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    let limit = i64::from(limit.unwrap_or(5_000).clamp(1, 20_000));
    let read = |sql: &str, params: &[&dyn rusqlite::ToSql]| -> Result<Vec<EmailKey>, String> {
        let mut statement = conn.prepare(sql).map_err(database_error)?;
        let rows = statement
            .query_map(params, |row| {
                Ok(EmailKey {
                    account_id: row.get(0)?,
                    id: row.get(1)?,
                })
            })
            .map_err(database_error)?;
        Ok(rows.filter_map(Result::ok).collect())
    };
    match account_id {
        Some(account_id) => read(
            "SELECT account_id, id FROM emails
             WHERE label = 'inbox' AND account_id = ?1
             ORDER BY date DESC LIMIT ?2",
            &[&account_id, &limit],
        ),
        None => read(
            "SELECT account_id, id FROM emails
             WHERE label = 'inbox' AND account_id != ''
             ORDER BY date DESC LIMIT ?1",
            &[&limit],
        ),
    }
}

#[tauri::command]
pub fn get_local_emails(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    account_id: Option<String>,
) -> Result<Vec<EmailSummary>, String> {
    crate::require_command_window(&window, &["main"])?;
    let db_path = get_db_path(&app);
    let conn = Connection::open(db_path).map_err(database_error)?;

    match account_id {
        Some(id) => {
            let sql = format!(
                "SELECT {SUMMARY_COLS} FROM emails WHERE account_id = ?1 ORDER BY date DESC"
            );
            let mut stmt = conn.prepare(&sql).map_err(database_error)?;
            let rows: Vec<EmailSummary> = stmt
                .query_map(params![id], map_summary_row)
                .map_err(database_error)?
                .filter_map(|r| r.ok())
                .collect();
            Ok(rows)
        }
        None => {
            let sql = format!(
                "SELECT {SUMMARY_COLS} FROM emails WHERE account_id != '' ORDER BY date DESC"
            );
            let mut stmt = conn.prepare(&sql).map_err(database_error)?;
            let rows: Vec<EmailSummary> = stmt
                .query_map([], map_summary_row)
                .map_err(database_error)?
                .filter_map(|r| r.ok())
                .collect();
            Ok(rows)
        }
    }
}

fn escape_like_pattern(query: &str) -> String {
    query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn fts_match_query(query: &str) -> String {
    query
        .split_whitespace()
        .filter(|term| term.chars().any(char::is_alphanumeric))
        .take(20)
        .map(|term| format!("\"{}\"*", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn fts_scoped_match_query(column: &str, query: &str) -> String {
    query
        .split_whitespace()
        .filter(|term| term.chars().any(char::is_alphanumeric))
        .take(20)
        .map(|term| format!("{column}:\"{}\"*", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn fts_any_match_query(query: &str) -> String {
    query
        .split_whitespace()
        .filter(|term| term.chars().any(char::is_alphanumeric))
        .take(20)
        .map(|term| format!("\"{}\"*", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn search_local_emails_from_conn(
    conn: &Connection,
    query: &str,
    account_id: Option<&str>,
    limit: i64,
) -> Result<Vec<EmailSummary>> {
    let pattern = format!("%{}%", escape_like_pattern(query.trim()));
    let text_match = "(subject LIKE ? ESCAPE '\\' OR sender LIKE ? ESCAPE '\\' OR recipient LIKE ? ESCAPE '\\' OR cc LIKE ? ESCAPE '\\' OR snippet LIKE ? ESCAPE '\\')";

    let (sql, account) = match account_id {
        Some(id) => (
            format!(
                "SELECT {SUMMARY_COLS} FROM emails
                 WHERE account_id = ? AND {text_match}
                 ORDER BY date DESC, id ASC LIMIT ?"
            ),
            Some(id),
        ),
        None => (
            format!(
                "SELECT {SUMMARY_COLS} FROM emails
                 WHERE account_id != '' AND {text_match}
                 ORDER BY date DESC, account_id ASC, id ASC LIMIT ?"
            ),
            None,
        ),
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = match account {
        Some(id) => stmt.query_map(
            params![id, pattern, pattern, pattern, pattern, pattern, limit],
            map_summary_row,
        )?,
        None => stmt.query_map(
            params![pattern, pattern, pattern, pattern, pattern, limit],
            map_summary_row,
        )?,
    };
    Ok(rows.filter_map(|row| row.ok()).collect())
}

/// Searches all locally cached message summaries for the selected account.
/// This deliberately searches metadata only; message bodies stay on-demand.
#[tauri::command]
pub fn search_local_emails(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    query: String,
    account_id: Option<String>,
    limit: Option<u32>,
) -> Result<Vec<EmailSummary>, String> {
    crate::require_command_window(&window, &["main"])?;
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let db_path = get_db_path(&app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    let limit = i64::from(limit.unwrap_or(500).clamp(1, 1_000));
    search_local_emails_from_conn(&conn, &query, account_id.as_deref(), limit)
        .map_err(database_error)
}

#[tauri::command]
pub async fn search_local_thread_groups(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    coordinator: tauri::State<'_, SearchCoordinator>,
    query: String,
    filters: Option<AdvancedSearchCriteria>,
    account_id: Option<String>,
    limit: Option<u32>,
    before_date: Option<i64>,
    before_account_id: Option<String>,
    before_thread_id: Option<String>,
) -> Result<Vec<ThreadGroup>, String> {
    crate::require_command_window(&window, &["main"])?;
    if query.trim().is_empty()
        && !filters
            .as_ref()
            .is_some_and(AdvancedSearchCriteria::is_active)
    {
        return Ok(Vec::new());
    }
    let db_path = get_db_path(&app);
    let limit = i64::from(limit.unwrap_or(100).clamp(1, 500));
    let coordinator = coordinator.inner().clone();
    let generation = coordinator.reserve();
    tauri::async_runtime::spawn_blocking(move || {
        let conn = Connection::open(db_path).map_err(database_error)?;
        conn.busy_timeout(Duration::from_secs(2)).map_err(database_error)?;
        conn.pragma_update(None, "query_only", true).map_err(database_error)?;
        if !coordinator.register(generation, conn.get_interrupt_handle()) {
            return Ok(Vec::new());
        }
        let started_at = Instant::now();
        let cursor = match (
            before_date,
            before_account_id.as_deref(),
            before_thread_id.as_deref(),
        ) {
            (Some(date), Some(account_id), Some(thread_id)) => {
                Some((date, account_id, thread_id))
            }
            _ => None,
        };
        let result = search_thread_groups_from_conn(
            &conn,
            &query,
            filters.as_ref(),
            account_id.as_deref(),
            limit,
            cursor,
        );
        coordinator.finish(generation);
        #[cfg(debug_assertions)]
        match &result {
            Ok(rows) => eprintln!(
                "[PERF][SEARCH] generation={generation} query_chars={} status=ok rows={} elapsed_ms={}",
                query.chars().count(),
                rows.len(),
                started_at.elapsed().as_millis()
            ),
            Err(error) => eprintln!(
                "[PERF][SEARCH] generation={generation} query_chars={} status=error elapsed_ms={} error={error}",
                query.chars().count(),
                started_at.elapsed().as_millis()
            ),
        }
        result.map_err(database_error)
    })
    .await
    .map_err(|_| "Search task could not complete.".to_string())?
}

#[tauri::command]
pub fn cancel_local_search(
    window: tauri::WebviewWindow,
    coordinator: tauri::State<'_, SearchCoordinator>,
) -> Result<(), String> {
    crate::require_command_window(&window, &["main"])?;
    coordinator.cancel();
    Ok(())
}

pub fn upsert_gmail_label(app: &AppHandle, label: &GmailLabel) -> Result<(), String> {
    let db_path = get_db_path(app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    conn.execute(
        "INSERT INTO gmail_labels (account_id, id, name, background_color, text_color)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(account_id, id) DO UPDATE SET
             name = excluded.name,
             background_color = excluded.background_color,
             text_color = excluded.text_color",
        params![
            label.account_id,
            label.id,
            label.name,
            label.background_color,
            label.text_color
        ],
    )
    .map_err(database_error)?;
    Ok(())
}

/// Seeds a label discovered via IMAP LIST (a Gmail account's real labels,
/// now that nothing else discovers them — see `discover_imap_mailboxes`).
/// Only inserts when missing: unlike `upsert_gmail_label`, this must never
/// overwrite a color or a pending local rename with server-observed defaults
/// on every sync.
pub fn seed_gmail_label_if_missing(
    app: &AppHandle,
    account_id: &str,
    id: &str,
    name: &str,
) -> Result<(), String> {
    let db_path = get_db_path(app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    conn.execute(
        "INSERT INTO gmail_labels (account_id, id, name, background_color, text_color, origin)
         VALUES (?1, ?2, ?3, NULL, NULL, 'server')
         ON CONFLICT(account_id, id) DO UPDATE SET origin = 'server'",
        params![account_id, id, name],
    )
    .map_err(database_error)?;
    Ok(())
}

/// Brings the account's label list in line with what the server just listed.
/// Only labels the server itself put here are dropped: one made in this app has
/// no server record until it is first applied to a message, and pruning it
/// would delete a label the moment it was created.
///
/// `renames` carries labels whose stored id is the raw wire name for a display
/// name the server also reports — the modified UTF-7 rows written before names
/// were decoded — so they move to the readable id with their messages.
pub fn reconcile_server_labels(
    app: &AppHandle,
    account_id: &str,
    names: &[String],
    renames: &[(String, String)],
) -> Result<(), String> {
    let db_path = get_db_path(app);
    let mut conn = Connection::open(db_path).map_err(database_error)?;
    for (from, to) in renames {
        if from == to {
            continue;
        }
        // A row already using the readable id leaves nothing to migrate.
        let taken: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM gmail_labels WHERE account_id = ?1 AND id = ?2)",
                params![account_id, to],
                |row| row.get(0),
            )
            .map_err(database_error)?;
        if taken {
            continue;
        }
        rename_gmail_label_from_conn(&mut conn, account_id, from, to, to).map_err(database_error)?;
    }
    let tx = conn.transaction().map_err(database_error)?;
    {
        let mut keep = std::collections::HashSet::new();
        for name in names {
            keep.insert(name.clone());
        }
        let mut stale = tx
            .prepare("SELECT id FROM gmail_labels WHERE account_id = ?1 AND origin = 'server'")
            .map_err(database_error)?;
        let listed: Vec<String> = stale
            .query_map(params![account_id], |row| row.get(0))
            .map_err(database_error)?
            .filter_map(Result::ok)
            .collect();
        drop(stale);
        for id in listed {
            if keep.contains(&id) {
                continue;
            }
            tx.execute(
                "DELETE FROM email_labels WHERE account_id = ?1 AND label_id = ?2",
                params![account_id, id],
            )
            .map_err(database_error)?;
            tx.execute(
                "DELETE FROM gmail_labels WHERE account_id = ?1 AND id = ?2",
                params![account_id, id],
            )
            .map_err(database_error)?;
        }
    }
    tx.commit().map_err(database_error)
}

pub fn delete_gmail_label_local(
    app: &AppHandle,
    account_id: &str,
    label_id: &str,
) -> Result<(), String> {
    let db_path = get_db_path(app);
    let mut conn = Connection::open(db_path).map_err(database_error)?;
    let tx = conn.transaction().map_err(database_error)?;
    tx.execute(
        "DELETE FROM email_labels WHERE account_id = ?1 AND label_id = ?2",
        params![account_id, label_id],
    )
    .map_err(database_error)?;
    tx.execute(
        "DELETE FROM gmail_labels WHERE account_id = ?1 AND id = ?2",
        params![account_id, label_id],
    )
    .map_err(database_error)?;
    tx.commit().map_err(database_error)
}

#[tauri::command]
pub fn get_gmail_labels(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    account_id: String,
) -> Result<Vec<GmailLabel>, String> {
    crate::require_command_window(&window, &["main"])?;
    get_gmail_labels_for_account(&app, &account_id)
}

/// The same list, for backend work that has no window behind it.
pub fn get_gmail_labels_for_account(
    app: &AppHandle,
    account_id: &str,
) -> Result<Vec<GmailLabel>, String> {
    let db_path = get_db_path(app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    let mut statement = conn
        .prepare(
            "SELECT id, account_id, name, background_color, text_color
             FROM gmail_labels WHERE account_id = ?1 ORDER BY name COLLATE NOCASE, id",
        )
        .map_err(database_error)?;
    let labels = statement
        .query_map(params![account_id], |row| {
            Ok(GmailLabel {
                id: row.get(0)?,
                account_id: row.get(1)?,
                name: row.get(2)?,
                background_color: row.get(3)?,
                text_color: row.get(4)?,
            })
        })
        .map_err(database_error)?
        .filter_map(Result::ok)
        .collect();
    Ok(labels)
}

/// Makes one label's membership match exactly what the server reported for one
/// cached mailbox. The mailbox scope is the point: a search only ever answers
/// for the mailbox it ran in, so a message cached elsewhere must not lose a
/// label because this particular search could not see it.
///
/// Returns how many cached messages changed.
pub fn set_label_membership(
    app: &AppHandle,
    account_id: &str,
    label_id: &str,
    mail_label: &str,
    member_ids: &[String],
) -> Result<usize, String> {
    let db_path = get_db_path(app);
    let mut conn = Connection::open(db_path).map_err(database_error)?;
    set_label_membership_from_conn(&mut conn, account_id, label_id, mail_label, member_ids)
}

fn set_label_membership_from_conn(
    conn: &mut Connection,
    account_id: &str,
    label_id: &str,
    mail_label: &str,
    member_ids: &[String],
) -> Result<usize, String> {
    let tx = conn.transaction().map_err(database_error)?;
    let mut changed = 0usize;
    {
        let cached: std::collections::HashSet<String> = tx
            .prepare(
                "SELECT el.email_id FROM email_labels el
                 JOIN emails e ON e.account_id = el.account_id AND e.id = el.email_id
                 WHERE el.account_id = ?1 AND el.label_id = ?2 AND e.label = ?3",
            )
            .map_err(database_error)?
            .query_map(params![account_id, label_id, mail_label], |row| row.get(0))
            .map_err(database_error)?
            .filter_map(Result::ok)
            .collect();
        let reported: std::collections::HashSet<&String> = member_ids.iter().collect();

        let mut add = tx
            .prepare(
                "INSERT OR IGNORE INTO email_labels (account_id, email_id, label_id)
                 SELECT ?1, ?2, ?3
                 WHERE EXISTS (
                     SELECT 1 FROM emails WHERE account_id = ?1 AND id = ?2 AND label = ?4
                 )",
            )
            .map_err(database_error)?;
        let mut remove = tx
            .prepare(
                "DELETE FROM email_labels
                 WHERE account_id = ?1 AND email_id = ?2 AND label_id = ?3",
            )
            .map_err(database_error)?;
        for email_id in member_ids {
            if cached.contains(email_id) {
                continue;
            }
            // A UID the server reports but this cache has not downloaded yet is
            // not an error: the message arrives with the label already on it.
            changed += add
                .execute(params![account_id, email_id, label_id, mail_label])
                .map_err(database_error)?;
        }
        for email_id in cached.iter().filter(|id| !reported.contains(id)) {
            remove
                .execute(params![account_id, email_id, label_id])
                .map_err(database_error)?;
            changed += 1;
        }
    }
    tx.commit().map_err(database_error)?;
    Ok(changed)
}

pub fn set_thread_gmail_label_local(
    app: &AppHandle,
    account_id: &str,
    thread_id: &str,
    label_id: &str,
    applied: bool,
) -> Result<(), String> {
    let db_path = get_db_path(app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    if applied {
        conn.execute(
            "INSERT OR IGNORE INTO email_labels (account_id, email_id, label_id)
             SELECT account_id, id, ?3 FROM emails
             WHERE account_id = ?1 AND thread_key = ?2 AND label != 'draft'",
            params![account_id, thread_id, label_id],
        )
        .map_err(database_error)?;
    } else {
        conn.execute(
            "DELETE FROM email_labels
             WHERE account_id = ?1 AND label_id = ?3 AND email_id IN (
                 SELECT id FROM emails WHERE account_id = ?1 AND thread_key = ?2
             )",
            params![account_id, thread_id, label_id],
        )
        .map_err(database_error)?;
    }
    Ok(())
}

pub fn gmail_label_exists(
    app: &AppHandle,
    account_id: &str,
    label_id: &str,
) -> Result<bool, String> {
    let db_path = get_db_path(app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM gmail_labels WHERE account_id = ?1 AND id = ?2)",
        params![account_id, label_id],
        |row| row.get(0),
    )
    .map_err(database_error)
}

pub fn get_gmail_label(
    app: &AppHandle,
    account_id: &str,
    label_id: &str,
) -> Result<Option<GmailLabel>, String> {
    let db_path = get_db_path(app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    conn.query_row(
        "SELECT id, account_id, name, background_color, text_color
         FROM gmail_labels WHERE account_id = ?1 AND id = ?2",
        params![account_id, label_id],
        |row| {
            Ok(GmailLabel {
                id: row.get(0)?,
                account_id: row.get(1)?,
                name: row.get(2)?,
                background_color: row.get(3)?,
                text_color: row.get(4)?,
            })
        },
    )
    .optional()
    .map_err(database_error)
}

/// Renames a label's identity (id and display name both change, since an IMAP
/// tag has no identifier separate from its own name) and repoints every
/// `email_labels` row that referenced the old id, atomically.
pub fn rename_gmail_label_local(
    app: &AppHandle,
    account_id: &str,
    old_id: &str,
    new_id: &str,
    new_name: &str,
) -> Result<(), String> {
    let db_path = get_db_path(app);
    let mut conn = Connection::open(db_path).map_err(database_error)?;
    rename_gmail_label_from_conn(&mut conn, account_id, old_id, new_id, new_name)
        .map_err(database_error)
}

fn rename_gmail_label_from_conn(
    conn: &mut Connection,
    account_id: &str,
    old_id: &str,
    new_id: &str,
    new_name: &str,
) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE gmail_labels SET id = ?3, name = ?4 WHERE account_id = ?1 AND id = ?2",
        params![account_id, old_id, new_id, new_name],
    )?;
    tx.execute(
        "UPDATE OR IGNORE email_labels SET label_id = ?3
         WHERE account_id = ?1 AND label_id = ?2",
        params![account_id, old_id, new_id],
    )?;
    // A message that already carried the new label keeps the one row it has:
    // the update above skipped it, and what is left behind is a reference to a
    // label id that no longer exists.
    tx.execute(
        "DELETE FROM email_labels WHERE account_id = ?1 AND label_id = ?2",
        params![account_id, old_id],
    )?;
    tx.commit()
}

pub fn set_gmail_label_color_local(
    app: &AppHandle,
    account_id: &str,
    label_id: &str,
    background_color: Option<&str>,
    text_color: Option<&str>,
) -> Result<(), String> {
    let db_path = get_db_path(app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    conn.execute(
        "UPDATE gmail_labels SET background_color = ?3, text_color = ?4
         WHERE account_id = ?1 AND id = ?2",
        params![account_id, label_id, background_color, text_color],
    )
    .map_err(database_error)?;
    Ok(())
}

#[tauri::command]
pub fn get_email_body(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    id: String,
    account_id: String,
) -> Result<String, String> {
    crate::require_command_window(&window, &["main"])?;
    let db_path = get_db_path(&app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    conn.query_row(
        "SELECT body_html FROM emails WHERE id = ?1 AND account_id = ?2",
        params![id, account_id],
        |row| row.get(0),
    )
    .map_err(database_error)
}

pub fn mark_email_as_read_local(app: &AppHandle, id: &str, account_id: &str) -> Result<(), String> {
    let db_path = get_db_path(app);
    let mut conn = Connection::open(db_path).map_err(database_error)?;
    let tx = conn.transaction().map_err(database_error)?;
    let thread_key: Option<String> = tx
        .query_row(
            "SELECT thread_key FROM emails WHERE id = ?1 AND account_id = ?2",
            params![id, account_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(database_error)?;
    tx.execute(
        "UPDATE emails SET unread = 0 WHERE id = ?1 AND account_id = ?2",
        params![id, account_id],
    )
    .map_err(database_error)?;
    if let Some(thread_key) = thread_key {
        refresh_thread_summary(&tx, account_id, &thread_key).map_err(database_error)?;
    }
    tx.commit().map_err(database_error)?;
    Ok(())
}

/// One cached message's state as the server just reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImapMessageState {
    pub email_id: String,
    pub unread: bool,
    pub starred: bool,
    /// The tags the server carries for this message. `None` means the pass
    /// could not read them — an older server, or a Gmail reply that could not
    /// be parsed — and the cached tags are left untouched instead of being
    /// mistaken for a message the user just cleared.
    pub tags: Option<Vec<String>>,
}

/// Writes the read, starred and tag state the IMAP server reports onto cached
/// messages. Only rows whose state actually differs are touched so an unchanged
/// mailbox costs no writes and no thread-summary rebuilds.
/// Returns how many cached messages actually changed, which is what tells a
/// caller whether the server reported anything the views have to be re-read for.
pub fn apply_imap_flag_state(
    app: &AppHandle,
    account_id: &str,
    updates: &[ImapMessageState],
) -> Result<usize, String> {
    if updates.is_empty() {
        return Ok(0);
    }
    let db_path = get_db_path(app);
    let mut conn = Connection::open(db_path).map_err(database_error)?;
    apply_imap_flag_state_from_conn(&mut conn, account_id, updates)
}

fn apply_imap_flag_state_from_conn(
    conn: &mut Connection,
    account_id: &str,
    updates: &[ImapMessageState],
) -> Result<usize, String> {
    let tx = conn.transaction().map_err(database_error)?;
    let mut changed_threads = std::collections::HashSet::new();
    let mut changed_messages = 0usize;
    {
        let mut current = tx
            .prepare(
                "SELECT e.unread, e.thread_key,
                        EXISTS (SELECT 1 FROM email_labels l
                                WHERE l.account_id = e.account_id AND l.email_id = e.id
                                  AND l.label_id = 'STARRED')
                 FROM emails e WHERE e.account_id = ?1 AND e.id = ?2",
            )
            .map_err(database_error)?;
        let mut set_unread = tx
            .prepare("UPDATE emails SET unread = ?3 WHERE account_id = ?1 AND id = ?2")
            .map_err(database_error)?;
        let mut add_star = tx
            .prepare(
                "INSERT OR IGNORE INTO email_labels (account_id, email_id, label_id)
                 VALUES (?1, ?2, 'STARRED')",
            )
            .map_err(database_error)?;
        let mut drop_star = tx
            .prepare(
                "DELETE FROM email_labels
                 WHERE account_id = ?1 AND email_id = ?2 AND label_id = 'STARRED'",
            )
            .map_err(database_error)?;
        let mut cached_tags = tx
            .prepare(
                "SELECT label_id FROM email_labels
                 WHERE account_id = ?1 AND email_id = ?2 AND label_id != 'STARRED'",
            )
            .map_err(database_error)?;
        let mut add_tag = tx
            .prepare(
                "INSERT OR IGNORE INTO email_labels (account_id, email_id, label_id)
                 VALUES (?1, ?2, ?3)",
            )
            .map_err(database_error)?;
        let mut drop_tag = tx
            .prepare(
                "DELETE FROM email_labels
                 WHERE account_id = ?1 AND email_id = ?2 AND label_id = ?3",
            )
            .map_err(database_error)?;
        // A tag the server knows about but this account has no record of is a
        // tag made somewhere else — another client, or this app before its
        // cache was cleared. Recording it is what makes it appear in the
        // sidebar instead of being dropped on the way in.
        let mut ensure_tag = tx
            .prepare(
                "INSERT OR IGNORE INTO gmail_labels (account_id, id, name, background_color, text_color)
                 VALUES (?1, ?2, ?2, NULL, NULL)",
            )
            .map_err(database_error)?;

        for ImapMessageState {
            email_id,
            unread,
            starred,
            tags,
        } in updates
        {
            let row: Option<(bool, String, bool)> = current
                .query_row(params![account_id, email_id], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .optional()
                .map_err(database_error)?;
            let Some((cached_unread, thread_key, cached_starred)) = row else {
                continue;
            };
            if cached_unread != *unread || cached_starred != *starred {
                changed_messages += 1;
            }
            if cached_unread != *unread {
                set_unread
                    .execute(params![account_id, email_id, *unread])
                    .map_err(database_error)?;
                changed_threads.insert(thread_key.clone());
            }
            if cached_starred != *starred {
                if *starred {
                    add_star
                        .execute(params![account_id, email_id])
                        .map_err(database_error)?;
                } else {
                    drop_star
                        .execute(params![account_id, email_id])
                        .map_err(database_error)?;
                }
                changed_threads.insert(thread_key.clone());
            }
            let Some(tags) = tags else {
                continue;
            };
            let cached: std::collections::BTreeSet<String> = cached_tags
                .query_map(params![account_id, email_id], |row| row.get(0))
                .map_err(database_error)?
                .filter_map(Result::ok)
                .collect();
            let reported: std::collections::BTreeSet<String> = tags.iter().cloned().collect();
            if cached == reported {
                continue;
            }
            for tag in reported.difference(&cached) {
                ensure_tag
                    .execute(params![account_id, tag])
                    .map_err(database_error)?;
                add_tag
                    .execute(params![account_id, email_id, tag])
                    .map_err(database_error)?;
            }
            for tag in cached.difference(&reported) {
                drop_tag
                    .execute(params![account_id, email_id, tag])
                    .map_err(database_error)?;
            }
            changed_messages += 1;
            changed_threads.insert(thread_key);
        }
    }
    for thread_key in &changed_threads {
        refresh_thread_summary(&tx, account_id, thread_key).map_err(database_error)?;
    }
    tx.commit().map_err(database_error)?;
    Ok(changed_messages)
}

pub fn mark_thread_as_unread_local(
    app: &AppHandle,
    thread_id: &str,
    account_id: &str,
) -> Result<(), String> {
    let db_path = get_db_path(app);
    let mut conn = Connection::open(db_path).map_err(database_error)?;
    let tx = conn.transaction().map_err(database_error)?;
    tx.execute(
        "UPDATE emails SET unread = 1 WHERE thread_key = ?1 AND account_id = ?2 AND label != 'draft'",
        params![thread_id, account_id],
    )
    .map_err(database_error)?;
    refresh_thread_summary(&tx, account_id, thread_id).map_err(database_error)?;
    tx.commit().map_err(database_error)?;
    Ok(())
}

pub fn mark_thread_as_read_local(
    app: &AppHandle,
    thread_id: &str,
    account_id: &str,
) -> Result<(), String> {
    let db_path = get_db_path(app);
    let mut conn = Connection::open(db_path).map_err(database_error)?;
    let tx = conn.transaction().map_err(database_error)?;
    tx.execute(
        "UPDATE emails SET unread = 0 WHERE thread_key = ?1 AND account_id = ?2 AND label != 'draft'",
        params![thread_id, account_id],
    )
    .map_err(database_error)?;
    refresh_thread_summary(&tx, account_id, thread_id).map_err(database_error)?;
    tx.commit().map_err(database_error)?;
    Ok(())
}

#[tauri::command]
pub fn get_inbox_unread_count(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    account_id: Option<String>,
) -> Result<i64, String> {
    crate::require_command_window(&window, &["main"])?;
    let db_path = get_db_path(&app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    inbox_unread_count_from_conn(&conn, account_id.as_deref()).map_err(database_error)
}

/// How many unread messages the inbox holds that arrived after a mark. The
/// catch-up summary needs the real number: an application that was closed for a
/// week can have more waiting than a page of the list ever reads.
#[tauri::command]
pub fn count_inbox_unread_since(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    account_id: String,
    since: i64,
) -> Result<i64, String> {
    crate::require_command_window(&window, &["main"])?;
    let db_path = get_db_path(&app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    conn.query_row(
        "SELECT COUNT(*) FROM emails
         WHERE account_id = ?1 AND label = 'inbox' AND unread = 1 AND date > ?2",
        params![account_id, since],
        |row| row.get(0),
    )
    .map_err(database_error)
}

fn inbox_unread_count_from_conn(conn: &Connection, account_id: Option<&str>) -> Result<i64> {
    let count: i64 = match account_id {
        Some(id) => conn.query_row(
            "SELECT COALESCE(
                (SELECT gmail_inbox_messages_unread FROM sync_state WHERE account_id = ?1),
                (SELECT COUNT(*) FROM emails WHERE label = 'inbox' AND unread = 1 AND account_id = ?1)
             )",
            params![id],
            |row| row.get(0),
        ),
        None => conn.query_row(
            "SELECT COALESCE(SUM(COALESCE(
                s.gmail_inbox_messages_unread,
                (SELECT COUNT(*) FROM emails e WHERE e.label = 'inbox' AND e.unread = 1 AND e.account_id = a.id)
             )), 0)
             FROM accounts a
             LEFT JOIN sync_state s ON s.account_id = a.id",
            [],
            |row| row.get(0),
        ),
    }
    ?;
    Ok(count)
}

// ── Sync state (per-account history ID) ────────────────────────────────────────

pub fn has_pending_mailbox_pages(
    app: &AppHandle,
    account_id: Option<&str>,
) -> Result<bool, String> {
    let db_path = get_db_path(app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    let count: i64 = match account_id {
        Some(account_id) => conn.query_row(
            "SELECT COUNT(*) FROM mailbox_cursors
             WHERE account_id = ?1 AND next_page_token IS NOT NULL",
            params![account_id],
            |row| row.get(0),
        ),
        None => conn.query_row(
            "SELECT COUNT(*) FROM mailbox_cursors WHERE next_page_token IS NOT NULL",
            [],
            |row| row.get(0),
        ),
    }
    .map_err(database_error)?;
    Ok(count > 0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxSyncState {
    pub status: String,
    pub error: Option<String>,
    pub retry_after: Option<i64>,
}

pub fn get_mailbox_sync_state(
    app: &AppHandle,
    account_id: &str,
) -> Result<Option<MailboxSyncState>, String> {
    let db_path = get_db_path(app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    conn.query_row(
        "SELECT mailbox_sync_status, mailbox_sync_error, mailbox_sync_retry_after
         FROM sync_state WHERE account_id = ?1",
        params![account_id],
        |row| {
            Ok(MailboxSyncState {
                status: row.get(0)?,
                error: row.get(1)?,
                retry_after: row.get(2)?,
            })
        },
    )
    .optional()
    .map_err(database_error)
}

pub fn get_all_mailbox_sync_states(app: &AppHandle) -> Result<Vec<MailboxSyncState>, String> {
    let db_path = get_db_path(app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    let mut statement = conn
        .prepare(
            "SELECT mailbox_sync_status, mailbox_sync_error, mailbox_sync_retry_after
             FROM sync_state",
        )
        .map_err(database_error)?;
    let states = statement
        .query_map([], |row| {
            Ok(MailboxSyncState {
                status: row.get(0)?,
                error: row.get(1)?,
                retry_after: row.get(2)?,
            })
        })
        .map_err(database_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(database_error)?;
    Ok(states)
}

#[tauri::command]
pub fn get_thread_emails(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    thread_id: String,
    account_id: String,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Vec<EmailSummary>, String> {
    crate::require_command_window(&window, &["main"])?;
    if thread_id.is_empty() {
        return Ok(vec![]);
    }
    let db_path = get_db_path(&app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    let limit = i64::from(limit.unwrap_or(20).clamp(1, 50));
    let offset = i64::from(offset.unwrap_or(0));
    let sql = format!(
        "SELECT {SUMMARY_COLS} FROM emails WHERE thread_id = ?1 AND account_id = ?2 AND label != 'draft' \
         ORDER BY date DESC, id ASC LIMIT ?3 OFFSET ?4"
    );
    let mut stmt = conn.prepare(&sql).map_err(database_error)?;
    let rows: Vec<EmailSummary> = stmt
        .query_map(
            params![thread_id, account_id, limit, offset],
            map_summary_row,
        )
        .map_err(database_error)?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

#[cfg(test)]
fn attachment_account_id<'a>(
    authorized_account_id: Option<&'a str>,
    attachment: &'a Attachment,
) -> &'a str {
    authorized_account_id.unwrap_or(attachment.account_id.as_str())
}

pub fn get_email_attachments_for_account(
    app: tauri::AppHandle,
    email_id: String,
    account_id: String,
) -> Result<Vec<Attachment>, String> {
    let db_path = get_db_path(&app);
    let conn = Connection::open(db_path).map_err(database_error)?;
    let mut stmt = conn
        .prepare(
            "SELECT id, email_id, account_id, filename, mime_type, size, attachment_id, data
             FROM attachments WHERE email_id = ?1 AND account_id = ?2 ORDER BY rowid ASC",
        )
        .map_err(database_error)?;
    let rows = stmt
        .query_map(params![email_id, account_id], |row| {
            Ok(Attachment {
                id: row.get(0)?,
                email_id: row.get(1)?,
                account_id: row.get(2)?,
                filename: row.get(3)?,
                mime_type: row.get(4)?,
                size: row.get(5)?,
                attachment_id: row.get(6)?,
                data: row.get(7)?,
            })
        })
        .map_err(database_error)?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

#[tauri::command]
pub fn get_email_attachments(
    window: tauri::WebviewWindow,
    app: tauri::AppHandle,
    email_id: String,
    account_id: String,
) -> Result<Vec<Attachment>, String> {
    crate::require_command_window(&window, &["main"])?;
    get_email_attachments_for_account(app, email_id, account_id)
}

fn delete_emails_by_ids_from_conn(
    conn: &mut Connection,
    account_id: &str,
    ids: &[String],
) -> Result<usize> {
    if ids.is_empty() {
        return Ok(0);
    }

    let placeholders = vec!["?"; ids.len()].join(",");
    let attachment_sql =
        format!("DELETE FROM attachments WHERE account_id = ? AND email_id IN ({placeholders})");
    let email_sql = format!("DELETE FROM emails WHERE account_id = ? AND id IN ({placeholders})");
    let mut params: Vec<&dyn rusqlite::types::ToSql> = vec![&account_id];
    params.extend(ids.iter().map(|s| s as &dyn rusqlite::types::ToSql));
    let tx = conn.transaction()?;
    let affected_thread_keys: Vec<String> =
        if tx.prepare("SELECT thread_key FROM emails LIMIT 0").is_ok() {
            let thread_key_sql = format!(
            "SELECT DISTINCT thread_key FROM emails WHERE account_id = ? AND id IN ({placeholders})"
        );
            let mut statement = tx.prepare(&thread_key_sql)?;
            let keys = statement
                .query_map(params.as_slice(), |row| row.get(0))?
                .filter_map(Result::ok)
                .collect();
            keys
        } else {
            Vec::new()
        };
    tx.execute(&attachment_sql, params.as_slice())?;
    let deleted = tx.execute(&email_sql, params.as_slice())?;
    for thread_key in affected_thread_keys {
        refresh_thread_summary(&tx, account_id, &thread_key)?;
    }
    tx.commit()?;
    Ok(deleted)
}

pub fn delete_emails_by_ids(
    app: &AppHandle,
    account_id: &str,
    account_generation: i64,
    ids: &[String],
) -> Result<(), String> {
    let db_path = get_db_path(app);
    let mut conn = Connection::open(db_path).map_err(database_error)?;
    ensure_account_generation(&conn, account_id, account_generation)
        .map_err(|_| "Account is no longer available")?;
    delete_emails_by_ids_from_conn(&mut conn, account_id, ids).map_err(database_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_too_long_for_one_credential_is_split_and_rejoined() {
        // Windows stores a credential in 2560 bytes, and as UTF-16 that is
        // 1280 characters: a Microsoft access token alone can be longer.
        let tokens = StoredTokens {
            access_token: "a".repeat(2200),
            refresh_token: "r".repeat(1400),
            expires_at: Some(1_700_000_000),
        };
        let data = serde_json::to_string(&tokens).expect("serialize");
        let parts = split_credential(&data, MAX_CREDENTIAL_CHARS);

        assert!(parts.len() > 1, "this session has to be split");
        assert!(parts
            .iter()
            .all(|part| part.chars().count() <= MAX_CREDENTIAL_CHARS));
        assert_eq!(parts.concat(), data);

        let rejoined: StoredTokens = serde_json::from_str(&parts.concat()).expect("rejoin");
        assert_eq!(rejoined.access_token, tokens.access_token);
        assert_eq!(rejoined.refresh_token, tokens.refresh_token);
        assert_eq!(rejoined.expires_at, tokens.expires_at);
    }

    #[test]
    fn a_session_that_fits_is_left_whole() {
        let data = serde_json::to_string(&StoredTokens {
            access_token: "access".to_string(),
            refresh_token: "refresh".to_string(),
            expires_at: None,
        })
        .expect("serialize");
        assert_eq!(
            split_credential(&data, MAX_CREDENTIAL_CHARS),
            vec![data.clone()]
        );
    }

    #[test]
    fn stored_tokens_accept_legacy_keyring_json_without_expiry() {
        let tokens: StoredTokens =
            serde_json::from_str(r#"{"access_token":"access","refresh_token":"refresh"}"#)
                .expect("legacy keyring JSON");
        assert_eq!(tokens.access_token, "access");
        assert_eq!(tokens.refresh_token, "refresh");
        assert_eq!(tokens.expires_at, None);
    }

    #[test]
    fn auth_info_never_serializes_bearer_credentials() {
        let value = serde_json::to_value(AuthInfo {
            authenticated: true,
            expires_at: Some(123),
            email: "alice@example.test".to_string(),
            picture: String::new(),
        })
        .expect("serialize auth info");
        assert_eq!(value["authenticated"], true);
        assert!(value.get("access_token").is_none());
        assert!(value.get("refresh_token").is_none());
    }

    #[test]
    fn account_reordering_requires_the_complete_unique_account_set() {
        let mut conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch(
            "CREATE TABLE accounts (
                id TEXT PRIMARY KEY,
                display_order INTEGER NOT NULL
             );
             INSERT INTO accounts (id, display_order) VALUES
                ('account-a', 0),
                ('account-b', 1);",
        )
        .expect("seed accounts");

        reorder_accounts_from_conn(
            &mut conn,
            &["account-b".to_string(), "account-a".to_string()],
        )
        .expect("reorder complete account set");
        assert!(reorder_accounts_from_conn(
            &mut conn,
            &["account-a".to_string(), "account-a".to_string()],
        )
        .is_err());
        assert!(reorder_accounts_from_conn(&mut conn, &["account-a".to_string()]).is_err());

        let order: Vec<String> = {
            let mut statement = conn
                .prepare("SELECT id FROM accounts ORDER BY display_order")
                .expect("prepare order query");
            statement
                .query_map([], |row| row.get(0))
                .expect("query order")
                .collect::<Result<_, _>>()
                .expect("collect order")
        };
        assert_eq!(order, ["account-b", "account-a"]);
    }

    #[test]
    fn synchronized_attachment_uses_the_authorized_account() {
        let attachment = Attachment {
            id: "attachment-a".to_string(),
            email_id: "message-a".to_string(),
            account_id: "untrusted-account".to_string(),
            filename: "report.pdf".to_string(),
            mime_type: "application/pdf".to_string(),
            size: 1,
            attachment_id: None,
            data: None,
        };
        assert_eq!(
            attachment_account_id(Some("authorized-account"), &attachment),
            "authorized-account"
        );
        assert_eq!(
            attachment_account_id(None, &attachment),
            "untrusted-account"
        );
    }

    fn seed_flag_state_database() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch(
            "CREATE TABLE emails (
                id TEXT NOT NULL,
                account_id TEXT NOT NULL,
                thread_key TEXT NOT NULL,
                unread INTEGER NOT NULL,
                label TEXT NOT NULL DEFAULT 'inbox',
                PRIMARY KEY (account_id, id)
            );
            CREATE TABLE email_labels (
                account_id TEXT NOT NULL,
                email_id TEXT NOT NULL,
                label_id TEXT NOT NULL,
                PRIMARY KEY (account_id, email_id, label_id)
            );
            CREATE TABLE gmail_labels (
                account_id TEXT NOT NULL,
                id TEXT NOT NULL,
                name TEXT NOT NULL,
                background_color TEXT,
                text_color TEXT,
                PRIMARY KEY (account_id, id)
            );
            INSERT INTO emails (id, account_id, thread_key, unread) VALUES
                ('imap:inbox:1', 'account-a', 'thread-1', 1),
                ('imap:inbox:2', 'account-a', 'thread-2', 0),
                ('imap:inbox:1', 'account-b', 'thread-9', 1);
            INSERT INTO email_labels (account_id, email_id, label_id) VALUES
                ('account-a', 'imap:inbox:2', 'STARRED');",
        )
        .expect("seed flag fixtures");
        conn
    }

    fn read_flag_state(conn: &Connection, account_id: &str, email_id: &str) -> (bool, bool) {
        conn.query_row(
            "SELECT e.unread,
                    EXISTS (SELECT 1 FROM email_labels l
                            WHERE l.account_id = e.account_id AND l.email_id = e.id
                              AND l.label_id = 'STARRED')
             FROM emails e WHERE e.account_id = ?1 AND e.id = ?2",
            params![account_id, email_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read flag state")
    }

    #[test]
    fn imap_flag_state_adopts_read_and_starred_changes_made_on_another_device() {
        let mut conn = seed_flag_state_database();

        apply_imap_flag_state_from_conn(
            &mut conn,
            "account-a",
            &[
                // Read elsewhere, and starred elsewhere.
                ImapMessageState {
                    email_id: "imap:inbox:1".to_string(),
                    unread: false,
                    starred: true,
                    tags: None,
                },
                // Marked unread again, and unstarred.
                ImapMessageState {
                    email_id: "imap:inbox:2".to_string(),
                    unread: true,
                    starred: false,
                    tags: None,
                },
            ],
        )
        .expect("apply flag state");

        assert_eq!(
            read_flag_state(&conn, "account-a", "imap:inbox:1"),
            (false, true)
        );
        assert_eq!(
            read_flag_state(&conn, "account-a", "imap:inbox:2"),
            (true, false)
        );
    }

    #[test]
    fn imap_flag_state_never_reaches_another_account() {
        let mut conn = seed_flag_state_database();

        apply_imap_flag_state_from_conn(
            &mut conn,
            "account-a",
            &[ImapMessageState {
                email_id: "imap:inbox:1".to_string(),
                unread: false,
                starred: false,
                tags: None,
            }],
        )
        .expect("apply flag state");

        assert_eq!(
            read_flag_state(&conn, "account-b", "imap:inbox:1"),
            (true, false)
        );
    }

    #[test]
    fn imap_flag_state_ignores_uids_that_are_not_cached() {
        let mut conn = seed_flag_state_database();

        apply_imap_flag_state_from_conn(
            &mut conn,
            "account-a",
            &[ImapMessageState {
                email_id: "imap:inbox:404".to_string(),
                unread: false,
                starred: true,
                tags: None,
            }],
        )
        .expect("apply flag state for an unknown message");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM emails", [], |row| row.get(0))
            .expect("count emails");
        assert_eq!(count, 3);
    }

    fn read_tags(conn: &Connection, account_id: &str, email_id: &str) -> Vec<String> {
        let mut statement = conn
            .prepare(
                "SELECT label_id FROM email_labels
                 WHERE account_id = ?1 AND email_id = ?2 AND label_id != 'STARRED'
                 ORDER BY label_id",
            )
            .expect("prepare tag read");
        let tags = statement
            .query_map(params![account_id, email_id], |row| row.get(0))
            .expect("read tags")
            .filter_map(Result::ok)
            .collect();
        tags
    }

    #[test]
    fn tags_follow_what_the_server_reports_and_record_unknown_ones() {
        let mut conn = seed_flag_state_database();
        conn.execute_batch(
            "INSERT INTO email_labels (account_id, email_id, label_id) VALUES
                 ('account-a', 'imap:inbox:1', 'Gone');",
        )
        .expect("seed a tag the server no longer has");

        apply_imap_flag_state_from_conn(
            &mut conn,
            "account-a",
            &[ImapMessageState {
                email_id: "imap:inbox:1".to_string(),
                unread: true,
                starred: false,
                tags: Some(vec!["Work".to_string(), "Önemli".to_string()]),
            }],
        )
        .expect("apply tag state");

        assert_eq!(
            read_tags(&conn, "account-a", "imap:inbox:1"),
            vec!["Work".to_string(), "Önemli".to_string()]
        );
        // A tag seen for the first time becomes a label of this account, or the
        // insert above would have been dropped by the label check.
        let known: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM gmail_labels WHERE account_id = 'account-a'",
                [],
                |row| row.get(0),
            )
            .expect("count labels");
        assert_eq!(known, 2);
    }

    #[test]
    fn a_pass_that_could_not_read_tags_leaves_them_alone() {
        let mut conn = seed_flag_state_database();
        conn.execute_batch(
            "INSERT INTO email_labels (account_id, email_id, label_id) VALUES
                 ('account-a', 'imap:inbox:1', 'Work');",
        )
        .expect("seed a tag");

        apply_imap_flag_state_from_conn(
            &mut conn,
            "account-a",
            &[ImapMessageState {
                email_id: "imap:inbox:1".to_string(),
                unread: false,
                starred: false,
                tags: None,
            }],
        )
        .expect("apply flag state without tags");

        assert_eq!(
            read_tags(&conn, "account-a", "imap:inbox:1"),
            vec!["Work".to_string()]
        );
    }

    #[test]
    fn the_star_is_not_touched_by_a_tag_reconcile() {
        let mut conn = seed_flag_state_database();

        apply_imap_flag_state_from_conn(
            &mut conn,
            "account-a",
            &[ImapMessageState {
                email_id: "imap:inbox:2".to_string(),
                unread: false,
                starred: true,
                tags: Some(Vec::new()),
            }],
        )
        .expect("apply flag state");

        assert_eq!(
            read_flag_state(&conn, "account-a", "imap:inbox:2"),
            (false, true)
        );
        assert!(read_tags(&conn, "account-a", "imap:inbox:2").is_empty());
    }

    #[test]
    fn label_membership_follows_one_mailbox_at_a_time() {
        let mut conn = seed_flag_state_database();
        conn.execute_batch(
            "INSERT INTO emails (id, account_id, thread_key, unread, label) VALUES
                 ('imap:archive:5', 'account-a', 'thread-5', 0, 'archive');
             INSERT INTO gmail_labels (account_id, id, name) VALUES
                 ('account-a', 'Work', 'Work');
             INSERT INTO email_labels (account_id, email_id, label_id) VALUES
                 ('account-a', 'imap:inbox:1', 'Work'),
                 ('account-a', 'imap:archive:5', 'Work');",
        )
        .expect("seed label membership");

        // The inbox search reports message 2 only: message 1 lost the label,
        // message 2 gained it, and the archived message is none of its business.
        let changed = set_label_membership_from_conn(
            &mut conn,
            "account-a",
            "Work",
            "inbox",
            &["imap:inbox:2".to_string()],
        )
        .expect("apply membership");

        assert_eq!(changed, 2);
        assert_eq!(read_tags(&conn, "account-a", "imap:inbox:1"), Vec::<String>::new());
        assert_eq!(read_tags(&conn, "account-a", "imap:inbox:2"), vec!["Work".to_string()]);
        // Cached under another label, so this search could not have seen it.
        assert_eq!(read_tags(&conn, "account-a", "imap:archive:5"), vec!["Work".to_string()]);
    }

    #[test]
    fn a_mailbox_checkpoint_round_trips_and_resets_with_its_uid_generation() {
        let conn = Connection::open_in_memory().expect("open memory db");
        conn.execute_batch(
            "CREATE TABLE imap_mailboxes (
                 account_id TEXT NOT NULL,
                 role TEXT NOT NULL,
                 mailbox TEXT NOT NULL,
                 uid_validity INTEGER NOT NULL DEFAULT 0,
                 uid_next INTEGER NOT NULL DEFAULT 0,
                 exists_count INTEGER NOT NULL DEFAULT 0,
                 highest_mod_seq INTEGER NOT NULL DEFAULT 0,
                 reconciled_at INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY (account_id, role)
             );
             INSERT INTO imap_mailboxes (account_id, role, mailbox)
             VALUES ('account-a', 'inbox', 'INBOX');",
        )
        .expect("seed mailbox");

        // A mailbox nobody has synced yet reads as all zeroes, which is what
        // forces the first full pass.
        assert_eq!(
            get_imap_mailbox_state_from_conn(&conn, "account-a", "inbox").expect("read state"),
            ImapMailboxState::default()
        );

        let synced = ImapMailboxState {
            uid_validity: 7,
            uid_next: 100,
            exists_count: 50,
            highest_mod_seq: 90_060_128_194_045_007,
            reconciled_at: 1_700_000_000,
        };
        set_imap_mailbox_state_from_conn(&conn, "account-a", "inbox", synced).expect("write state");
        // The sequence is wider than a u32 and has to survive the round trip
        // intact, or CHANGEDSINCE would ask about the wrong point in history.
        assert_eq!(
            get_imap_mailbox_state_from_conn(&conn, "account-a", "inbox").expect("read state"),
            synced
        );

        // Reassigned UIDs: the pass that rebuilds the cache writes the new
        // generation's numbers, and the old sequence must not survive it.
        let rebuilt = ImapMailboxState {
            uid_validity: 9,
            uid_next: 3,
            exists_count: 2,
            highest_mod_seq: 0,
            reconciled_at: 1_700_000_100,
        };
        set_imap_mailbox_state_from_conn(&conn, "account-a", "inbox", rebuilt)
            .expect("write state");
        assert_eq!(
            get_imap_mailbox_state_from_conn(&conn, "account-a", "inbox").expect("read state"),
            rebuilt
        );
    }

    #[test]
    fn delete_emails_by_ids_scopes_every_id_to_the_requested_account() {
        let mut conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch(
            "CREATE TABLE emails (
                id TEXT NOT NULL,
                account_id TEXT NOT NULL,
                PRIMARY KEY (account_id, id)
            );
            CREATE TABLE attachments (
                id TEXT NOT NULL,
                email_id TEXT NOT NULL,
                account_id TEXT NOT NULL,
                PRIMARY KEY (account_id, id)
            );
            INSERT INTO emails (id, account_id) VALUES
                ('delete-1', 'account-a'),
                ('delete-2', 'account-a'),
                ('keep-a', 'account-a'),
                ('delete-1', 'account-b');
            INSERT INTO attachments (id, email_id, account_id) VALUES
                ('delete-att-1', 'delete-1', 'account-a'),
                ('delete-att-2', 'delete-2', 'account-a'),
                ('keep-att-a', 'keep-a', 'account-a'),
                ('keep-att-b', 'delete-1', 'account-b');",
        )
        .expect("seed emails and attachments");

        let ids = vec!["delete-1".to_string(), "delete-2".to_string()];
        let deleted = delete_emails_by_ids_from_conn(&mut conn, "account-a", &ids)
            .expect("delete selected account emails");

        assert_eq!(deleted, 2);
        let remaining: Vec<(String, String)> = conn
            .prepare("SELECT account_id, id FROM emails ORDER BY account_id, id")
            .expect("prepare remaining rows")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query remaining rows")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("read remaining rows");
        assert_eq!(
            remaining,
            vec![
                ("account-a".to_string(), "keep-a".to_string()),
                ("account-b".to_string(), "delete-1".to_string()),
            ]
        );
        let remaining_attachments: Vec<(String, String)> = conn
            .prepare("SELECT account_id, id FROM attachments ORDER BY account_id, id")
            .expect("prepare remaining attachments")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query remaining attachments")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("read remaining attachments");
        assert_eq!(
            remaining_attachments,
            vec![
                ("account-a".to_string(), "keep-att-a".to_string()),
                ("account-b".to_string(), "keep-att-b".to_string()),
            ]
        );
    }

    #[test]
    fn orphaned_attachment_migration_keeps_only_account_scoped_mail_children() {
        let conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch(
            "CREATE TABLE emails (id TEXT NOT NULL, account_id TEXT NOT NULL);
             CREATE TABLE attachments (id TEXT, email_id TEXT, account_id TEXT);
             INSERT INTO emails VALUES ('shared-mail', 'account-a');
             INSERT INTO attachments VALUES
                 ('valid', 'shared-mail', 'account-a'),
                 ('wrong-account', 'shared-mail', 'account-b'),
                 ('missing-mail', 'missing', 'account-a');",
        )
        .expect("seed orphaned attachments");

        let deleted =
            purge_orphaned_attachments_from_conn(&conn).expect("purge orphaned attachments");

        assert_eq!(deleted, 2);
        let remaining: Vec<String> = conn
            .prepare("SELECT id FROM attachments ORDER BY id")
            .expect("prepare remaining attachments")
            .query_map([], |row| row.get(0))
            .expect("query remaining attachments")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("read remaining attachments");
        assert_eq!(remaining, vec!["valid".to_string()]);
    }

    #[test]
    fn account_generation_rejects_writes_from_a_removed_account_worker() {
        let conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch(
            "CREATE TABLE accounts (
                id TEXT PRIMARY KEY,
                cache_generation INTEGER NOT NULL
            );
            INSERT INTO accounts (id, cache_generation) VALUES ('account-a', 3);",
        )
        .expect("seed account generation");

        assert!(ensure_account_generation(&conn, "account-a", 3).is_ok());
        conn.execute(
            "UPDATE accounts SET cache_generation = 4 WHERE id = 'account-a'",
            [],
        )
        .expect("simulate account removal and re-add");

        assert!(ensure_account_generation(&conn, "account-a", 3).is_err());
        assert!(ensure_account_generation(&conn, "account-a", 4).is_ok());
    }

    #[test]
    fn removing_an_account_deletes_its_attachments_and_invalidates_its_generation() {
        let mut conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch(
            "CREATE TABLE accounts (id TEXT PRIMARY KEY, cache_generation INTEGER NOT NULL);
             CREATE TABLE account_generations (account_id TEXT PRIMARY KEY, generation INTEGER NOT NULL);
             CREATE TABLE emails (id TEXT, account_id TEXT);
             CREATE TABLE attachments (id TEXT, email_id TEXT, account_id TEXT);
             CREATE TABLE sync_state (account_id TEXT PRIMARY KEY);
             CREATE TABLE mailbox_cursors (account_id TEXT, label TEXT, next_page_token TEXT);
             INSERT INTO accounts VALUES ('account-a', 3), ('account-b', 1);
             INSERT INTO account_generations VALUES ('account-a', 3), ('account-b', 1);
             INSERT INTO emails VALUES ('mail-a', 'account-a'), ('mail-b', 'account-b');
             INSERT INTO attachments VALUES ('att-a', 'mail-a', 'account-a'), ('att-b', 'mail-b', 'account-b');
             INSERT INTO sync_state VALUES ('account-a'), ('account-b');
             INSERT INTO mailbox_cursors VALUES ('account-a', 'inbox', 'cursor-a'), ('account-b', 'inbox', 'cursor-b');",
        )
        .expect("seed account cache");

        remove_account_cache_from_conn(&mut conn, "account-a").expect("remove account cache");

        for table in [
            "accounts",
            "emails",
            "attachments",
            "sync_state",
            "mailbox_cursors",
        ] {
            let count: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE account_id = 'account-a'"),
                    [],
                    |row| row.get(0),
                )
                .or_else(|_| {
                    conn.query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE id = 'account-a'"),
                        [],
                        |row| row.get(0),
                    )
                })
                .expect("count removed account rows");
            assert_eq!(count, 0, "{table} still has removed-account data");
        }
        let generation: i64 = conn
            .query_row(
                "SELECT generation FROM account_generations WHERE account_id = 'account-a'",
                [],
                |row| row.get(0),
            )
            .expect("read invalidated generation");
        assert_eq!(generation, 4);
        let remaining_attachments: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM attachments WHERE account_id = 'account-b'",
                [],
                |row| row.get(0),
            )
            .expect("keep other account attachment");
        assert_eq!(remaining_attachments, 1);
    }

    #[test]
    fn orphaned_cache_diagnosis_counts_rows_without_returning_mail_content() {
        let conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch(
            "CREATE TABLE emails (id TEXT, account_id TEXT, label TEXT, unread BOOLEAN);
             CREATE TABLE attachments (id TEXT, account_id TEXT);
             INSERT INTO emails VALUES
                ('orphan-unread', '', 'inbox', 1),
                ('orphan-read', '', 'inbox', 0),
                ('owned-unread', 'account-a', 'inbox', 1);
             INSERT INTO attachments VALUES ('orphan-att', ''), ('owned-att', 'account-a');",
        )
        .expect("seed orphaned cache");

        assert_eq!(
            orphaned_cache_counts_from_conn(&conn).expect("count orphaned cache"),
            OrphanedCacheCounts {
                emails: 2,
                inbox_unread: 1,
                attachments: 1,
            }
        );
    }

    #[test]
    fn orphaned_cache_purge_removes_only_rows_without_an_account_owner() {
        let mut conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch(
            "CREATE TABLE emails (id TEXT, account_id TEXT);
             CREATE TABLE attachments (id TEXT, account_id TEXT);
             CREATE TABLE sync_state (account_id TEXT);
             CREATE TABLE mailbox_cursors (account_id TEXT);
             INSERT INTO emails VALUES ('orphan-mail', ''), ('owned-mail', 'account-a');
             INSERT INTO attachments VALUES ('orphan-att', ''), ('owned-att', 'account-a');
             INSERT INTO sync_state VALUES (''), ('account-a');
             INSERT INTO mailbox_cursors VALUES (''), ('account-a');",
        )
        .expect("seed cache rows");

        purge_orphaned_cache_from_conn(&mut conn).expect("purge orphaned cache");

        for table in ["emails", "attachments", "sync_state", "mailbox_cursors"] {
            let orphaned: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE account_id = ''"),
                    [],
                    |row| row.get(0),
                )
                .expect("count orphaned rows");
            let owned: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE account_id = 'account-a'"),
                    [],
                    |row| row.get(0),
                )
                .expect("count owned rows");
            assert_eq!(orphaned, 0, "{table} retained an orphaned row");
            assert_eq!(owned, 1, "{table} removed an owned row");
        }
    }

    #[test]
    fn local_search_covers_cached_metadata_and_respects_account_scope() {
        let conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch(
            "CREATE TABLE emails (
                id TEXT, thread_id TEXT, thread_key TEXT, sender TEXT, recipient TEXT, cc TEXT,
                subject TEXT, snippet TEXT, date INTEGER, unread BOOLEAN, label TEXT,
                account_id TEXT, reply_to TEXT DEFAULT '', message_id TEXT DEFAULT '',
                references_header TEXT DEFAULT ''
             );
             INSERT INTO emails (id, thread_id, sender, recipient, cc, subject, snippet, date, unread, label, account_id) VALUES
                ('a-subject', 't-a', 'Alice <alice@example.test>', '', '', 'Project Atlas', '', 30, 1, 'archive', 'account-a'),
                ('a-recipient', 't-b', 'Bob <bob@example.test>', 'team@example.test', '', 'Status', 'Weekly update', 20, 0, 'sent', 'account-a'),
                ('b-subject', 't-c', 'Carol <carol@example.test>', '', '', 'Project Atlas', '', 10, 1, 'inbox', 'account-b'),
                ('orphan', 't-d', 'Legacy', '', '', 'Project Atlas', '', 40, 1, 'inbox', '');",
        )
        .expect("seed cached summaries");

        let account_a = search_local_emails_from_conn(&conn, "atlas", Some("account-a"), 50)
            .expect("search selected account");
        assert_eq!(
            account_a
                .iter()
                .map(|email| email.id.as_str())
                .collect::<Vec<_>>(),
            ["a-subject"]
        );

        let all_accounts =
            search_local_emails_from_conn(&conn, "atlas", None, 50).expect("search all accounts");
        assert_eq!(
            all_accounts
                .iter()
                .map(|email| email.id.as_str())
                .collect::<Vec<_>>(),
            ["a-subject", "b-subject"]
        );

        let recipient =
            search_local_emails_from_conn(&conn, "team@example.test", Some("account-a"), 50)
                .expect("search recipient");
        assert_eq!(recipient.len(), 1);
        assert_eq!(recipient[0].id, "a-recipient");
    }

    #[test]
    fn full_text_search_finds_visible_body_content_without_indexing_html_noise() {
        assert_eq!(
            email_body_to_search_text(
                "<style>.fin{color:red}</style><p>Weekly Fin update &amp; notes</p>"
            ),
            "Weekly Fin update & notes"
        );
        assert_eq!(
            email_body_to_search_text("Contact Support <support@example.test> for help"),
            "Contact Support <support@example.test> for help"
        );

        let conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch(
            "CREATE TABLE emails (
                id TEXT, thread_id TEXT, thread_key TEXT, sender TEXT, recipient TEXT, cc TEXT,
                subject TEXT, snippet TEXT, body_text TEXT, date INTEGER,
                unread BOOLEAN, label TEXT, account_id TEXT,
                reply_to TEXT DEFAULT '', message_id TEXT DEFAULT '',
                references_header TEXT DEFAULT ''
             );
             CREATE VIRTUAL TABLE email_search USING fts5(
                sender, recipient, cc, subject, snippet, body_text,
                content='emails', content_rowid='rowid',
                tokenize='unicode61 remove_diacritics 2'
             );
             CREATE TABLE thread_summaries (
                account_id TEXT, thread_key TEXT, latest_email_id TEXT, latest_date INTEGER,
                unread_count INTEGER, message_count INTEGER, participants TEXT,
                PRIMARY KEY (account_id, thread_key)
             );
             INSERT INTO emails (id, thread_id, thread_key, sender, recipient, cc, subject, snippet, body_text, date, unread, label, account_id)
             VALUES
               ('body-match', 'thread-body', 'thread-body', 'Sender', 'me@example.test', '', 'Weekly update', 'Opening summary', 'The Fin report is ready', 10, 0, 'archive', 'account-a'),
               ('body-match-older', 'thread-body', 'thread-body', 'Earlier sender', 'me@example.test', '', 'Earlier update', 'Earlier summary', 'No matching term here', 8, 1, 'inbox', 'account-a'),
               ('second-match', 'thread-second', 'thread-second', 'Other sender', 'me@example.test', '', 'Second update', 'Second summary', 'Another Fin item', 5, 0, 'inbox', 'account-a');
             INSERT INTO thread_summaries VALUES
               ('account-a', 'thread-body', 'body-match', 10, 1, 2, 'Sender' || char(31) || 'Earlier sender'),
               ('account-a', 'thread-second', 'second-match', 5, 0, 1, 'Other sender');
             INSERT INTO email_search(email_search) VALUES('rebuild');",
        )
        .expect("seed full-text search");

        let groups =
            get_thread_groups_from_conn(&conn, None, Some("fin"), Some("account-a"), 1, None)
                .expect("search body content");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].latest_email.id, "body-match");
        assert_eq!(groups[0].count, 2);
        assert!(groups[0]
            .latest_email
            .snippet
            .to_lowercase()
            .contains("fin"));

        let cursor = &groups[0].latest_email;
        let next = get_thread_groups_from_conn(
            &conn,
            None,
            Some("fin"),
            Some("account-a"),
            1,
            Some((cursor.date, &cursor.account_id, &cursor.thread_id)),
        )
        .expect("paginate body search");
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].latest_email.id, "second-match");

        let address_match = get_thread_groups_from_conn(
            &conn,
            None,
            Some("me@example.test"),
            Some("account-a"),
            20,
            None,
        )
        .expect("search tokenized address");
        assert_eq!(address_match.len(), 2);
    }

    #[test]
    fn advanced_search_combines_structured_local_filters() {
        assert!(!AdvancedSearchCriteria::default().is_active());
        let conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch(
            "CREATE TABLE emails (
                id TEXT, thread_id TEXT, thread_key TEXT, sender TEXT, recipient TEXT, cc TEXT,
                subject TEXT, snippet TEXT, body_text TEXT, date INTEGER,
                unread BOOLEAN, label TEXT, account_id TEXT,
                reply_to TEXT DEFAULT '', message_id TEXT DEFAULT '',
                references_header TEXT DEFAULT ''
             );
             CREATE VIRTUAL TABLE email_search USING fts5(
                sender, recipient, cc, subject, snippet, body_text,
                content='emails', content_rowid='rowid'
             );
             CREATE TABLE thread_summaries (
                account_id TEXT, thread_key TEXT, latest_email_id TEXT, latest_date INTEGER,
                unread_count INTEGER, message_count INTEGER, participants TEXT,
                PRIMARY KEY (account_id, thread_key)
             );
             CREATE TABLE attachments (id TEXT, email_id TEXT, account_id TEXT);
             CREATE TABLE email_labels (account_id TEXT, email_id TEXT, label_id TEXT);
             INSERT INTO emails (id, thread_id, thread_key, sender, recipient, cc, subject, snippet, body_text, date, unread, label, account_id) VALUES
               ('match', 'thread-match', 'thread-match', 'Alice <alice@example.test>', 'me@example.test', '', 'Quarterly plan', '', 'green budget notes', 2000, 1, 'inbox', 'account-a'),
               ('other', 'thread-other', 'thread-other', 'Bob <bob@example.test>', 'me@example.test', '', 'Quarterly plan', '', 'red budget notes', 2100, 1, 'inbox', 'account-a');
             INSERT INTO thread_summaries VALUES
               ('account-a', 'thread-match', 'match', 2000, 1, 1, 'Alice'),
               ('account-a', 'thread-other', 'other', 2100, 1, 1, 'Bob');
             INSERT INTO attachments VALUES ('attachment', 'match', 'account-a');
             INSERT INTO email_labels VALUES ('account-a', 'match', 'STARRED');
             INSERT INTO email_search(email_search) VALUES('rebuild');",
        )
        .expect("seed advanced search cache");

        let filters = AdvancedSearchCriteria {
            from: "alice".to_string(),
            includes: "budget".to_string(),
            excludes: "red".to_string(),
            after_date: Some(1000),
            before_date: Some(3000),
            location: "inbox".to_string(),
            has_attachment: true,
            unread: true,
            starred: true,
            ..AdvancedSearchCriteria::default()
        };
        let results =
            search_thread_groups_from_conn(&conn, "", Some(&filters), Some("account-a"), 20, None)
                .expect("run advanced search");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].latest_email.id, "match");

        let subject_exclusion = AdvancedSearchCriteria {
            excludes: "quarterly".to_string(),
            ..AdvancedSearchCriteria::default()
        };
        assert!(search_thread_groups_from_conn(
            &conn,
            "",
            Some(&subject_exclusion),
            Some("account-a"),
            20,
            None,
        )
        .expect("exclude a subject term")
        .is_empty());

        let sender_exclusion = AdvancedSearchCriteria {
            excludes: "alice".to_string(),
            ..AdvancedSearchCriteria::default()
        };
        let without_alice = search_thread_groups_from_conn(
            &conn,
            "",
            Some(&sender_exclusion),
            Some("account-a"),
            20,
            None,
        )
        .expect("exclude a sender term");
        assert_eq!(without_alice.len(), 1);
        assert_eq!(without_alice[0].latest_email.id, "other");
    }

    #[test]
    fn thread_summary_refresh_tracks_latest_counts_and_participants() {
        let conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch(
            "CREATE TABLE emails (
                id TEXT, account_id TEXT, thread_key TEXT, sender TEXT,
                date INTEGER, unread BOOLEAN, label TEXT
             );
             CREATE INDEX thread_lookup ON emails(account_id, thread_key, date DESC);
             CREATE TABLE thread_summaries (
                account_id TEXT, thread_key TEXT, latest_email_id TEXT, latest_date INTEGER,
                unread_count INTEGER, message_count INTEGER, participants TEXT,
                PRIMARY KEY (account_id, thread_key)
             );
             INSERT INTO emails VALUES
                ('older', 'account-a', 'thread-a', 'Alice', 10, 1, 'inbox'),
                ('newer', 'account-a', 'thread-a', 'Bob', 20, 0, 'archive'),
                ('draft', 'account-a', 'thread-a', 'Me', 30, 0, 'draft');",
        )
        .expect("seed thread messages");

        refresh_thread_summary(&conn, "account-a", "thread-a").expect("refresh summary");
        let summary: (String, i64, i64, i64, String) = conn
            .query_row(
                "SELECT latest_email_id, latest_date, unread_count, message_count, participants
                 FROM thread_summaries WHERE account_id = 'account-a' AND thread_key = 'thread-a'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("read summary");
        assert_eq!(summary.0, "newer");
        assert_eq!((summary.1, summary.2, summary.3), (20, 1, 2));
        let participants: std::collections::HashSet<&str> = summary.4.split('\u{1f}').collect();
        assert_eq!(
            participants,
            std::collections::HashSet::from(["Alice", "Bob"])
        );

        conn.execute("DELETE FROM emails WHERE label != 'draft'", [])
            .expect("delete visible messages");
        refresh_thread_summary(&conn, "account-a", "thread-a").expect("remove empty summary");
        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM thread_summaries", [], |row| {
                row.get(0)
            })
            .expect("count summaries");
        assert_eq!(remaining, 0);
    }

    #[test]
    fn cancelled_search_generation_cannot_register_late() {
        let coordinator = SearchCoordinator::default();
        let generation = coordinator.reserve();
        coordinator.cancel();
        let conn = Connection::open_in_memory().expect("open in-memory database");
        assert!(!coordinator.register(generation, conn.get_interrupt_handle()));
    }

    #[test]
    fn thread_groups_are_aggregated_and_keyset_paginated() {
        let conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch(
            "CREATE TABLE emails (
                id TEXT, thread_id TEXT, thread_key TEXT, sender TEXT, recipient TEXT, cc TEXT,
                subject TEXT, snippet TEXT, body_text TEXT, date INTEGER, unread BOOLEAN, label TEXT,
                account_id TEXT, reply_to TEXT DEFAULT '', message_id TEXT DEFAULT '',
                references_header TEXT DEFAULT ''
             );
             CREATE VIRTUAL TABLE email_search USING fts5(
                sender, recipient, cc, subject, snippet, body_text,
                content='emails', content_rowid='rowid'
             );
             CREATE TABLE thread_summaries (
                account_id TEXT, thread_key TEXT, latest_email_id TEXT, latest_date INTEGER,
                unread_count INTEGER, message_count INTEGER, participants TEXT,
                PRIMARY KEY (account_id, thread_key)
             );
             INSERT INTO emails (id, thread_id, thread_key, sender, recipient, cc, subject, snippet, body_text, date, unread, label, account_id) VALUES
                ('t1-old', 'thread-1', 'thread-1', 'Alice <alice@example.test>', '', '', 'First', 'old', '', 10, 1, 'inbox', 'account-a'),
                ('t1-new', 'thread-1', 'thread-1', 'Bob <bob@example.test>', '', '', 'First', 'new', '', 40, 0, 'inbox', 'account-a'),
                ('t2', 'thread-2', 'thread-2', 'Carol <carol@example.test>', '', '', 'Second', '', '', 30, 1, 'inbox', 'account-a'),
                ('t3', 'thread-3', 'thread-3', 'Dave <dave@example.test>', '', '', 'Third', '', '', 20, 0, 'inbox', 'account-a'),
                ('draft', 'thread-3', 'thread-3', 'Me <me@example.test>', '', '', 'Draft reply', '', '', 25, 0, 'draft', 'account-a'),
                ('sent', 'thread-4', 'thread-4', 'Eve', '', '', 'Sent', '', '', 50, 1, 'sent', 'account-a'),
                ('spam', 'thread-5', 'thread-5', 'Spam', '', '', 'Spam', '', '', 60, 1, 'spam', 'account-a'),
                ('trash', 'thread-6', 'thread-6', 'Trash', '', '', 'Trash', '', '', 55, 0, 'trash', 'account-a');
             INSERT INTO thread_summaries VALUES
                ('account-a', 'thread-1', 't1-new', 40, 1, 2, 'Alice <alice@example.test>' || char(31) || 'Bob <bob@example.test>'),
                ('account-a', 'thread-2', 't2', 30, 1, 1, 'Carol <carol@example.test>'),
                ('account-a', 'thread-3', 't3', 20, 0, 1, 'Dave <dave@example.test>'),
                ('account-a', 'thread-4', 'sent', 50, 1, 1, 'Eve'),
                ('account-a', 'thread-5', 'spam', 60, 1, 1, 'Spam'),
                ('account-a', 'thread-6', 'trash', 55, 0, 1, 'Trash');
             INSERT INTO email_search(email_search) VALUES('rebuild');",
        )
        .expect("seed conversation summaries");

        let first =
            get_thread_groups_from_conn(&conn, Some("inbox"), None, Some("account-a"), 2, None)
                .expect("load first conversation page");
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].latest_email.id, "t1-new");
        assert_eq!(first[0].count, 2);
        assert_eq!(first[0].unread_count, 1);
        assert!(first[0].has_unread);
        assert_eq!(first[0].participants, ["Alice", "Bob"]);
        assert_eq!(first[1].latest_email.id, "t2");

        let cursor = &first[1];
        let second = get_thread_groups_from_conn(
            &conn,
            Some("inbox"),
            None,
            Some("account-a"),
            2,
            Some((
                cursor.latest_email.date,
                &cursor.latest_email.account_id,
                &cursor.latest_email.thread_id,
            )),
        )
        .expect("load second conversation page");
        assert_eq!(
            second
                .iter()
                .map(|group| group.latest_email.id.as_str())
                .collect::<Vec<_>>(),
            ["t3"]
        );

        let all_mail =
            get_thread_groups_from_conn(&conn, Some("all"), None, Some("account-a"), 20, None)
                .expect("load all mail without spam, trash, or drafts");
        assert_eq!(
            all_mail
                .iter()
                .map(|group| group.latest_email.id.as_str())
                .collect::<Vec<_>>(),
            ["sent", "t1-new", "t2", "t3"],
        );

        let drafts = get_thread_groups_from_conn(
            &conn,
            None,
            Some("Draft reply"),
            Some("account-a"),
            10,
            None,
        )
        .expect("search conversations without draft cards");
        assert!(drafts.is_empty());

        conn.execute_batch(
            "CREATE TABLE email_labels (
                 account_id TEXT, email_id TEXT, label_id TEXT
             );
             INSERT INTO email_labels VALUES
                 ('account-a', 't1-old', 'Label_9'),
                 ('account-a', 't1-new', 'Label_9'),
                 ('account-a', 't2', 'STARRED');",
        )
        .expect("seed Gmail labels");
        let labeled = get_thread_groups_from_conn(
            &conn,
            Some("gmail:Label_9"),
            None,
            Some("account-a"),
            10,
            None,
        )
        .expect("filter conversations by Gmail label");
        assert_eq!(labeled.len(), 1);
        assert_eq!(labeled[0].latest_email.id, "t1-new");
        assert_eq!(labeled[0].label_ids, ["Label_9"]);

        let starred =
            get_thread_groups_from_conn(&conn, Some("starred"), None, Some("account-a"), 10, None)
                .expect("filter starred conversations");
        assert_eq!(starred.len(), 1);
        assert_eq!(starred[0].latest_email.id, "t2");
        assert_eq!(starred[0].label_ids, ["STARRED"]);
    }

    #[test]
    fn contact_suggestions_are_scoped_to_the_sending_account() {
        let conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch(
            "CREATE TABLE emails (sender TEXT, recipient TEXT, label TEXT, account_id TEXT);
             INSERT INTO emails VALUES
               ('Alice <alice@example.test>', '', 'inbox', 'account-a'),
               ('Bob <bob@example.test>', '', 'inbox', 'account-b'),
               ('', 'carol@example.test', 'sent', 'account-a'),
               ('', 'dave@example.test', 'sent', 'account-b');",
        )
        .expect("seed contacts");

        let account_a = search_contacts_from_conn(&conn, "example.test", "account-a")
            .expect("search account a contacts");
        let mut account_a_emails: Vec<&str> = account_a
            .iter()
            .map(|contact| contact.email.as_str())
            .collect();
        account_a_emails.sort_unstable();
        assert_eq!(
            account_a_emails,
            ["alice@example.test", "carol@example.test"]
        );

        let account_b = search_contacts_from_conn(&conn, "example.test", "account-b")
            .expect("search account b contacts");
        let mut account_b_emails: Vec<&str> = account_b
            .iter()
            .map(|contact| contact.email.as_str())
            .collect();
        account_b_emails.sort_unstable();
        assert_eq!(account_b_emails, ["bob@example.test", "dave@example.test"]);
    }

    #[test]
    fn resetting_local_cache_clears_all_accounts_and_invalidates_generations() {
        let mut conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch(
            "CREATE TABLE accounts (id TEXT PRIMARY KEY, cache_generation INTEGER NOT NULL);
             CREATE TABLE account_generations (account_id TEXT PRIMARY KEY, generation INTEGER NOT NULL);
             CREATE TABLE emails (id TEXT, account_id TEXT);
             CREATE TABLE attachments (id TEXT, account_id TEXT);
             CREATE TABLE sync_state (account_id TEXT);
             CREATE TABLE mailbox_cursors (account_id TEXT);
             INSERT INTO accounts VALUES ('account-a', 4), ('account-b', 9);
             INSERT INTO account_generations VALUES ('account-a', 4), ('account-b', 9);
             INSERT INTO emails VALUES ('a-mail', 'account-a'), ('b-mail', 'account-b');
             INSERT INTO attachments VALUES ('a-attachment', 'account-a'), ('b-attachment', 'account-b');
             INSERT INTO sync_state VALUES ('account-a'), ('account-b');
             INSERT INTO mailbox_cursors VALUES ('account-a'), ('account-b');",
        )
        .expect("seed local cache");

        let invalidated =
            reset_local_mail_cache_from_conn(&mut conn, None).expect("reset all local cache");
        assert_eq!(
            invalidated,
            vec!["account-a".to_string(), "account-b".to_string()]
        );
        for table in ["emails", "attachments", "sync_state", "mailbox_cursors"] {
            let count: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("count cleared cache rows");
            assert_eq!(count, 0, "{table} was not cleared");
        }
        let generations: Vec<(String, i64)> = conn
            .prepare("SELECT id, cache_generation FROM accounts ORDER BY id")
            .expect("prepare generation query")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("query generations")
            .filter_map(Result::ok)
            .collect();
        assert_eq!(
            generations,
            vec![("account-a".to_string(), 5), ("account-b".to_string(), 10)]
        );
    }

    #[test]
    fn unread_badge_uses_gmail_message_count_with_local_fallback_per_account() {
        let conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch(
            "CREATE TABLE accounts (id TEXT PRIMARY KEY);
             CREATE TABLE sync_state (
                account_id TEXT PRIMARY KEY,
                gmail_inbox_messages_unread INTEGER,
                gmail_inbox_threads_unread INTEGER
             );
             CREATE TABLE emails (account_id TEXT, label TEXT, unread BOOLEAN);
             INSERT INTO accounts VALUES ('account-a'), ('account-b');
             INSERT INTO sync_state VALUES ('account-a', 5, 2);
             INSERT INTO emails VALUES
                ('account-a', 'inbox', 1), ('account-a', 'inbox', 1),
                ('account-b', 'inbox', 1), ('account-b', 'inbox', 1), ('account-b', 'inbox', 1),
                ('', 'inbox', 1);",
        )
        .expect("seed unread counts");

        assert_eq!(
            inbox_unread_count_from_conn(&conn, Some("account-a")).unwrap(),
            5
        );
        assert_eq!(
            inbox_unread_count_from_conn(&conn, Some("account-b")).unwrap(),
            3
        );
        assert_eq!(inbox_unread_count_from_conn(&conn, None).unwrap(), 8);
    }
}

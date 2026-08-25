import { invoke } from "@tauri-apps/api/core";
import type { Account, AppControls, AttachmentPayload, AuthInfo, CustomMailbox, DraftContent, DraftPage, EmailSummary, GmailLabel, SavedDraft, SendOutcome, ThreadGroup } from "./types";
import type { AdvancedSearchCriteria } from "./advancedSearch";

export interface MailboxDownloadStatus {
  running: boolean;
  pending: boolean;
  state: "waiting" | "running" | "paused" | "error" | "completed" | "relogin_required" | "rate_limited";
  retryAfter: number | null;
}

export interface EmailPageInput {
  label: string;
  accountId: string | null;
  limit?: number;
  beforeDate?: EmailSummary["date"] | null;
  beforeAccountId?: string | null;
  beforeId?: string | null;
}

export interface ContactSuggestion {
  name: string;
  email: string;
}

export interface EmailAttachmentInfo {
  id: string;
  filename: string;
  mime_type: string;
  size: number;
  attachment_id: string | null;
  data: string | null;
}

export interface SavedAttachment {
  fileName: string;
  revealed: boolean;
}

export interface CustomNotificationInput {
  title: string;
  body: string;
  kind: "mail" | "update" | "summary";
  code?: string | null;
  emailId?: string | null;
  duration: number;
  accountId?: string | null;
  accountPicture?: string | null;
  multiAccount?: boolean;
  copyLabel?: string;
  copiedLabel?: string;
  copyFailedLabel?: string;
  dismissAllLabel?: string;
}

/** Emitted by the backend watcher after a server-reported change reached the local cache. */
export interface ImapChangeEvent {
  accountId: string;
}

export type MailSecurity = "tls" | "starttls";

export interface ImapAccountInput {
  email: string;
  username: string;
  password: string;
  imapHost: string;
  imapPort: number;
  imapSecurity: MailSecurity;
  smtpHost: string;
  smtpPort: number;
  smtpSecurity: MailSecurity;
}

export interface MailConnectionReport {
  imapOk: boolean;
  smtpOk: boolean;
  mailboxCount: number;
}

export type DiscoveredMailProvider = "google" | "microsoft" | "yahoo" | "icloud" | "manual";

export interface ProviderDiscovery {
  email: string;
  provider: DiscoveredMailProvider;
  authType: "oauth" | "app_password" | "manual";
}

export interface ThreadPageInput {
  label: string;
  accountId: string | null;
  limit?: number;
  beforeDate?: number | null;
  beforeAccountId?: string | null;
  beforeThreadId?: string | null;
}

export interface ThreadSearchInput {
  query: string;
  filters?: AdvancedSearchCriteria;
  accountId: string | null;
  limit?: number;
  beforeDate?: number | null;
  beforeAccountId?: string | null;
  beforeThreadId?: string | null;
}

export const tauriApi = {
  discoverMailProvider: (email: string) =>
    invoke<ProviderDiscovery>("discover_mail_provider", { email }),
  startMailOAuth: (email: string, provider: DiscoveredMailProvider) =>
    invoke<AuthInfo>("start_mail_oauth", { email, provider }),
  cancelMailOAuth: () => invoke<void>("cancel_mail_oauth"),
  testMailAccount: (input: ImapAccountInput) =>
    invoke<MailConnectionReport>("test_mail_account", { input }),
  addMailAccount: (input: ImapAccountInput) =>
    invoke<Account>("add_mail_account", { input }),
  syncImapEmails: (accountId: string) =>
    invoke<void>("sync_imap_emails", { accountId }),
  startImapWatch: (accountId: string, mailboxRole = "inbox") =>
    invoke<void>("start_imap_watch", { accountId, mailboxRole }),
  stopImapWatch: (accountId: string, mailboxRole = "inbox") =>
    invoke<void>("stop_imap_watch", { accountId, mailboxRole }),
  wakeImapWatchers: () => invoke<void>("wake_imap_watchers"),
  getAccounts: () => invoke<Account[]>("get_accounts"),
  getAccountAuth: (accountId: string) =>
    invoke<AuthInfo | null>("get_account_auth", { accountId }),
  refreshAccessToken: (accountId: string) =>
    invoke<AuthInfo>("refresh_access_token", { accountId }),
  removeAccount: (accountId: string) =>
    invoke<void>("remove_account", { accountId }),
  reorderAccounts: (orderedIds: string[]) =>
    invoke<void>("reorder_accounts", { orderedIds }),
  getInboxEmailKeys: (accountId: string | null, limit?: number) =>
    invoke<Array<{ accountId: string; id: string }>>("get_inbox_email_keys", { accountId, limit }),
  getEmailsByLabel: (input: EmailPageInput) =>
    invoke<EmailSummary[]>("get_emails_by_label", { ...input }),
  getThreadGroupsByLabel: (input: ThreadPageInput) =>
    invoke<ThreadGroup[]>("get_thread_groups_by_label", { ...input }),
  getCustomImapMailboxes: (accountId: string) =>
    invoke<CustomMailbox[]>("get_custom_imap_mailboxes", { accountId }),
  getGmailLabels: (accountId: string) =>
    invoke<GmailLabel[]>("get_gmail_labels", { accountId }),
  createGmailLabel: (accountId: string, name: string) =>
    invoke<GmailLabel>("create_gmail_label", { accountId, name }),
  renameGmailLabel: (accountId: string, labelId: string, name: string) =>
    invoke<GmailLabel>("rename_gmail_label", { accountId, labelId, name }),
  setGmailLabelColor: (
    accountId: string,
    labelId: string,
    backgroundColor: string | null,
    textColor: string | null,
  ) => invoke<GmailLabel>("set_gmail_label_color", {
    accountId, labelId, backgroundColor, textColor,
  }),
  deleteGmailLabel: (accountId: string, labelId: string) =>
    invoke<void>("delete_gmail_label", { accountId, labelId }),
  setThreadGmailLabel: (accountId: string, threadId: string, labelId: string, applied: boolean) =>
    invoke<void>("set_thread_gmail_label", { accountId, threadId, labelId, applied }),
  setThreadStarred: (accountId: string, threadId: string, starred: boolean) =>
    invoke<void>("set_thread_gmail_label", {
      accountId,
      threadId,
      labelId: "STARRED",
      applied: starred,
    }),
  searchLocalEmails: (query: string, accountId: string | null, limit: number) =>
    invoke<EmailSummary[]>("search_local_emails", { query, accountId, limit }),
  searchLocalThreadGroups: (input: ThreadSearchInput) =>
    invoke<ThreadGroup[]>("search_local_thread_groups", { ...input }),
  cancelLocalSearch: () => invoke<void>("cancel_local_search"),
  searchContacts: (query: string, accountId: string) =>
    invoke<ContactSuggestion[]>("search_contacts", { query, accountId }),
  getMailboxDownloadStatus: (accountId: string | null) =>
    invoke<MailboxDownloadStatus>("get_mailbox_download_status", { accountId }),
  resetLocalMailCache: (accountId: string | null) =>
    invoke<void>("reset_local_mail_cache", { accountId }),
  syncEmails: (accountId: string, force: boolean) =>
    invoke<void>("sync_emails", { accountId, force }),
  getInboxUnreadCount: (accountId: string | null) =>
    invoke<number>("get_inbox_unread_count", { accountId }),
  getEmailBody: (id: string, accountId: string) =>
    invoke<string>("get_email_body", { id, accountId }),
  getEmailAttachments: (emailId: string, accountId: string) =>
    invoke<EmailAttachmentInfo[]>("get_email_attachments", { emailId, accountId }),
  fetchAttachmentData: (emailId: string, accountId: string, attachmentDbId: string) =>
    invoke<string>("fetch_attachment_data", { emailId, accountId, attachmentDbId }),
  saveAndRevealAttachment: (emailId: string, accountId: string, attachmentDbId: string) =>
    invoke<SavedAttachment>("save_and_reveal_attachment", { emailId, accountId, attachmentDbId }),
  getThreadEmails: (threadId: string, accountId: string, limit = 20, offset = 0) =>
    invoke<EmailSummary[]>("get_thread_emails", { threadId, accountId, limit, offset }),
  markAsRead: (accountId: string, messageId: string) =>
    invoke<void>("mark_as_read", { accountId, messageId }),
  markThreadAsRead: (accountId: string, threadId: string) =>
    invoke<void>("mark_thread_as_read", { accountId, threadId }),
  markAsUnread: (accountId: string, threadId: string) =>
    invoke<void>("mark_as_unread", { accountId, threadId }),
  archiveEmail: (accountId: string, threadId: string) =>
    invoke<void>("archive_email", { accountId, threadId }),
  reportSpam: (accountId: string, threadId: string) =>
    invoke<void>("report_spam", { accountId, threadId }),
  trashEmail: (accountId: string, threadId: string) =>
    invoke<void>("trash_email", { accountId, threadId }),
  moveToInbox: (accountId: string, threadId: string) =>
    invoke<void>("move_to_inbox", { accountId, threadId }),
  moveToMailbox: (accountId: string, threadId: string, mailboxRole: string) =>
    invoke<void>("move_to_mailbox", { accountId, threadId, mailboxRole }),
  sendReply: (input: {
    accountId: string;
    to: string;
    cc: string;
    subject: string;
    body: string;
    threadId: string;
    inReplyTo: string;
    references: string;
    attachments: AttachmentPayload[] | null;
  }) => invoke<SendOutcome>("send_reply", { ...input }),
  sendEmail: (input: {
    accountId: string;
    to: string;
    cc: string;
    bcc: string;
    subject: string;
    body: string;
    attachments: AttachmentPayload[] | null;
  }) => invoke<SendOutcome>("send_email", { ...input }),
  listDrafts: (accountId: string, pageToken: string | null = null) =>
    invoke<DraftPage>("list_drafts", { accountId, pageToken }),
  getDraft: (accountId: string, draftId: string) =>
    invoke<DraftContent>("get_draft", { accountId, draftId }),
  saveDraft: (input: {
    accountId: string;
    draftId: string | null;
    to: string;
    cc: string;
    bcc: string;
    subject: string;
    body: string;
    attachments: AttachmentPayload[] | null;
    threadId?: string | null;
    inReplyTo?: string | null;
    references?: string | null;
  }) => invoke<SavedDraft>("save_draft", {
    ...input,
    threadId: input.threadId ?? null,
    inReplyTo: input.inReplyTo ?? null,
    references: input.references ?? null,
  }),
  sendDraft: (accountId: string, draftId: string, verificationMessageId: string) =>
    invoke<SendOutcome>("send_draft", { accountId, draftId, verificationMessageId }),
  deleteDraft: (accountId: string, draftId: string) =>
    invoke<void>("delete_draft", { accountId, draftId }),
  getLaunchAtStartup: () => invoke<boolean>("get_launch_at_startup"),
  setLaunchAtStartup: (enabled: boolean) =>
    invoke<boolean>("set_launch_at_startup", { enabled }),
  openDefaultMailSettings: () => invoke<void>("open_default_mail_settings"),
  getAppControls: () => invoke<AppControls>("get_app_controls"),
  setAppControls: (controls: Partial<AppControls>) =>
    invoke<AppControls>("set_app_controls", { controls }),
  setAppLanguage: (language: AppControls["appLanguage"]) =>
    invoke<AppControls>("set_app_language", { language }),
  isSystemFullscreen: () => invoke<boolean>("is_system_fullscreen"),
  setUnreadIndicator: (count: number, tooltip: string) =>
    invoke<void>("set_unread_indicator", { count, tooltip }),
  showCustomNotification: (notification: CustomNotificationInput) =>
    invoke<void>("show_custom_notification", { ...notification }),
};

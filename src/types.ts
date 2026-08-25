export type OtpMode = "off" | "balanced" | "strict";
export type NotificationMode = "all" | "otpOnly" | "off";
export type RenderMode = "full" | "simple";
export type RemoteImageMode = "always" | "trusted" | "ask";
export type MailZoom = "fit" | number;
export type DensityMode = "comfortable" | "compact";
export type MailViewMode = "split" | "single-toggle" | "inbox-first";
export type MailViewPreference = "auto" | MailViewMode;

export interface Account {
  id: string;       // same as email
  email: string;
  picture: string;
  display_order: number;
  provider: "imap" | "google";
}

export interface EmailSummary {
  id: string;
  thread_id: string;
  sender: string;
  recipient: string;
  cc: string;
  reply_to: string;
  message_id: string;
  references: string;
  subject: string;
  snippet: string;
  date: number;
  unread: boolean;
  label: string;
  account_id: string;
}

export interface GmailLabel {
  id: string;
  account_id: string;
  name: string;
  background_color: string | null;
  text_color: string | null;
}

// A user-named IMAP folder with no recognized system role (not Inbox/Sent/
// Archive/etc.). `role` is the label to pass to `getEmailsByLabel`.
export interface CustomMailbox {
  role: string;
  name: string;
}

export interface ThreadGroup {
  latestEmail: EmailSummary;
  hasUnread: boolean;
  unreadCount: number;
  count: number;
  participants: string[];
  labelIds: string[];
}

export interface AuthInfo {
  authenticated: boolean;
  expires_at: number | null;
  email: string;
  picture: string;
}

export interface AppControls {
  notificationMode: NotificationMode;
  mailSyncPaused: boolean;
  quietHoursEnabled: boolean;
  quietHoursStart: string;
  quietHoursEnd: string;
  appLanguage: "en" | "tr";
}

export interface AttachmentPayload {
  filename: string;
  mimeType: string;
  data: string; // base64
}

export interface DraftSummary {
  id: string;
  messageId: string;
  rfcMessageId: string;
  threadId: string;
  inReplyTo: string;
  references: string;
  to: string;
  cc: string;
  bcc: string;
  subject: string;
  snippet: string;
  updatedAt: number;
}

export interface DraftPage {
  drafts: DraftSummary[];
  nextPageToken: string | null;
}

export interface DraftContent extends DraftSummary {
  body: string;
  attachments: AttachmentPayload[];
}

export interface SavedDraft {
  id: string;
  messageId: string;
  verificationMessageId: string;
  updatedAt: number;
}

export interface SendOutcome {
  status: "sent" | "outcome_unknown";
  messageId: string;
}

export const DEFAULT_APP_CONTROLS: AppControls = {
  notificationMode: "all",
  mailSyncPaused: false,
  quietHoursEnabled: false,
  quietHoursStart: "22:00",
  quietHoursEnd: "08:00",
  appLanguage: "en",
};

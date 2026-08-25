import type { EmailSummary } from "./types";

export const MAX_KNOWN_EMAIL_IDS = 10_000;

export function emailCacheKey(email: Pick<EmailSummary, "account_id" | "id">): string {
  return `${email.account_id}\u0000${email.id}`;
}

interface UpdateNotificationBaselineOptions {
  freshInbox: EmailSummary[];
  knownEmailIds: Set<string>;
  readyAccountIds: Set<string>;
  successfullySyncedAccountIds: Set<string>;
  suppressNotifications: boolean;
}

export function updateNotificationBaseline(options: UpdateNotificationBaselineOptions): EmailSummary[] {
  const {
    freshInbox, knownEmailIds, readyAccountIds,
    successfullySyncedAccountIds, suppressNotifications,
  } = options;
  const newUnreadEmails = freshInbox.filter(email =>
    !suppressNotifications && email.unread && readyAccountIds.has(email.account_id) &&
    !knownEmailIds.has(emailCacheKey(email))
  );

  for (const email of freshInbox) knownEmailIds.add(emailCacheKey(email));
  while (knownEmailIds.size > MAX_KNOWN_EMAIL_IDS) {
    const oldest = knownEmailIds.values().next().value;
    if (oldest === undefined) break;
    knownEmailIds.delete(oldest);
  }
  for (const accountId of successfullySyncedAccountIds) readyAccountIds.add(accountId);
  return newUnreadEmails;
}

/// The newest inbox date this app has accounted for, per account. Remembering
/// it is what lets a later run tell mail that arrived while it was closed from
/// mail the user has already been told about.
export function latestInboxDates(freshInbox: EmailSummary[]): Record<string, number> {
  const latest: Record<string, number> = {};
  for (const email of freshInbox) {
    const known = latest[email.account_id] ?? 0;
    if (email.date > known) latest[email.account_id] = email.date;
  }
  return latest;
}

/// How many unread mails in these accounts arrived after this app last had
/// something to say. An account with no recorded mark is a first run: nothing
/// counts as missed, since the user was never told anything to begin with.
export function countMissedMail(
  freshInbox: EmailSummary[],
  accountIds: ReadonlySet<string>,
  lastSeen: Record<string, number>,
): number {
  return freshInbox.filter(email => {
    if (!email.unread || !accountIds.has(email.account_id)) return false;
    const mark = lastSeen[email.account_id];
    return mark !== undefined && email.date > mark;
  }).length;
}

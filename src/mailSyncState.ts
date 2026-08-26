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
export function latestInboxDates(freshInbox: EmailSummary[], now = Date.now()): Record<string, number> {
  const latest: Record<string, number> = {};
  for (const email of freshInbox) {
    // A message dates itself, and spam routinely dates itself years ahead. One
    // of those would push the mark past every mail that follows it and silence
    // the catch-up summary for good, so the mark never runs ahead of the clock.
    const date = Math.min(email.date, now);
    const known = latest[email.account_id] ?? 0;
    if (date > known) latest[email.account_id] = date;
  }
  return latest;
}

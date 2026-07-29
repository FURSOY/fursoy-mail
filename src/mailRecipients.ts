import type { EmailSummary } from "./types";

export interface ReplyRecipients {
  to: string[];
  cc: string[];
  canReplyAll: boolean;
}

function splitMailboxList(value: string): string[] {
  const parts: string[] = [];
  let current = "";
  let quoted = false;
  let angleDepth = 0;
  for (const character of value) {
    if (character === '"') quoted = !quoted;
    if (!quoted && character === "<") angleDepth += 1;
    if (!quoted && character === ">") angleDepth = Math.max(0, angleDepth - 1);
    if (!quoted && angleDepth === 0 && (character === "," || character === ";")) {
      if (current.trim()) parts.push(current.trim());
      current = "";
    } else {
      current += character;
    }
  }
  if (current.trim()) parts.push(current.trim());
  return parts;
}

export function mailboxAddress(value: string): string {
  const angle = value.match(/<\s*([^<>]+?)\s*>/);
  return (angle?.[1] ?? value).trim().replace(/^mailto:/i, "");
}

export function parseMailboxList(value: string): string[] {
  return splitMailboxList(value)
    .map(mailboxAddress)
    .filter(Boolean);
}

function uniqueWithoutOwn(values: string[], ownAddress: string): string[] {
  const own = ownAddress.trim().toLowerCase();
  const seen = new Set<string>();
  return values.filter(value => {
    const normalized = value.toLowerCase();
    if (!normalized || normalized === own || seen.has(normalized)) return false;
    seen.add(normalized);
    return true;
  });
}

export function calculateReplyRecipients(email: EmailSummary): ReplyRecipients {
  const ownAddress = email.account_id;
  const originalTo = uniqueWithoutOwn(parseMailboxList(email.recipient), ownAddress);
  const preferredReplyTargets = uniqueWithoutOwn(
    parseMailboxList(email.reply_to || email.sender),
    ownAddress,
  );
  const replyTargets = preferredReplyTargets.length > 0
    ? preferredReplyTargets
    : originalTo.slice(0, 1);

  const allTo = uniqueWithoutOwn([...replyTargets, ...originalTo], ownAddress);
  const toKeys = new Set(allTo.map(value => value.toLowerCase()));
  const allCc = uniqueWithoutOwn(parseMailboxList(email.cc), ownAddress)
    .filter(value => !toKeys.has(value.toLowerCase()));

  return {
    to: replyTargets,
    cc: allCc,
    canReplyAll: allTo.length + allCc.length > 1,
  };
}

export function calculateReplyAllRecipients(email: EmailSummary): Pick<ReplyRecipients, "to" | "cc"> {
  const reply = calculateReplyRecipients(email);
  const ownAddress = email.account_id;
  const allTo = uniqueWithoutOwn(
    [...reply.to, ...parseMailboxList(email.recipient)],
    ownAddress,
  );
  const toKeys = new Set(allTo.map(value => value.toLowerCase()));
  return {
    to: allTo,
    cc: uniqueWithoutOwn(parseMailboxList(email.cc), ownAddress)
      .filter(value => !toKeys.has(value.toLowerCase())),
  };
}

export function isValidEmailAddress(value: string): boolean {
  const address = mailboxAddress(value);
  return /^[^\s@<>]+@[^\s@<>]+\.[^\s@<>]+$/.test(address);
}

export function areValidRecipients(values: string[]): boolean {
  return values.length > 0 && values.every(isValidEmailAddress);
}

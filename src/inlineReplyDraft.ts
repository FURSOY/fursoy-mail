import type { EmailSummary } from "./types";

export interface StoredInlineReplyDraft {
  body: string;
  mode: "reply" | "reply-all";
  draftId: string | null;
  verificationMessageId: string | null;
}

export function inlineReplyStorageKey(email: EmailSummary): string {
  return `fursoy_inline_reply:${email.account_id}:${email.thread_id || email.id}:${email.id}`;
}

export function parseStoredInlineReplyDraft(raw: string | null): StoredInlineReplyDraft | null {
  if (!raw) return null;
  try {
    const value = JSON.parse(raw) as Partial<StoredInlineReplyDraft>;
    if (typeof value.body !== "string") return null;
    return {
      body: value.body,
      mode: value.mode === "reply-all" ? "reply-all" : "reply",
      draftId: typeof value.draftId === "string" ? value.draftId : null,
      verificationMessageId: typeof value.verificationMessageId === "string" ? value.verificationMessageId : null,
    };
  } catch {
    return null;
  }
}

export function extractInlineReplyBody(draftBody: string): string {
  const quoteStart = draftBody.search(/<blockquote\b/i);
  const authored = quoteStart >= 0 ? draftBody.slice(0, quoteStart) : draftBody;
  return authored.replace(/(?:\s*<br\s*\/?>\s*)+$/gi, "").trim();
}

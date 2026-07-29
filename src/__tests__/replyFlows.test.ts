import { describe, expect, it } from "vitest";
import type { EmailSummary } from "../types";
import { buildForwardBody, buildReplyBody } from "../mailCompose";
import { extractInlineReplyBody, inlineReplyStorageKey, parseStoredInlineReplyDraft } from "../inlineReplyDraft";

const message = (id: string, threadId: string, overrides: Partial<EmailSummary> = {}): EmailSummary => ({
  id,
  thread_id: threadId,
  sender: `Sender ${id} <${id}@example.test>`,
  recipient: "me@example.test",
  cc: "",
  reply_to: "",
  message_id: `<${id}@example.test>`,
  references: "<root@example.test>",
  subject: `Subject ${id}`,
  snippet: `Snippet ${id}`,
  date: id === "old" ? 1 : 2,
  unread: false,
  label: "inbox",
  account_id: "me@example.test",
  ...overrides,
});

describe("message-card reply and forward flows", () => {
  it("quotes the selected old message rather than the thread's latest message", () => {
    const old = message("old", "thread-a");
    const reply = buildReplyBody(old, "My reply", "OLD BODY", "{date}: {sender} wrote");
    expect(reply).toContain("OLD BODY");
    expect(reply).toContain("old@example.test");
    expect(reply).not.toContain("latest@example.test");
  });

  it("forwards only the clicked message metadata and exact body", () => {
    const old = message("old", "thread-a");
    const forwarded = buildForwardBody(old, "OLD BODY", {
      forwardedMessage: "Forwarded message",
      sender: "From",
      subject: "Subject",
      date: "Date",
    });
    expect(forwarded).toContain("OLD BODY");
    expect(forwarded).toContain("old@example.test");
    expect(forwarded).not.toContain("latest@example.test");
  });

  it("keeps inline drafts separate by thread and target message and restores their mode", () => {
    const first = message("old", "thread-a");
    const second = message("latest", "thread-a");
    const otherThread = message("old", "thread-b");
    expect(new Set([
      inlineReplyStorageKey(first),
      inlineReplyStorageKey(second),
      inlineReplyStorageKey(otherThread),
    ]).size).toBe(3);

    expect(parseStoredInlineReplyDraft(JSON.stringify({
      body: "Saved body",
      mode: "reply-all",
      draftId: "draft-1",
      verificationMessageId: "verify-1",
    }))).toEqual({
      body: "Saved body",
      mode: "reply-all",
      draftId: "draft-1",
      verificationMessageId: "verify-1",
    });
  });

  it("restores only the authored part of a Gmail reply draft", () => {
    expect(extractInlineReplyBody("My saved answer<br/><br/><blockquote>Quoted mail</blockquote>"))
      .toBe("My saved answer");
  });
});

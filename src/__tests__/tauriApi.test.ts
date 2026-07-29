import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => undefined),
}));

import { invoke } from "@tauri-apps/api/core";
import { tauriApi } from "../tauriApi";

const invokeMock = vi.mocked(invoke);

describe("typed Tauri boundary", () => {
  beforeEach(() => invokeMock.mockClear());

  it("keeps credentials behind the Rust boundary during sync", async () => {
    await tauriApi.syncEmails("account-a", false);
    await tauriApi.syncEmails("account-b", true);

    expect(invokeMock.mock.calls).toEqual([
      ["sync_emails", { accountId: "account-a", force: false }],
      ["sync_emails", { accountId: "account-b", force: true }],
    ]);
  });

  it("scopes toolbar trash actions to both account and conversation", async () => {
    await tauriApi.trashEmail("account-a", "shared-thread-id");

    expect(invokeMock.mock.calls).toEqual([
      ["trash_email", { accountId: "account-a", threadId: "shared-thread-id" }],
    ]);
  });

  it("preserves all-account and cursor paging parameters", async () => {
    await tauriApi.getEmailsByLabel({
      label: "inbox",
      accountId: null,
      limit: 100,
      beforeDate: 123,
      beforeAccountId: "account-a",
      beforeId: "message-a",
    });

    expect(invokeMock).toHaveBeenCalledWith("get_emails_by_label", {
      label: "inbox",
      accountId: null,
      limit: 100,
      beforeDate: 123,
      beforeAccountId: "account-a",
      beforeId: "message-a",
    });
  });

  it("pages grouped conversations with a stable thread cursor", async () => {
    await tauriApi.getThreadGroupsByLabel({
      label: "inbox",
      accountId: null,
      limit: 100,
      beforeDate: 123,
      beforeAccountId: "account-a",
      beforeThreadId: "thread-a",
    });

    expect(invokeMock).toHaveBeenCalledWith("get_thread_groups_by_label", {
      label: "inbox",
      accountId: null,
      limit: 100,
      beforeDate: 123,
      beforeAccountId: "account-a",
      beforeThreadId: "thread-a",
    });
  });

  it("pages local search by conversation instead of using a fixed result cap", async () => {
    await tauriApi.searchLocalThreadGroups({
      query: "atlas",
      accountId: "account-a",
      limit: 100,
      beforeDate: 55,
      beforeAccountId: "account-a",
      beforeThreadId: "thread-b",
    });

    expect(invokeMock).toHaveBeenCalledWith("search_local_thread_groups", {
      query: "atlas",
      accountId: "account-a",
      limit: 100,
      beforeDate: 55,
      beforeAccountId: "account-a",
      beforeThreadId: "thread-b",
    });
  });

  it("verifies an uncertain send by account and generated message ID", async () => {
    await tauriApi.verifySentMessage(
      "account-a",
      "<fursoy-0123456789abcdef@mail.invalid>",
    );

    expect(invokeMock).toHaveBeenCalledWith("verify_sent_message", {
      accountId: "account-a",
      messageId: "<fursoy-0123456789abcdef@mail.invalid>",
    });
  });

  it("reports the selected Gmail conversation as spam", async () => {
    await tauriApi.reportSpam("account-a", "thread-42");

    expect(invokeMock).toHaveBeenCalledWith("report_spam", {
      accountId: "account-a",
      threadId: "thread-42",
    });
  });

  it("passes optional Cc and Bcc recipients through the typed send boundary", async () => {
    await tauriApi.sendEmail({
      accountId: "account-a",
      to: "to@example.test",
      cc: "copy@example.test",
      bcc: "hidden@example.test",
      subject: "Status",
      body: "<p>Hello</p>",
      attachments: null,
    });

    expect(invokeMock).toHaveBeenCalledWith("send_email", {
      accountId: "account-a",
      to: "to@example.test",
      cc: "copy@example.test",
      bcc: "hidden@example.test",
      subject: "Status",
      body: "<p>Hello</p>",
      attachments: null,
    });
  });

  it("sends a reply with its selected message RFC headers and Cc recipients", async () => {
    await tauriApi.sendReply({
      accountId: "me@example.test",
      to: "alice@example.test",
      cc: "team@example.test",
      subject: "Status",
      body: "<p>Reply</p>",
      threadId: "gmail-thread",
      inReplyTo: "<child@example.test>",
      references: "<root@example.test>",
      attachments: null,
    });

    expect(invokeMock).toHaveBeenCalledWith("send_reply", {
      accountId: "me@example.test",
      to: "alice@example.test",
      cc: "team@example.test",
      subject: "Status",
      body: "<p>Reply</p>",
      threadId: "gmail-thread",
      inReplyTo: "<child@example.test>",
      references: "<root@example.test>",
      attachments: null,
    });
  });

  it("binds an autosaved inline reply draft to its Gmail thread", async () => {
    await tauriApi.saveDraft({
      accountId: "me@example.test",
      draftId: null,
      to: "unfinished-address",
      cc: "",
      bcc: "",
      subject: "Re: Status",
      body: "<p>Draft</p>",
      attachments: null,
      threadId: "gmail-thread",
      inReplyTo: "<child@example.test>",
      references: "<root@example.test>",
    });

    expect(invokeMock).toHaveBeenCalledWith("save_draft", expect.objectContaining({
      to: "unfinished-address",
      threadId: "gmail-thread",
      inReplyTo: "<child@example.test>",
      references: "<root@example.test>",
    }));
  });
});

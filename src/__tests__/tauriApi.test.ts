import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => undefined),
}));

import { invoke } from "@tauri-apps/api/core";
import { tauriApi } from "../tauriApi";

const invokeMock = vi.mocked(invoke);

describe("typed Tauri boundary", () => {
  beforeEach(() => invokeMock.mockClear());

  it("discovers a provider before starting a scoped browser sign-in", async () => {
    await tauriApi.discoverMailProvider("person@gmail.com");
    await tauriApi.startMailOAuth("person@gmail.com", "google");
    await tauriApi.cancelMailOAuth();

    expect(invokeMock.mock.calls).toEqual([
      ["discover_mail_provider", { email: "person@gmail.com" }],
      ["start_mail_oauth", { email: "person@gmail.com", provider: "google" }],
      ["cancel_mail_oauth"],
    ]);
  });

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

  it("drives the IMAP watcher by account and mailbox without exposing credentials", async () => {
    await tauriApi.startImapWatch("account-a");
    await tauriApi.stopImapWatch("account-a");
    await tauriApi.startImapWatch("account-a", "custom:Work");
    await tauriApi.stopImapWatch("account-a", "custom:Work");

    expect(invokeMock.mock.calls).toEqual([
      ["start_imap_watch", { accountId: "account-a", mailboxRole: "inbox" }],
      ["stop_imap_watch", { accountId: "account-a", mailboxRole: "inbox" }],
      ["start_imap_watch", { accountId: "account-a", mailboxRole: "custom:Work" }],
      ["stop_imap_watch", { accountId: "account-a", mailboxRole: "custom:Work" }],
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
    const filters = {
      from: "alice@example.test", to: "", subject: "", includes: "", excludes: "",
      afterDate: null, beforeDate: null, location: "all",
      dateMode: "range" as const, dateAnchor: null, dateWindow: "1d" as const,
      locationExplicit: false,
      hasAttachment: true, unread: false, starred: false,
    };
    await tauriApi.searchLocalThreadGroups({
      query: "atlas",
      filters,
      accountId: "account-a",
      limit: 100,
      beforeDate: 55,
      beforeAccountId: "account-a",
      beforeThreadId: "thread-b",
    });

    expect(invokeMock).toHaveBeenCalledWith("search_local_thread_groups", {
      query: "atlas",
      filters,
      accountId: "account-a",
      limit: 100,
      beforeDate: 55,
      beforeAccountId: "account-a",
      beforeThreadId: "thread-b",
    });
  });

  it("reports the selected conversation as spam", async () => {
    await tauriApi.reportSpam("account-a", "thread-42");

    expect(invokeMock).toHaveBeenCalledWith("report_spam", {
      accountId: "account-a",
      threadId: "thread-42",
    });
  });

  it("marks the selected Gmail conversation as read", async () => {
    await tauriApi.markThreadAsRead("account-a", "thread-42");

    expect(invokeMock).toHaveBeenCalledWith("mark_thread_as_read", {
      accountId: "account-a",
      threadId: "thread-42",
    });
  });

  it("scopes Gmail label creation and conversation updates to one account", async () => {
    await tauriApi.createGmailLabel("account-a", "Projects/Atlas");
    await tauriApi.setThreadGmailLabel("account-a", "thread-42", "Label_9", true);

    expect(invokeMock.mock.calls).toEqual([
      ["create_gmail_label", { accountId: "account-a", name: "Projects/Atlas" }],
      ["set_thread_gmail_label", {
        accountId: "account-a",
        threadId: "thread-42",
        labelId: "Label_9",
        applied: true,
      }],
    ]);
  });

  it("stars a Gmail conversation in the selected account", async () => {
    await tauriApi.setThreadStarred("account-a", "thread-42", true);

    expect(invokeMock).toHaveBeenCalledWith("set_thread_gmail_label", {
      accountId: "account-a",
      threadId: "thread-42",
      labelId: "STARRED",
      applied: true,
    });
  });

  it("scopes Gmail label management actions to the owning account", async () => {
    await tauriApi.renameGmailLabel("account-a", "Label_9", "Projects");
    await tauriApi.setGmailLabelColor("account-a", "Label_9", "#4a86e8", "#ffffff");
    await tauriApi.deleteGmailLabel("account-a", "Label_9");

    expect(invokeMock.mock.calls).toEqual([
      ["rename_gmail_label", { accountId: "account-a", labelId: "Label_9", name: "Projects" }],
      ["set_gmail_label_color", {
        accountId: "account-a",
        labelId: "Label_9",
        backgroundColor: "#4a86e8",
        textColor: "#ffffff",
      }],
      ["delete_gmail_label", { accountId: "account-a", labelId: "Label_9" }],
    ]);
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

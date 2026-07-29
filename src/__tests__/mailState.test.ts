import { describe, expect, it, vi } from "vitest";
import { enqueueMailMutation, inboxUnreadDelta, runAuthenticatedMailAction } from "../mailActionState";
import { emailCacheKey, MAX_KNOWN_EMAIL_IDS, updateNotificationBaseline } from "../mailSyncState";
import type { EmailSummary } from "../types";

function mail(id: string, accountId = "account-a", overrides: Partial<EmailSummary> = {}): EmailSummary {
  return {
    id,
    thread_id: `thread-${id}`,
    sender: "Sender <sender@example.test>",
    recipient: accountId,
    cc: "",
    reply_to: "",
    message_id: `<${id}@example.test>`,
    references: "",
    subject: `Subject ${id}`,
    snippet: "",
    date: 1,
    unread: true,
    label: "inbox",
    account_id: accountId,
    ...overrides,
  };
}

describe("mail mutation queue", () => {
  it("serializes mutations for the same message", async () => {
    const queue = new Map<string, Promise<void>>();
    const order: string[] = [];
    let releaseFirst!: () => void;
    let markFirstStarted!: () => void;
    const firstBlocked = new Promise<void>(resolve => { releaseFirst = resolve; });
    const firstStarted = new Promise<void>(resolve => { markFirstStarted = resolve; });

    const first = enqueueMailMutation(queue, "account-a\0mail-a", async () => {
      order.push("first-start");
      markFirstStarted();
      await firstBlocked;
      order.push("first-end");
    });
    const second = enqueueMailMutation(queue, "account-a\0mail-a", async () => {
      order.push("second");
    });

    await firstStarted;
    expect(order).toEqual(["first-start"]);
    releaseFirst();
    await Promise.all([first, second]);
    expect(order).toEqual(["first-start", "first-end", "second"]);
    expect(queue.size).toBe(0);
  });
});

describe("authenticated mail actions", () => {
  function dependencies() {
    return {
      refreshAccessToken: vi.fn(async () => ({ authenticated: true })),
      upsertToken: vi.fn(),
      clearExpiredAccount: vi.fn(),
      markAccountExpired: vi.fn(),
    };
  }

  it("runs once with the current token when authentication is valid", async () => {
    const deps = dependencies();
    const action = vi.fn(async () => undefined);

    await runAuthenticatedMailAction({
      accountId: "account-a", currentToken: "current-token", reloginRequiredMessage: "Relogin",
      action, ...deps,
    });

    expect(action).toHaveBeenCalledTimes(1);
    expect(action).toHaveBeenCalledWith("current-token");
    expect(deps.refreshAccessToken).not.toHaveBeenCalled();
  });

  it("fails before calling Gmail when no access token is available", async () => {
    const deps = dependencies();
    const action = vi.fn(async () => undefined);

    await expect(runAuthenticatedMailAction({
      accountId: "account-a", currentToken: "", reloginRequiredMessage: "Relogin required",
      action, ...deps,
    })).rejects.toThrow("Relogin required");

    expect(action).not.toHaveBeenCalled();
    expect(deps.refreshAccessToken).not.toHaveBeenCalled();
  });

  it("refreshes an expired token and retries exactly once", async () => {
    const deps = dependencies();
    const action = vi.fn()
      .mockRejectedValueOnce(new Error("401 Unauthorized"))
      .mockResolvedValueOnce(undefined);

    await runAuthenticatedMailAction({
      accountId: "account-a", currentToken: "expired-token", reloginRequiredMessage: "Relogin",
      action, ...deps,
    });

    expect(action.mock.calls).toEqual([["expired-token"], ["active"]]);
    expect(deps.refreshAccessToken).toHaveBeenCalledTimes(1);
    expect(deps.upsertToken).toHaveBeenCalledWith("account-a", "active");
    expect(deps.clearExpiredAccount).toHaveBeenCalledWith("account-a");
    expect(deps.markAccountExpired).not.toHaveBeenCalled();
  });

  it("does not retry permission and ordinary Gmail failures", async () => {
    const deps = dependencies();
    const action = vi.fn(async () => {
      throw new Error("403 Request had insufficient authentication scopes");
    });

    await expect(runAuthenticatedMailAction({
      accountId: "account-a", currentToken: "current-token", reloginRequiredMessage: "Relogin",
      action, ...deps,
    })).rejects.toThrow("insufficient authentication scopes");

    expect(action).toHaveBeenCalledTimes(1);
    expect(deps.refreshAccessToken).not.toHaveBeenCalled();
    expect(deps.markAccountExpired).not.toHaveBeenCalled();
  });

  it("marks only the affected account expired when refresh fails", async () => {
    const deps = dependencies();
    deps.refreshAccessToken.mockRejectedValueOnce(new Error("refresh failed"));

    await expect(runAuthenticatedMailAction({
      accountId: "account-b", currentToken: "expired-token", reloginRequiredMessage: "Relogin",
      action: vi.fn(async () => { throw new Error("401 Unauthorized"); }),
      ...deps,
    })).rejects.toThrow("refresh failed");

    expect(deps.markAccountExpired).toHaveBeenCalledWith("account-b");
  });
});

describe("inbox unread deltas", () => {
  it("decrements when unread mail leaves inbox and restores on return", () => {
    const inboxMail = mail("inbox-mail");
    const trashMail = mail("trash-mail", "account-a", { label: "trash" });
    expect(inboxUnreadDelta(inboxMail, "trash")).toBe(-1);
    expect(inboxUnreadDelta(inboxMail, "archive")).toBe(-1);
    expect(inboxUnreadDelta(inboxMail, "spam")).toBe(-1);
    expect(inboxUnreadDelta(trashMail, "inbox")).toBe(1);
  });

  it("does not change the badge for read mail or moves outside inbox", () => {
    expect(inboxUnreadDelta(mail("read", "account-a", { unread: false }), "trash")).toBe(0);
    expect(inboxUnreadDelta(mail("spam", "account-a", { label: "spam" }), "trash")).toBe(0);
  });
});

describe("multi-account notification baseline", () => {
  it("bounds the remembered email IDs to the newest entries", () => {
    const known = new Set(Array.from({ length: MAX_KNOWN_EMAIL_IDS }, (_, index) => `old-${index}`));
    updateNotificationBaseline({
      freshInbox: [mail("new")], knownEmailIds: known, readyAccountIds: new Set(),
      successfullySyncedAccountIds: new Set(), suppressNotifications: true,
    });
    expect(known.size).toBe(MAX_KNOWN_EMAIL_IDS);
    expect(known.has("old-0")).toBe(false);
    expect(known.has(emailCacheKey(mail("new")))).toBe(true);
  });

  it("builds each account baseline without notifying old unread mail", () => {
    const known = new Set<string>();
    const ready = new Set<string>();
    const initialA = mail("initial-a", "account-a");

    const notifications = updateNotificationBaseline({
      freshInbox: [initialA], knownEmailIds: known, readyAccountIds: ready,
      successfullySyncedAccountIds: new Set(["account-a"]), suppressNotifications: false,
    });

    expect(notifications).toEqual([]);
    expect(known.has(emailCacheKey(initialA))).toBe(true);
    expect(ready).toEqual(new Set(["account-a"]));
  });

  it("notifies ready accounts while independently baselining a new account", () => {
    const knownA = mail("known-a", "account-a");
    const freshA = mail("fresh-a", "account-a");
    const initialB = mail("initial-b", "account-b");
    const known = new Set([emailCacheKey(knownA)]);
    const ready = new Set(["account-a"]);

    const notifications = updateNotificationBaseline({
      freshInbox: [knownA, freshA, initialB], knownEmailIds: known, readyAccountIds: ready,
      successfullySyncedAccountIds: new Set(["account-a", "account-b"]), suppressNotifications: false,
    });

    expect(notifications.map(item => item.id)).toEqual(["fresh-a"]);
    expect(ready).toEqual(new Set(["account-a", "account-b"]));
    expect(known.has(emailCacheKey(initialB))).toBe(true);
  });

  it("keeps historical IDs so old unread mail cannot notify again", () => {
    const oldMail = mail("old-mail");
    const newerMail = mail("new-mail");
    const known = new Set<string>();
    const ready = new Set(["account-a"]);
    const synced = new Set(["account-a"]);

    updateNotificationBaseline({
      freshInbox: [oldMail], knownEmailIds: known, readyAccountIds: ready,
      successfullySyncedAccountIds: synced, suppressNotifications: false,
    });
    updateNotificationBaseline({
      freshInbox: [newerMail], knownEmailIds: known, readyAccountIds: ready,
      successfullySyncedAccountIds: synced, suppressNotifications: false,
    });
    const repeated = updateNotificationBaseline({
      freshInbox: [newerMail, oldMail], knownEmailIds: known, readyAccountIds: ready,
      successfullySyncedAccountIds: synced, suppressNotifications: false,
    });

    expect(repeated).toEqual([]);
  });

  it("records suppressed messages so they do not notify on the next sync", () => {
    const suppressedMail = mail("suppressed-mail");
    const known = new Set<string>();
    const ready = new Set(["account-a"]);
    const synced = new Set(["account-a"]);

    const suppressed = updateNotificationBaseline({
      freshInbox: [suppressedMail], knownEmailIds: known, readyAccountIds: ready,
      successfullySyncedAccountIds: synced, suppressNotifications: true,
    });
    const nextSync = updateNotificationBaseline({
      freshInbox: [suppressedMail], knownEmailIds: known, readyAccountIds: ready,
      successfullySyncedAccountIds: synced, suppressNotifications: false,
    });

    expect(suppressed).toEqual([]);
    expect(nextSync).toEqual([]);
  });
});

import { useCallback, useEffect, useRef, useState, useTransition, type MutableRefObject } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { AppLocale, AppLanguage } from "../i18n";
import { updateNotificationBaseline } from "../mailSyncState";
import { syncIntervalDelayMs } from "../syncInterval";
import type { Account, AppControls, EmailSummary, OtpMode } from "../types";
import { tauriApi, type ImapChangeEvent } from "../tauriApi";
import { extractVerificationCode, isAuthFailure, isInQuietHours, isMailListTab, isSessionRevoked, MAIL_PAGE_SIZE } from "../utils";

interface SyncOptions {
  userInitiated?: boolean;
  suppressNotifications?: boolean;
  accountId?: string;
  accountIds?: string[];
}

interface UseMailSyncOptions {
  accounts: Account[];
  accountTokens: Record<string, string>;
  accountsRef: MutableRefObject<Account[]>;
  accountTokensRef: MutableRefObject<Record<string, string>>;
  activeAccountId: string | null;
  activeTab: string;
  activeAccountIdRef: MutableRefObject<string | null>;
  expiredAccountsRef: MutableRefObject<Set<string>>;
  tokenExpiredRef: MutableRefObject<boolean>;
  appControlsRef: MutableRefObject<AppControls>;
  activeTabRef: MutableRefObject<string>;
  syncIntervalRef: MutableRefObject<number | null>;
  syncChainIdRef: MutableRefObject<number>;
  backgroundSyncRef: MutableRefObject<(options?: SyncOptions) => Promise<boolean>>;
  recentNotificationsRef: MutableRefObject<Record<string, { accountId: string; messageId: string } | null>>;
  knownEmailIdsRef: MutableRefObject<Set<string>>;
  notificationReadyAccountIdsRef: MutableRefObject<Set<string>>;
  notificationBaselineEpochRef: MutableRefObject<number>;
  pendingUnreadBadgeDeltasRef: MutableRefObject<Map<string, { delta: number; expiresAt: number }>>;
  emailsLength: number;
  syncIntervalSeconds: number;
  notificationDuration: number;
  notificationInfinite: boolean;
  otpMode: OtpMode;
  appLanguage: AppLanguage;
  locale: AppLocale;
  loadEmails: () => Promise<EmailSummary[]>;
  shouldDeferNetwork: (userInitiated?: boolean) => Promise<boolean>;
  refreshAccessToken: (accountId: string) => Promise<{ authenticated: boolean }>;
  upsertToken: (accountId: string, accessToken: string) => void;
  clearExpiredAccount: (accountId: string) => void;
  setSessionExpired: (expired: boolean) => void;
  markAccountExpired: (accountId: string, showMessage?: boolean) => void;
  markSessionExpired: (showMessage?: boolean) => void;
  showToast: (message: string, type?: "error" | "success" | "info") => void;
}

/// Which mailbox role a tab corresponds to for live-push purposes, or `null`
/// when the tab has no single IMAP mailbox behind it (a cross-mailbox view
/// like starred/all, or a Gmail label, which is not an IMAP mailbox at all).
function watchableFolderRole(tab: string): string | null {
  switch (tab) {
    case "sent": return "sent";
    case "archive": return "archive";
    case "spam": return "junk";
    case "trash": return "trash";
    default:
      return tab.startsWith("custom:") ? tab : null;
  }
}

interface WatchTarget {
  accountId: string;
  role: string;
}

function watchTargetKey(target: WatchTarget): string {
  return JSON.stringify(target);
}

export function useMailSync(options: UseMailSyncOptions) {
  const {
    accounts, accountTokens, accountsRef, accountTokensRef, activeAccountId, activeTab,
    activeAccountIdRef, expiredAccountsRef, tokenExpiredRef, appControlsRef, activeTabRef,
    syncIntervalRef, syncChainIdRef, backgroundSyncRef, recentNotificationsRef,
    knownEmailIdsRef, notificationReadyAccountIdsRef, notificationBaselineEpochRef,
    pendingUnreadBadgeDeltasRef, emailsLength, syncIntervalSeconds,
    notificationDuration, notificationInfinite, otpMode, appLanguage, locale,
    loadEmails, shouldDeferNetwork, refreshAccessToken, upsertToken,
    clearExpiredAccount, setSessionExpired, markAccountExpired,
    markSessionExpired, showToast,
  } = options;

  const [isUserSyncing, setIsUserSyncing] = useState(false);
  const [isBackgroundSyncing, setIsBackgroundSyncing] = useState(false);
  const [inboxUnread, setInboxUnread] = useState(0);
  const [, startDataTransition] = useTransition();
  const syncIntervalSecondsRef = useRef(syncIntervalSeconds);
  const notificationDurationRef = useRef(notificationDuration);
  const notificationInfiniteRef = useRef(notificationInfinite);
  const backgroundSyncFlightRef = useRef<Promise<boolean> | null>(null);
  const pendingSyncAccountIdsRef = useRef<Set<string>>(new Set());
  const pendingFullSyncRef = useRef(false);
  // Accounts become watchable only once their credential is loaded, which
  // happens after the account list is published. Keying the watcher effect on
  // the account list alone would start it while no account had a token yet and
  // never re-run, so derive the set from token state and restart only when the
  // membership itself changes, not on every token value update. Every
  // token-bearing account always wants its inbox watched; the account and tab
  // currently open in the UI additionally wants its own folder watched, when
  // that folder maps to a single IMAP mailbox at all (see watchableFolderRole).
  const watchableTargets = new Map<string, WatchTarget>();
  for (const account of accounts) {
    if (!accountTokens[account.id]) continue;
    const inboxTarget: WatchTarget = { accountId: account.id, role: "inbox" };
    watchableTargets.set(watchTargetKey(inboxTarget), inboxTarget);
    if (account.id === activeAccountId) {
      const role = watchableFolderRole(activeTab);
      if (role) {
        const target: WatchTarget = { accountId: account.id, role };
        watchableTargets.set(watchTargetKey(target), target);
      }
    }
  }
  const watchableKey = [...watchableTargets.keys()].sort().join("|");
  const watchableTargetsRef = useRef<Map<string, WatchTarget>>(watchableTargets);
  watchableTargetsRef.current = watchableTargets;
  const watchedTargetsRef = useRef<Map<string, WatchTarget>>(new Map());
  syncIntervalSecondsRef.current = syncIntervalSeconds;
  notificationDurationRef.current = notificationDuration;
  notificationInfiniteRef.current = notificationInfinite;

  const syncAccountWithAutoRefresh = useCallback(async (
    accountId: string,
    token: string,
    force = false,
  ): Promise<string> => {
    try {
      await tauriApi.syncEmails(accountId, force);
      return token;
    } catch (error) {
      if (!isAuthFailure(error)) throw error;
      let refreshed: { authenticated: boolean };
      try {
        refreshed = await refreshAccessToken(accountId);
      } catch (refreshError) {
        // Only a credential the provider rejected means the account has to be
        // signed in to again. A refresh that could not be reached — no network
        // yet after a resume, a throttled or failing endpoint — leaves the
        // stored session intact for the next round.
        if (!isSessionRevoked(refreshError)) throw new Error(locale.messages.refreshFailed);
        console.error("The stored session was rejected.");
        markAccountExpired(accountId);
        throw new Error(locale.messages.reloginRequired);
      }
      if (!refreshed.authenticated) {
        markAccountExpired(accountId);
        throw new Error(locale.messages.reloginRequired);
      }
      upsertToken(accountId, "active");
      clearExpiredAccount(accountId);
      setSessionExpired(false);
      // The renewed session is what the retry proves; a mailbox that fails now
      // is a sync problem, not an expired account.
      await tauriApi.syncEmails(accountId, force);
      return "active";
    }
  }, [clearExpiredAccount, locale, markAccountExpired, refreshAccessToken, setSessionExpired, upsertToken]);

  const adjustUnreadBadge = useCallback((accountId: string, delta: number) => {
    const activeAccountId = activeAccountIdRef.current;
    if (activeAccountId !== null && activeAccountId !== accountId) return;
    const now = Date.now();
    const previous = pendingUnreadBadgeDeltasRef.current.get(accountId);
    const nextDelta = (previous?.expiresAt && previous.expiresAt > now ? previous.delta : 0) + delta;
    if (nextDelta === 0) pendingUnreadBadgeDeltasRef.current.delete(accountId);
    else pendingUnreadBadgeDeltasRef.current.set(accountId, { delta: nextDelta, expiresAt: now + 30_000 });
    setInboxUnread(current => Math.max(0, current + delta));
  }, [activeAccountIdRef, pendingUnreadBadgeDeltasRef]);

  const refreshUnreadCount = useCallback(async () => {
    try {
      const accountId = activeAccountIdRef.current;
      const count = await tauriApi.getInboxUnreadCount(accountId);
      const now = Date.now();
      let pendingDelta = 0;
      for (const [id, pending] of pendingUnreadBadgeDeltasRef.current) {
        if (pending.expiresAt <= now) pendingUnreadBadgeDeltasRef.current.delete(id);
        else if (accountId === null || accountId === id) pendingDelta += pending.delta;
      }
      startDataTransition(() => setInboxUnread(Math.max(0, count + pendingDelta)));
      return count;
    } catch {
      return 0;
    }
  }, [activeAccountIdRef, pendingUnreadBadgeDeltasRef]);

  const notifyNewEmails = useCallback(async (newEmails: EmailSummary[]) => {
    if (newEmails.length === 0) return;
    const controls = appControlsRef.current;
    if (controls.notificationMode === "off" || isInQuietHours(controls)) return;
    try {
      for (const email of newEmails.slice(0, 5)) {
        const senderName = email.sender.split("<")[0].replace(/"/g, "").trim() || email.sender;
        const body = otpMode === "off" ? "" : await tauriApi.getEmailBody(email.id, email.account_id).catch(() => "");
        const code = extractVerificationCode({ ...email, body_html: body }, otpMode, appLanguage);
        if (controls.notificationMode === "otpOnly" && !code) continue;
        const account = accountsRef.current.find(item => item.id === email.account_id);
        const title = senderName.slice(0, 64);
        const notificationBody = (email.subject || email.snippet || "").trim().slice(0, 100) || locale.messages.newMessage;
        const notificationKey = title + notificationBody;
        const previous = recentNotificationsRef.current[notificationKey];
        recentNotificationsRef.current[notificationKey] = previous &&
          (previous.accountId !== email.account_id || previous.messageId !== email.id)
          ? null
          : { accountId: email.account_id, messageId: email.id };
        const notificationKeys = Object.keys(recentNotificationsRef.current);
        while (notificationKeys.length > 100) {
          const oldest = notificationKeys.shift();
          if (oldest !== undefined) delete recentNotificationsRef.current[oldest];
        }
        await tauriApi.showCustomNotification({
          title,
          body: notificationBody,
          kind: "mail",
          code: code || null,
          emailId: email.id,
          duration: notificationInfiniteRef.current ? 0 : notificationDurationRef.current * 1000,
          accountId: email.account_id || null,
          accountPicture: account?.picture || null,
          multiAccount: accountsRef.current.length > 1,
          copyLabel: locale.common.copy,
          copiedLabel: locale.common.copied,
          copyFailedLabel: locale.common.copyFailedRetry,
          dismissAllLabel: locale.common.dismissAll,
        });
      }
    } catch (error) {
      console.error("Notification error:", error);
    }
  }, [accountsRef, appControlsRef, appLanguage, locale, otpMode, recentNotificationsRef]);

  const clearPeriodicSync = useCallback(() => {
    syncChainIdRef.current += 1;
    if (syncIntervalRef.current !== null) {
      clearTimeout(syncIntervalRef.current);
      syncIntervalRef.current = null;
    }
  }, [syncChainIdRef, syncIntervalRef]);

  // The backend watcher delivers inbox changes promptly, but it only covers the
  // inbox of servers that support IDLE. This timer is what keeps every other
  // mailbox current, and the floor under an account whose watcher cannot run.
  const startPeriodicSync = useCallback(() => {
    clearPeriodicSync();
    const chainId = syncChainIdRef.current;
    const scheduleNext = () => {
      if (syncChainIdRef.current !== chainId) return;
      syncIntervalRef.current = window.setTimeout(async () => {
        if (syncChainIdRef.current !== chainId) return;
        if (Object.keys(accountTokensRef.current).length > 0 && !tokenExpiredRef.current) {
          await backgroundSyncRef.current();
        }
        scheduleNext();
      }, syncIntervalDelayMs(syncIntervalSecondsRef.current));
    };
    scheduleNext();
  }, [accountTokensRef, backgroundSyncRef, clearPeriodicSync, syncChainIdRef, syncIntervalRef, tokenExpiredRef]);

  useEffect(() => {
    if (Object.keys(accountTokensRef.current).length > 0) startPeriodicSync();
  }, [syncIntervalSeconds]);

  // Everything that has to happen once an account's cache has moved, whether
  // this app fetched the change or the backend watcher did. Reads only local
  // state, so a watcher event costs no network.
  const publishCacheChanges = useCallback(async (
    changedAccountIds: Set<string>,
    suppressNotifications: boolean,
  ) => {
    const readyAccountIds = notificationReadyAccountIdsRef.current;
    const establishesBaseline = [...changedAccountIds]
      .some(accountId => !readyAccountIds.has(accountId));
    // The first successful sync builds a broad local baseline. Later ones read
    // only the normal first page and merge it into the known-id set.
    const freshInbox = await tauriApi.getEmailsByLabel({
      label: "inbox",
      accountId: null,
      limit: establishesBaseline ? 5_000 : undefined,
    });
    const newUnreadEmails = updateNotificationBaseline({
      freshInbox,
      knownEmailIds: knownEmailIdsRef.current,
      readyAccountIds,
      successfullySyncedAccountIds: changedAccountIds,
      suppressNotifications,
    });
    await notifyNewEmails(newUnreadEmails);
    // A custom folder and a label tab each have their own list, and the
    // watcher that just wrote to the cache is often watching exactly one of
    // them. Refreshing only the fixed tabs is what left those lists stale
    // until the user switched away and back.
    if (isMailListTab(activeTabRef.current) && emailsLength <= MAIL_PAGE_SIZE) await loadEmails();
    await refreshUnreadCount();
  }, [
    activeTabRef, emailsLength, knownEmailIdsRef, loadEmails,
    notificationReadyAccountIdsRef, notifyNewEmails, refreshUnreadCount,
  ]);

  const runBackgroundSync = useCallback(async (syncOptions?: SyncOptions): Promise<boolean> => {
    const requestedAccountIds = syncOptions?.accountId
      ? new Set([syncOptions.accountId])
      : syncOptions?.accountIds
        ? new Set(syncOptions.accountIds)
        : null;
    const currentAccounts = requestedAccountIds
      ? accountsRef.current.filter(account => requestedAccountIds.has(account.id))
      : accountsRef.current;
    const tokens = accountTokensRef.current;
    if (currentAccounts.length === 0) return false;
    const userInitiated = syncOptions?.userInitiated ?? false;
    const baselineEpoch = notificationBaselineEpochRef.current;
    if (appControlsRef.current.mailSyncPaused && !userInitiated) return false;
    if (appControlsRef.current.notificationMode === "off" && !userInitiated) {
      const isVisible = await getCurrentWindow().isVisible().catch(() => true);
      if (!isVisible) return false;
    }
    if (await shouldDeferNetwork(userInitiated)) {
      console.log("System in fullscreen/game mode, skipping background sync.");
      return false;
    }

    try {
      if (userInitiated) setIsUserSyncing(true);
      else setIsBackgroundSyncing(true);
      let anySuccess = false;
      const successfullySyncedAccountIds = new Set<string>();
      for (const account of currentAccounts) {
        const token = tokens[account.id];
        if (!token || expiredAccountsRef.current.has(account.id)) continue;
        try {
          // A sync the user asked for must not answer from a checkpoint that
          // cannot see a message read or starred on another device.
          await syncAccountWithAutoRefresh(account.id, token, userInitiated);
          anySuccess = true;
          successfullySyncedAccountIds.add(account.id);
        } catch (error) {
          if (!isAuthFailure(error)) console.error("Account sync failed.");
        }
      }

      if (anySuccess) {
        const suppressNotifications = syncOptions?.suppressNotifications === true ||
          baselineEpoch !== notificationBaselineEpochRef.current;
        await publishCacheChanges(successfullySyncedAccountIds, suppressNotifications);
      }
      return anySuccess;
    } catch (error) {
      console.error("Background sync failed:", error);
      if (isAuthFailure(error)) {
        markSessionExpired();
        return false;
      }
      showToast(`${locale.messages.syncFailedDetail}: ${error instanceof Error ? error.message : String(error)}`, "error");
      return false;
    } finally {
      if (userInitiated) setIsUserSyncing(false);
      else setIsBackgroundSyncing(false);
    }
  }, [
    accountsRef, accountTokensRef, appControlsRef, expiredAccountsRef, locale,
    markSessionExpired, notificationBaselineEpochRef, publishCacheChanges,
    shouldDeferNetwork, showToast, syncAccountWithAutoRefresh,
  ]);

  const backgroundSync = useCallback((syncOptions?: SyncOptions): Promise<boolean> => {
    const existing = backgroundSyncFlightRef.current;
    if (existing) {
      const requestedIds = syncOptions?.accountId
        ? [syncOptions.accountId]
        : syncOptions?.accountIds;
      if (requestedIds) {
        for (const accountId of requestedIds) pendingSyncAccountIdsRef.current.add(accountId);
      } else {
        pendingFullSyncRef.current = true;
      }
      return existing;
    }

    const flight = runBackgroundSync(syncOptions);
    backgroundSyncFlightRef.current = flight;
    const clearFlight = () => {
      if (backgroundSyncFlightRef.current === flight) {
        backgroundSyncFlightRef.current = null;
        const runAll = pendingFullSyncRef.current;
        const accountIds = [...pendingSyncAccountIdsRef.current];
        pendingFullSyncRef.current = false;
        pendingSyncAccountIdsRef.current.clear();
        if (runAll || accountIds.length > 0) {
          void backgroundSync(runAll ? undefined : { accountIds });
        }
      }
    };
    void flight.then(clearFlight, clearFlight);
    return flight;
  }, [runBackgroundSync]);

  backgroundSyncRef.current = backgroundSync;

  // The watchers themselves live in Rust: one connection per watched (account,
  // mailbox) pair, parked on IDLE. This effect only owns which pairs have one,
  // and starts or stops the ones that changed — the inbox for every
  // token-bearing account, plus whatever folder is currently open in the UI.
  useEffect(() => {
    const watched = watchedTargetsRef.current;
    const wanted = watchableTargetsRef.current;
    for (const [key, target] of [...watched]) {
      if (wanted.has(key)) continue;
      watched.delete(key);
      void tauriApi.stopImapWatch(target.accountId, target.role).catch(() => {});
    }
    for (const [key, target] of wanted) {
      if (watched.has(key)) continue;
      watched.set(key, target);
      void tauriApi.startImapWatch(target.accountId, target.role).catch(() => {
        // A watcher that could not start leaves the mailbox on the periodic
        // sync, so let the next change try again.
        watched.delete(key);
      });
    }
  }, [watchableKey]);

  // The watcher has already written the change to the cache, so this only has
  // to publish it: no sync, no second connection to the server.
  useEffect(() => {
    const unlistenPromise = listen<ImapChangeEvent>("imap-mailbox-changed", event => {
      const accountId = event.payload.accountId;
      if (!accountId) return;
      void publishCacheChanges(new Set([accountId]), false);
    });
    return () => { void unlistenPromise.then(unlisten => unlisten()); };
  }, [publishCacheChanges]);

  // The watcher is the only thing still talking to the server while the window
  // sits idle, so a session it finds revoked has to reach the UI from here.
  // Otherwise the account simply stops receiving mail with nothing said.
  useEffect(() => {
    const unlistenPromise = listen<ImapChangeEvent>("mail-session-expired", event => {
      const accountId = event.payload.accountId;
      if (!accountId) return;
      markAccountExpired(accountId);
    });
    return () => { void unlistenPromise.then(unlisten => unlisten()); };
  }, [markAccountExpired]);

  useEffect(() => {
    const watched = watchedTargetsRef.current;
    return () => {
      for (const target of watched.values()) {
        void tauriApi.stopImapWatch(target.accountId, target.role).catch(() => {});
      }
      watched.clear();
    };
  }, []);

  return {
    isUserSyncing,
    isBackgroundSyncing,
    inboxUnread,
    setIsUserSyncing,
    setIsBackgroundSyncing,
    adjustUnreadBadge,
    refreshUnreadCount,
    clearPeriodicSync,
    startPeriodicSync,
    backgroundSync,
  };
}

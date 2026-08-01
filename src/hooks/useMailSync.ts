import { useCallback, useEffect, useRef, useState, useTransition, type MutableRefObject } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { AppLocale, AppLanguage } from "../i18n";
import { updateNotificationBaseline } from "../mailSyncState";
import { syncIntervalDelayMs } from "../syncInterval";
import type { Account, AppControls, EmailSummary, OtpMode } from "../types";
import { tauriApi } from "../tauriApi";
import { extractVerificationCode, isAuthFailure, isInQuietHours, MAIL_PAGE_SIZE, MAIL_TABS } from "../utils";

interface SyncOptions {
  userInitiated?: boolean;
  suppressNotifications?: boolean;
  eventDriven?: boolean;
  accountId?: string;
  accountIds?: string[];
}

interface UseMailSyncOptions {
  accounts: Account[];
  accountsRef: MutableRefObject<Account[]>;
  accountTokensRef: MutableRefObject<Record<string, string>>;
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

export function useMailSync(options: UseMailSyncOptions) {
  const {
    accounts, accountsRef, accountTokensRef, activeAccountIdRef,
    expiredAccountsRef, tokenExpiredRef, appControlsRef, activeTabRef,
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
  const pendingEventDrivenSyncRef = useRef(false);
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
      try {
        const refreshed = await refreshAccessToken(accountId);
        if (!refreshed.authenticated) throw new Error(locale.messages.reloginRequired);
        upsertToken(accountId, "active");
        clearExpiredAccount(accountId);
        setSessionExpired(false);
        await tauriApi.syncEmails(accountId, force);
        return "active";
      } catch {
        console.error("Token refresh failed.");
        markAccountExpired(accountId);
        throw new Error(locale.messages.reloginRequired);
      }
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

  const startPeriodicSync = useCallback(() => {
    clearPeriodicSync();
    const legacyAccountIds = accountsRef.current
      .filter(account => account.provider === "google")
      .map(account => account.id);
    if (legacyAccountIds.length === 0) return;
    const chainId = syncChainIdRef.current;
    const scheduleNext = () => {
      if (syncChainIdRef.current !== chainId) return;
      syncIntervalRef.current = window.setTimeout(async () => {
        if (syncChainIdRef.current !== chainId) return;
        if (Object.keys(accountTokensRef.current).length > 0 && !tokenExpiredRef.current) {
          await backgroundSyncRef.current({ accountIds: legacyAccountIds });
        }
        scheduleNext();
      }, syncIntervalDelayMs(syncIntervalSecondsRef.current));
    };
    scheduleNext();
  }, [accountTokensRef, backgroundSyncRef, clearPeriodicSync, syncChainIdRef, syncIntervalRef, tokenExpiredRef]);

  useEffect(() => {
    if (Object.keys(accountTokensRef.current).length > 0) startPeriodicSync();
  }, [syncIntervalSeconds]);

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
    if (appControlsRef.current.notificationMode === "off" && !userInitiated && !syncOptions?.eventDriven) {
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
          await syncAccountWithAutoRefresh(account.id, token, userInitiated);
          anySuccess = true;
          successfullySyncedAccountIds.add(account.id);
        } catch (error) {
          if (!isAuthFailure(error)) console.error("Account sync failed.");
        }
      }

      if (anySuccess) {
        const readyAccountIds = notificationReadyAccountIdsRef.current;
        const establishesBaseline = [...successfullySyncedAccountIds]
          .some(accountId => !readyAccountIds.has(accountId));
        // The first successful sync builds a broad local baseline. Later syncs
        // read only the normal first page and merge it into the known-id set.
        const freshInbox = await tauriApi.getEmailsByLabel({
          label: "inbox",
          accountId: null,
          limit: establishesBaseline ? 5_000 : undefined,
        });
        const suppressNotifications = syncOptions?.suppressNotifications === true ||
          baselineEpoch !== notificationBaselineEpochRef.current;
        const newUnreadEmails = updateNotificationBaseline({
          freshInbox,
          knownEmailIds: knownEmailIdsRef.current,
          readyAccountIds,
          successfullySyncedAccountIds,
          suppressNotifications,
        });
        await notifyNewEmails(newUnreadEmails);
        if (MAIL_TABS.has(activeTabRef.current) && emailsLength <= MAIL_PAGE_SIZE) await loadEmails();
        await refreshUnreadCount();
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
    accountsRef, accountTokensRef, activeTabRef, appControlsRef, emailsLength,
    expiredAccountsRef, knownEmailIdsRef, loadEmails, locale, markSessionExpired,
    notificationBaselineEpochRef, notificationReadyAccountIdsRef, notifyNewEmails,
    refreshUnreadCount, shouldDeferNetwork, showToast, syncAccountWithAutoRefresh,
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
      pendingEventDrivenSyncRef.current ||= syncOptions?.eventDriven === true;
      return existing;
    }

    const flight = runBackgroundSync(syncOptions);
    backgroundSyncFlightRef.current = flight;
    const clearFlight = () => {
      if (backgroundSyncFlightRef.current === flight) {
        backgroundSyncFlightRef.current = null;
        const runAll = pendingFullSyncRef.current;
        const accountIds = [...pendingSyncAccountIdsRef.current];
        const eventDriven = pendingEventDrivenSyncRef.current;
        pendingFullSyncRef.current = false;
        pendingEventDrivenSyncRef.current = false;
        pendingSyncAccountIdsRef.current.clear();
        if (runAll || accountIds.length > 0) {
          void backgroundSync(runAll ? { eventDriven } : { accountIds, eventDriven });
        }
      }
    };
    void flight.then(clearFlight, clearFlight);
    return flight;
  }, [runBackgroundSync]);

  backgroundSyncRef.current = backgroundSync;

  useEffect(() => {
    let cancelled = false;
    const timers = new Set<number>();
    const wait = (milliseconds: number) => new Promise<void>(resolve => {
      const timer = window.setTimeout(() => {
        timers.delete(timer);
        resolve();
      }, milliseconds);
      timers.add(timer);
    });

    const watchAccount = async (accountId: string) => {
      while (!cancelled) {
        try {
          const outcome = await tauriApi.waitForImapChange(accountId);
          if (cancelled) return;
          if (outcome === "changed") {
            await backgroundSyncRef.current({ accountId, eventDriven: true });
          } else {
            await wait(Math.max(syncIntervalDelayMs(syncIntervalSecondsRef.current), 60_000));
            if (!cancelled) await backgroundSyncRef.current({ accountId, eventDriven: true });
          }
        } catch (error) {
          if (cancelled) return;
          if (isAuthFailure(error)) {
            markAccountExpired(accountId, false);
            return;
          }
          await wait(30_000);
        }
      }
    };

    for (const account of accounts) {
      if (account.provider === "imap" && accountTokensRef.current[account.id]) {
        void watchAccount(account.id);
      }
    }

    return () => {
      cancelled = true;
      for (const timer of timers) window.clearTimeout(timer);
      timers.clear();
    };
  }, [accounts, accountTokensRef, backgroundSyncRef, markAccountExpired]);

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

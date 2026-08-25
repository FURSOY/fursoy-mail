import { useState, useEffect, useRef, useCallback, useTransition } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { openUrl } from "@tauri-apps/plugin-opener";
import { getCurrent, onOpenUrl } from "@tauri-apps/plugin-deep-link";
import {
  Edit3, Inbox, AlertTriangle, CheckCircle, XCircle, X,
} from "lucide-react";
import { LocaleContext, locales, type AppLanguage } from "./i18n";
import { surfaces, themePresets, type ThemePresetName } from "./theme";
import "./index.css";
import { normalizeSyncIntervalSeconds } from "./syncInterval";
import { readMailListCache, writeMailListCache, type MailListCache } from "./mailListCache";
import {
  advancedSearchKey, createEmptyAdvancedSearch, isAdvancedSearchActive, searchSidebarTab,
  type AdvancedSearchCriteria,
} from "./advancedSearch";

import {
  type EmailSummary, type ThreadGroup, type AppControls, type OtpMode, type RenderMode,
  type MailZoom, type DensityMode, type MailViewMode, type MailViewPreference,
  type RemoteImageMode, type CustomMailbox, type GmailLabel, DEFAULT_APP_CONTROLS,
} from "./types";
import {
  MAIL_WAKE_THROTTLE_MS, STARTUP_NETWORK_DELAY_MS,
  MAX_MAIL_LIST_CACHE_ENTRIES, MAIL_PAGE_SIZE, ZOOM_STEPS,
  isMailListTab, isSessionRevoked, extractVerificationCode,
  readMailZoom, readThemePreset, getAutoMailViewMode, parseMailtoUrl,
} from "./utils";
import { addBoundedSetValue, MAX_RECENTLY_READ_EMAILS, MAX_REMOTE_IMAGE_EMAILS } from "./boundedSet";

import { Sidebar } from "./components/Sidebar";
import { Onboarding } from "./components/Onboarding";
import { EmailList, type BulkMailAction } from "./components/EmailList";
import { EmailReader } from "./components/EmailReader";
import { SettingsPanel } from "./components/SettingsPanel";
import { ComposeModal } from "./components/ComposeModal";
import { ConfirmModal } from "./components/ConfirmModal";
import { AddMailAccountModal } from "./components/AddMailAccountModal";
import { ToolbarTip } from "./components/ToolbarTip";
import { WindowTitlebar } from "./components/WindowTitlebar";
import { useUpdater } from "./hooks/useUpdater";
import { useAccounts } from "./hooks/useAccounts";
import { useMailSync } from "./hooks/useMailSync";
import { useMailActions } from "./hooks/useMailActions";
import { useMailReader } from "./hooks/useMailReader";
import { tauriApi, type DiscoveredMailProvider, type ImapAccountInput, type MailboxDownloadStatus } from "./tauriApi";
import { enqueueMailMutation, runAuthenticatedMailAction, type MailMutationQueue } from "./mailActionState";

function readTrustedImageSenders(): Record<string, string[]> {
  try {
    const saved = JSON.parse(localStorage.getItem("fursoy_trusted_image_senders") ?? "{}");
    if (!saved || typeof saved !== "object") return {};
    return Object.fromEntries(
      Object.entries(saved).filter(([, senders]) =>
        Array.isArray(senders) && senders.every(sender => typeof sender === "string")
      )
    ) as Record<string, string[]>;
  } catch {
    return {};
  }
}

function getSenderAddress(sender: string): string {
  const match = sender.match(/<([^>]+)>/);
  return (match?.[1] ?? sender).trim().toLowerCase();
}

function mailKey(accountId: string, messageId: string): string {
  return `${accountId}\u0000${messageId}`;
}

function emailKey(email: EmailSummary): string {
  return mailKey(email.account_id, email.id);
}

function threadKey(group: ThreadGroup): string {
  const email = group.latestEmail;
  return `${email.account_id}\u0000${email.thread_id || email.id}`;
}

/// Puts a freshly read first page back at the top without throwing away the
/// pages the user already scrolled into. Anything older than the refreshed page
/// is still theirs to look at; anything inside it comes from the new read, so a
/// thread that moved or disappeared does not survive as a duplicate.
function mergeRefreshedPage(fresh: ThreadGroup[], loaded: ThreadGroup[]): ThreadGroup[] {
  if (fresh.length === 0 || loaded.length <= fresh.length) return fresh;
  const freshKeys = new Set(fresh.map(threadKey));
  const oldestFresh = fresh[fresh.length - 1].latestEmail.date;
  const tail = loaded.filter(group =>
    !freshKeys.has(threadKey(group)) && group.latestEmail.date < oldestFresh);
  return [...fresh, ...tail];
}

function updateGroupLabel(group: ThreadGroup, mail: EmailSummary, labelId: string, applied: boolean): ThreadGroup {
  const groupThreadId = group.latestEmail.thread_id || group.latestEmail.id;
  const mailThreadId = mail.thread_id || mail.id;
  if (group.latestEmail.account_id !== mail.account_id || groupThreadId !== mailThreadId) return group;
  const labelIds = applied
    ? [...new Set([...group.labelIds, labelId])]
    : group.labelIds.filter(id => id !== labelId);
  return { ...group, labelIds };
}

function applyRecentlyRead(group: ThreadGroup, recentlyRead: Set<string>): ThreadGroup {
  if (!recentlyRead.has(emailKey(group.latestEmail))) return group;
  const unreadCount = group.latestEmail.unread ? Math.max(0, group.unreadCount - 1) : group.unreadCount;
  return {
    ...group,
    latestEmail: { ...group.latestEmail, unread: false },
    unreadCount,
    hasUnread: unreadCount > 0,
  };
}

function updateGroupUnread(group: ThreadGroup, mail: EmailSummary, unread: boolean): ThreadGroup {
  if (!sameEmail(group.latestEmail, mail) || group.latestEmail.unread === unread) return group;
  const unreadCount = Math.max(0, group.unreadCount + (unread ? 1 : -1));
  return {
    ...group,
    latestEmail: { ...group.latestEmail, unread },
    unreadCount,
    hasUnread: unreadCount > 0,
  };
}

function sameEmail(left: EmailSummary, right: EmailSummary): boolean {
  return left.id === right.id && left.account_id === right.account_id;
}

const BULK_MAIL_ACTION_CONCURRENCY = 6;

function App() {
  const [activeTab, setActiveTab] = useState<string>("inbox");
  const [selectedMail, setSelectedMail] = useState<string | null>(null);
  const [authStatus, setAuthStatus] = useState<string>("");

  // Settings
  const [syncIntervalValue, setSyncIntervalValue] = useState(() => {
    const saved = localStorage.getItem("fursoy_sync_interval");
    return normalizeSyncIntervalSeconds(saved);
  });
  const [notifDuration, setNotifDuration] = useState(() => {
    const saved = localStorage.getItem("fursoy_notif_duration");
    return saved ? parseInt(saved, 10) : 5;
  });
  const [notifInfinite, setNotifInfinite] = useState(() => {
    return localStorage.getItem("fursoy_notif_infinite") === "true";
  });
  const [pauseOnFullscreen, setPauseOnFullscreen] = useState(() => {
    return localStorage.getItem("fursoy_pause_on_fullscreen") !== "false";
  });
  const [launchAtStartup, setLaunchAtStartup] = useState(false);
  const [startupSettingLoading, setStartupSettingLoading] = useState(false);
  const [lazyBodyLoading, setLazyBodyLoading] = useState(() => {
    return localStorage.getItem("fursoy_lazy_body_loading") !== "false";
  });
  const [renderMode, setRenderMode] = useState<RenderMode>(() => {
    return localStorage.getItem("fursoy_render_mode") === "simple" ? "simple" : "full";
  });
  const [remoteImageMode, setRemoteImageMode] = useState<RemoteImageMode>(() => {
    const saved = localStorage.getItem("fursoy_remote_image_mode");
    return saved === "trusted" || saved === "ask" ? saved : "always";
  });
  const [trustedImageSenders, setTrustedImageSenders] = useState<Record<string, string[]>>(readTrustedImageSenders);
  const [loadedRemoteImageEmails, setLoadedRemoteImageEmails] = useState<Set<string>>(() => new Set());
  const [mailZoom, setMailZoom] = useState<MailZoom>(() => readMailZoom());
  const [mailFitScale, setMailFitScale] = useState(1);
  const [appControls, setAppControls] = useState<AppControls>(DEFAULT_APP_CONTROLS);
  const [otpMode, setOtpMode] = useState<OtpMode>(() => {
    const saved = localStorage.getItem("fursoy_otp_mode");
    return saved === "off" || saved === "strict" ? saved : "balanced";
  });
  const [appLanguage, setAppLanguage] = useState<AppLanguage>(DEFAULT_APP_CONTROLS.appLanguage);
  const tr = locales[appLanguage];
  const [themePreset, setThemePreset] = useState<ThemePresetName>(() => readThemePreset());
  const [densityMode, setDensityMode] = useState<DensityMode>(() => {
    return localStorage.getItem("fursoy_density_mode") === "compact" ? "compact" : "comfortable";
  });
  const [mailViewPreference, setMailViewPreference] = useState<MailViewPreference>(() => {
    const saved = localStorage.getItem("fursoy_mail_view_mode");
    return saved === "split" || saved === "single-toggle" || saved === "inbox-first" ? saved : "auto";
  });
  const [windowWidth, setWindowWidth] = useState(() => window.innerWidth);
  const [singlePanelView, setSinglePanelView] = useState<"list" | "reader">("list");
  const [emails, setEmails] = useState<EmailSummary[]>([]);
  const [mailThreadGroups, setMailThreadGroups] = useState<ThreadGroup[]>([]);
  const [gmailLabelsByAccount, setGmailLabelsByAccount] = useState<Record<string, GmailLabel[]>>({});
  const [customMailboxesByAccount, setCustomMailboxesByAccount] = useState<Record<string, CustomMailbox[]>>({});
  const {
    accounts, accountsLoaded, accountTokens, activeAccountId,
    tokenExpired, expiredAccountIds,
    accountsRef, accountTokensRef, activeAccountIdRef, expiredAccountsRef, tokenExpiredRef,
    setIsConnecting, selectAccount, upsertToken, clearExpiredAccount, expireAccount, setSessionExpired, reloadAccounts,
    initializeAccounts, addImapAccount, addOAuthMailAccount, disconnectAccount, reorderAndReloadAccounts, refreshAccessToken,
  } = useAccounts();

  const [searchQuery, setSearchQuery] = useState("");
  const [advancedSearch, setAdvancedSearch] = useState<AdvancedSearchCriteria>(createEmptyAdvancedSearch);
  const [activeSearchQuery, setActiveSearchQuery] = useState("");
  const [activeAdvancedSearch, setActiveAdvancedSearch] = useState<AdvancedSearchCriteria>(createEmptyAdvancedSearch);
  const [isSearchLoading, setIsSearchLoading] = useState(false);
  const [searchFailed, setSearchFailed] = useState(false);
  const [searchSubmitVersion, setSearchSubmitVersion] = useState(0);
  const [searchResults, setSearchResults] = useState<EmailSummary[] | null>(null);
  const [searchIndexVersion, setSearchIndexVersion] = useState(0);
  const [searchThreadGroups, setSearchThreadGroups] = useState<ThreadGroup[] | null>(null);
  const [hasMoreEmails, setHasMoreEmails] = useState(true);
  const [isLoadingMoreEmails, setIsLoadingMoreEmails] = useState(false);
  const [isMailListLoading, setIsMailListLoading] = useState(false);
  const [mailAppendVersion, setMailAppendVersion] = useState(0);
  const [notificationFocusVersion, setNotificationFocusVersion] = useState(0);
  const [isMailboxBackfilling, setIsMailboxBackfilling] = useState(false);
  const [mailboxDownloadPending, setMailboxDownloadPending] = useState(false);
  const [mailboxDownloadState, setMailboxDownloadState] = useState<MailboxDownloadStatus["state"]>("completed");
  const [isResettingLocalMailbox, setIsResettingLocalMailbox] = useState(false);
  const [mobileMenuOpen, setMobileMenuOpen] = useState(false);
  const [readingToolsOpen, setReadingToolsOpen] = useState(false);
  const [isWindowMaximized, setIsWindowMaximized] = useState(false);
  const [toasts, setToasts] = useState<{ id: number; msg: string; type: "error" | "success" | "info" }[]>([]);
  const [verificationCopyState, setVerificationCopyState] = useState<"idle" | "copied">("idle");
  const [mailAccountModalOpen, setMailAccountModalOpen] = useState(false);

  // Refs
  const searchInputRef = useRef<HTMLInputElement>(null);
  const mailScrollRef = useRef<HTMLDivElement>(null);
  const syncIntervalRef = useRef<number | null>(null);
  const syncChainIdRef = useRef(0);
  const recentNotificationsRef = useRef<Record<string, { accountId: string; messageId: string } | null>>({});
  const lastToastRef = useRef<{ msg: string; type: "error" | "success" | "info"; at: number } | null>(null);
  const toastTimersRef = useRef<Map<number, ReturnType<typeof setTimeout>>>(new Map());
  const previousAutoMailViewModeRef = useRef<MailViewMode | null>(null);
  const backgroundSyncRef = useRef<
    (opts?: { userInitiated?: boolean; suppressNotifications?: boolean }) => Promise<boolean>
  >(async () => false);
  const knownEmailIdsRef = useRef<Set<string>>(new Set());
  const notificationReadyAccountIdsRef = useRef<Set<string>>(new Set());
  const notificationBaselineEpochRef = useRef(0);
  const recentlyReadRef = useRef<Set<string>>(new Set());
  const mailMutationQueueRef = useRef<MailMutationQueue>(new Map());
  const pendingStarMutationsRef = useRef<Set<string>>(new Set());
  const pendingUnreadBadgeDeltasRef = useRef<Map<string, { delta: number; expiresAt: number }>>(new Map());
  const mailPageCursorRef = useRef<ThreadGroup | null>(null);
  const mailListRequestIdRef = useRef(0);
  const searchRequestIdRef = useRef(0);
  const activeSearchQueryRef = useRef("");
  const activeAdvancedSearchRef = useRef<AdvancedSearchCriteria>(createEmptyAdvancedSearch());
  const handledSearchSubmitVersionRef = useRef(0);
  const isLoadingMoreEmailsRef = useRef(false);
  const tabEmailCacheRef = useRef<MailListCache>({});
  const mailListLoadingTimerRef = useRef<number | null>(null);
  const [, startTabTransition] = useTransition();
  const [, startDataTransition] = useTransition();
  const activeTabRef = useRef(activeTab);
  activeTabRef.current = activeTab;
  const pauseOnFullscreenRef = useRef(pauseOnFullscreen);
  pauseOnFullscreenRef.current = pauseOnFullscreen;
  const appControlsRef = useRef(appControls);
  appControlsRef.current = appControls;
  const appControlsSaveQueueRef = useRef<Promise<void>>(Promise.resolve());

  // Derive a "current context" access token (for UI checks and email-less operations)
  const accessToken = (() => {
    if (activeAccountId && accountTokens[activeAccountId]) return accountTokens[activeAccountId];
    const primary = accounts[0];
    if (primary && accountTokens[primary.id]) return accountTokens[primary.id];
    return null;
  })();

  // Look up the right token for a specific email's account
  const getTokenForEmail = (email: EmailSummary | undefined): string => {
    return email ? accountTokens[email.account_id] ?? "" : "";
  };

  useEffect(() => {
    const handleResize = () => setWindowWidth(window.innerWidth);
    handleResize();
    window.addEventListener("resize", handleResize);
    return () => window.removeEventListener("resize", handleResize);
  }, []);

  useEffect(() => {
    const win = getCurrentWindow();
    let disposed = false;
    let unlistenResize: (() => void) | undefined;

    const syncMaximizedState = async () => {
      try {
        const maximized = await win.isMaximized();
        if (!disposed) setIsWindowMaximized(maximized);
      } catch (err) {
        console.error("Failed to read window maximized state:", err);
      }
    };

    void syncMaximizedState();
    win.onResized(() => { void syncMaximizedState(); })
      .then((unlisten) => { if (disposed) unlisten(); else unlistenResize = unlisten; })
      .catch((err) => { console.error("Failed to listen for window resize:", err); });

    return () => { disposed = true; unlistenResize?.(); };
  }, []);

  useEffect(() => {
    const preset = themePresets[themePreset];
    const root = document.documentElement;
    root.style.setProperty("--app-accent", preset.accent);
    root.style.setProperty("--app-accent-hover", preset.accentHover);
    root.style.setProperty("--app-accent-soft", preset.accentSoft);
    root.style.setProperty("--app-accent-shadow", preset.accentShadow);
    root.dataset.density = densityMode;
  }, [themePreset, densityMode]);

  useEffect(() => {
    tauriApi.getLaunchAtStartup().then(setLaunchAtStartup).catch(console.error);
    tauriApi.getAppControls()
      .then((controls) => {
        const savedLanguage: AppLanguage = controls.appLanguage === "tr" ? "tr" : "en";
        const normalized: AppControls = { ...DEFAULT_APP_CONTROLS, ...controls, appLanguage: savedLanguage };
        appControlsRef.current = normalized;
        setAppControls(normalized);
        setAppLanguage(savedLanguage);
      })
      .catch(console.error);
  }, []);

  useEffect(() => {
    const unlistenPromise = listen<AppControls>("app-controls-changed", (event) => {
      const savedLanguage: AppLanguage = event.payload.appLanguage === "tr" ? "tr" : "en";
      const normalized: AppControls = { ...DEFAULT_APP_CONTROLS, ...event.payload, appLanguage: savedLanguage };
      appControlsRef.current = normalized;
      setAppControls(normalized);
      setAppLanguage(savedLanguage);
    });
    return () => { unlistenPromise.then((unlisten) => unlisten()); };
  }, []);

  const showToast = useCallback((msg: string, type: "error" | "success" | "info" = "info") => {
    const id = Date.now();
    const lastToast = lastToastRef.current;
    if (lastToast?.msg === msg && lastToast.type === type && id - lastToast.at < 8000) return;
    lastToastRef.current = { msg, type, at: id };
    setToasts(prev => {
      const deduped = prev.filter(toast => toast.msg !== msg || toast.type !== type);
      return [...deduped.slice(-2), { id, msg, type }];
    });
    const timer = setTimeout(() => {
      toastTimersRef.current.delete(id);
      setToasts(prev => prev.filter(t => t.id !== id));
    }, 4000);
    toastTimersRef.current.set(id, timer);
  }, []);

  useEffect(() => () => {
    for (const timer of toastTimersRef.current.values()) clearTimeout(timer);
    toastTimersRef.current.clear();
  }, []);

  const markAccountExpired = useCallback((accountId: string, showMessage = true) => {
    const { newlyExpired, allExpired } = expireAccount(accountId);
    if (!newlyExpired) return;

    // Per-account notification (only when single account expires, not via markSessionExpired)
    if (showMessage) {
      const email = accountsRef.current.find(a => a.id === accountId)?.email ?? accountId;
      showToast(tr.messages.accountSessionExpired.replace("{email}", email), "error");
    }

    // All accounts expired → banner + stop sync
    if (allExpired && !tokenExpiredRef.current) {
      setSessionExpired(true);
      setIsUserSyncing(false);
      setIsBackgroundSyncing(false);
      syncChainIdRef.current++;
      if (syncIntervalRef.current !== null) {
        clearTimeout(syncIntervalRef.current);
        syncIntervalRef.current = null;
      }
    }
  }, [expireAccount, setSessionExpired, showToast, tokenExpiredRef, tr]);

  // backward-compat alias used in a few places
  const markSessionExpired = useCallback((showMessage = true) => {
    accountsRef.current.forEach(a => markAccountExpired(a.id, false));
    if (showMessage) showToast(tr.messages.reloginRequired, "error");
  }, [markAccountExpired, showToast, tr]);

  const shouldDeferNetworkForGameMode = useCallback(async (userInitiated = false) => {
    if (userInitiated || !pauseOnFullscreenRef.current) return false;
    try {
      return await tauriApi.isSystemFullscreen();
    } catch (e) {
      console.error("Fullscreen check failed:", e);
      return false;
    }
  }, []);

  const {
    currentVersion,
    isCheckingUpdate,
    updateAvailable,
    updateProgress,
    updateError,
    updateStatus,
    checkForUpdates,
    installUpdate,
  } = useUpdater({
    locale: tr,
    showToast,
    shouldDeferNetwork: shouldDeferNetworkForGameMode,
  });

  // The notification listener is registered once, so it reaches the current
  // open handler through a ref rather than closing over the first render's.
  const openMailFromListRef = useRef<(mail: EmailSummary) => Promise<void>>(async () => {});
  const activeMailRef = useRef<EmailSummary | null>(null);

  useEffect(() => {
    const updateScrollTimers = new Set<number>();
    const openNotificationMail = async (messageId: string, accountId?: string) => {
      if (!messageId || !accountId) return;
      if (accountId && accountId !== activeAccountIdRef.current) {
        selectAccount(accountId);
      }
      setMobileMenuOpen(false);
      setSinglePanelView("reader");
      activeTabRef.current = "inbox";
      mailListRequestIdRef.current += 1;
      startTabTransition(() => setActiveTab("inbox"));
      setSelectedMail(mailKey(accountId, messageId));
      setNotificationFocusVersion(version => version + 1);
      const loaded = await loadEmails("inbox");
      await getCurrentWindow().show();
      await getCurrentWindow().unminimize();
      await getCurrentWindow().setFocus();
      // Opening from a notification is opening the mail, so it has to go
      // through the same handler a click in the list does — selecting it alone
      // left the message unread, on the server as well as in the list.
      const opened = loaded.find(mail => emailKey(mail) === mailKey(accountId, messageId));
      if (opened) await openMailFromListRef.current(opened);
    };

    const unlistenCustomPromise = listen<{ emailId?: string; accountId?: string }>("open-notification-mail", async (event) => {
      await openNotificationMail(event.payload?.emailId || "", event.payload?.accountId);
    });
    const unlistenPluginPromise = listen<{ actionId: string; notification: { title: string; body: string } }>(
      "notification-action",
      async (event) => {
        const payload = event.payload?.notification;
        if (!payload) return;
        const key = (payload.title || "") + (payload.body || "");
        const mail = recentNotificationsRef.current[key];
        if (mail) await openNotificationMail(mail.messageId, mail.accountId);
      }
    );
    const unlistenUpdatePromise = listen("open-update-settings", async () => {
      setMobileMenuOpen(false);
      activeTabRef.current = "settings";
      mailListRequestIdRef.current += 1;
      startTabTransition(() => setActiveTab("settings"));
      await getCurrentWindow().show();
      await getCurrentWindow().unminimize();
      await getCurrentWindow().setFocus();
      const timer = window.setTimeout(() => {
        updateScrollTimers.delete(timer);
        document.getElementById("settings-updates")?.scrollIntoView({ block: "center", behavior: "smooth" });
      }, 100);
      updateScrollTimers.add(timer);
    });

    return () => {
      for (const timer of updateScrollTimers) window.clearTimeout(timer);
      unlistenCustomPromise.then(unlisten => unlisten());
      unlistenPluginPromise.then(unlisten => unlisten());
      unlistenUpdatePromise.then(unlisten => unlisten());
    };
  }, []);

  const isMailContextCurrent = (label: string, accountId: string | null) =>
    activeTabRef.current === label && activeAccountIdRef.current === accountId;

  const mailCacheKey = (label: string, accountId: string | null) =>
    `${accountId ?? "__all_accounts__"}\u0000${label}`;

  const loadEmails = async (
    tab?: string,
    options?: { append?: boolean; cursor?: ThreadGroup | null; merge?: boolean },
  ) => {
    try {
      const label = tab || activeTabRef.current;
      if (!isMailListTab(label)) {
        startDataTransition(() => setEmails([]));
        setMailThreadGroups([]);
        return [];
      }
      const accountId = activeAccountIdRef.current; // null = all accounts
      const cursor = options?.cursor ?? null;
      const requestId = ++mailListRequestIdRef.current;
      const result = await tauriApi.getThreadGroupsByLabel({
        label,
        accountId,
        limit: MAIL_PAGE_SIZE,
        beforeDate: cursor?.latestEmail.date ?? null,
        beforeAccountId: cursor?.latestEmail.account_id ?? null,
        beforeThreadId: cursor ? (cursor.latestEmail.thread_id || cursor.latestEmail.id) : null,
      });
      if (requestId !== mailListRequestIdRef.current || !isMailContextCurrent(label, accountId)) {
        return [];
      }
      const adjusted = result.map(group => applyRecentlyRead(group, recentlyReadRef.current));
      if (!options?.append) {
        setHasMoreEmails(adjusted.length === MAIL_PAGE_SIZE);
      }
      if (adjusted.length > 0) {
        mailPageCursorRef.current = adjusted[adjusted.length - 1];
      }

      const cacheKey = mailCacheKey(label, accountId);
      const cachedGroups = readMailListCache(tabEmailCacheRef.current, cacheKey) ?? [];
      const cachedKeys = new Set(cachedGroups.map(threadKey));
      const nextGroups = options?.append
        ? [...cachedGroups, ...adjusted.filter(group => !cachedKeys.has(threadKey(group)))]
        : options?.merge
          ? mergeRefreshedPage(adjusted, cachedGroups)
          : adjusted;
      writeMailListCache(tabEmailCacheRef.current, cacheKey, nextGroups, MAX_MAIL_LIST_CACHE_ENTRIES);
      startDataTransition(() => {
        setMailThreadGroups(nextGroups);
        setEmails(nextGroups.map(group => group.latestEmail));
        if (options?.append && adjusted.length > 0) {
          setMailAppendVersion(version => version + 1);
        }
      });
      return adjusted.map(group => group.latestEmail);
    } catch (e) {
      console.error("Failed to load emails:", e);
      return [];
    }
  };

  const resetMailPagination = () => {
    mailListRequestIdRef.current += 1;
    mailPageCursorRef.current = null;
    isLoadingMoreEmailsRef.current = false;
    setHasMoreEmails(true);
    setIsLoadingMoreEmails(false);
  };

  const loadOlderEmails = async () => {
    const label = activeTabRef.current;
    const accountId = activeAccountIdRef.current;
    if (!isMailListTab(label) || !hasMoreEmails || isLoadingMoreEmailsRef.current) return false;

    isLoadingMoreEmailsRef.current = true;
    setIsLoadingMoreEmails(true);
    try {
      const query = activeSearchQueryRef.current;
      const filters = activeAdvancedSearchRef.current;
      let pageLength = 0;
      if (query || isAdvancedSearchActive(filters)) {
        const cursor = mailPageCursorRef.current;
        const page = await tauriApi.searchLocalThreadGroups({
          query,
          filters,
          accountId,
          limit: MAIL_PAGE_SIZE,
          beforeDate: cursor?.latestEmail.date ?? null,
          beforeAccountId: cursor?.latestEmail.account_id ?? null,
          beforeThreadId: cursor ? (cursor.latestEmail.thread_id || cursor.latestEmail.id) : null,
        });
        if (
          query !== searchQuery.trim()
          || advancedSearchKey(filters) !== advancedSearchKey(advancedSearch)
          || accountId !== activeAccountIdRef.current
        ) return false;
        const adjusted = page.map(group => applyRecentlyRead(group, recentlyReadRef.current));
        if (adjusted.length > 0) mailPageCursorRef.current = adjusted[adjusted.length - 1];
        const current = searchThreadGroups ?? [];
        const seen = new Set(current.map(threadKey));
        const next = [...current, ...adjusted.filter(group => !seen.has(threadKey(group)))];
        setSearchThreadGroups(next);
        setSearchResults(next.map(group => group.latestEmail));
        if (adjusted.length > 0) setMailAppendVersion(version => version + 1);
        pageLength = adjusted.length;
      } else {
        pageLength = (await loadEmails(label, { append: true, cursor: mailPageCursorRef.current })).length;
      }
      const status = await tauriApi.getMailboxDownloadStatus(accountId)
        .catch(() => ({ running: false, pending: false, state: "completed" as const, retryAfter: null }));
      if (!isMailContextCurrent(label, accountId)) return false;
      if (pageLength === 0 && status.pending && !status.running) {
        // Never block the list on Gmail. Request a safe per-account sync and
        // let the existing background worker populate SQLite asynchronously.
        const targets = accountId
          ? [{ id: accountId, token: accountTokensRef.current[accountId] }]
          : accountsRef.current.map(account => ({ id: account.id, token: accountTokensRef.current[account.id] }));
        void Promise.allSettled(
          targets
            .filter((target): target is { id: string; token: string } => !!target.token)
            .map(target => tauriApi.syncEmails(target.id, true))
        );
      }
      setIsMailboxBackfilling(status.running);
      setMailboxDownloadPending(status.pending);
      setMailboxDownloadState(status.state);
      setHasMoreEmails(pageLength === MAIL_PAGE_SIZE || ((!query && !isAdvancedSearchActive(filters)) && (status.running || status.pending)));
      return pageLength > 0;
    } catch (error) {
      console.error("Failed to load older emails:", error);
      showToast(tr.mail.loadOlderFailed, "error");
      return false;
    } finally {
      if (isMailContextCurrent(label, accountId)) {
        isLoadingMoreEmailsRef.current = false;
        setIsLoadingMoreEmails(false);
      }
    }
  };

  const resetLocalMailbox = () => {
    if (isResettingLocalMailbox) return;
    setConfirmModal({
      message: tr.localMailbox.confirm,
      onConfirm: async () => {
        setIsResettingLocalMailbox(true);
        try {
          // Make the reset visible immediately. If the local delete fails, the
          // current list is loaded again below instead of leaving stale rows up.
          tabEmailCacheRef.current = {};
          setEmails([]);
          setMailThreadGroups([]);
          setSelectedMail(null);
          setSelectedMailBody("");
          setSelectedMailBodyId(null);
          resetMailPagination();
          try {
            await tauriApi.resetLocalMailCache(null);
          } catch (error) {
            console.error("Failed to reset local mailbox:", error);
            showToast(tr.localMailbox.resetFailed, "error");
            void loadEmails(activeTabRef.current);
            return;
          }
          recentNotificationsRef.current = {};
          recentlyReadRef.current.clear();
          knownEmailIdsRef.current.clear();
          notificationReadyAccountIdsRef.current.clear();
          notificationBaselineEpochRef.current += 1;
          try {
            await backgroundSyncRef.current({ userInitiated: true, suppressNotifications: true });
            showToast(tr.localMailbox.resetSuccess, "success");
          } catch (error) {
            console.error("Local mailbox was reset but resync failed:", error);
            showToast(tr.localMailbox.resyncFailed, "error");
            void loadEmails(activeTabRef.current);
          }
        } finally {
          setIsResettingLocalMailbox(false);
        }
      },
    });
  };

  const handleLaunchAtStartupChange = async (checked: boolean) => {
    setStartupSettingLoading(true);
    const previous = launchAtStartup;
    setLaunchAtStartup(checked);
    try {
      const actual = await tauriApi.setLaunchAtStartup(checked);
      setLaunchAtStartup(actual);
      showToast(actual ? tr.startup.enabled : tr.startup.disabled, "success");
    } catch (e) {
      console.error("Failed to update startup setting:", e);
      setLaunchAtStartup(previous);
      showToast(`${tr.startup.failed}: ${e}`, "error");
    } finally {
      setStartupSettingLoading(false);
    }
  };

  const updateAppControls = (patch: Partial<AppControls>) => {
    const previous = appControlsRef.current;
    const merged = { ...DEFAULT_APP_CONTROLS, ...previous, ...patch };
    appControlsRef.current = merged;
    setAppControls(merged);
    if (patch.appLanguage) setAppLanguage(patch.appLanguage);

    appControlsSaveQueueRef.current = appControlsSaveQueueRef.current.then(async () => {
      try {
        const saved = await tauriApi.setAppControls(patch);
        if (appControlsRef.current === merged) {
          const normalized = { ...DEFAULT_APP_CONTROLS, ...saved };
          appControlsRef.current = normalized;
          setAppControls(normalized);
          setAppLanguage(normalized.appLanguage);
        }
      } catch (e) {
        console.error("Failed to update app controls:", e);
        if (appControlsRef.current === merged) {
          appControlsRef.current = previous;
          setAppControls(previous);
          setAppLanguage(previous.appLanguage);
        }
        showToast(`${tr.messages.settingSaveFailed}: ${e}`, "error");
      }
    });
  };

  const {
    isUserSyncing,
    isBackgroundSyncing,
    inboxUnread,
    setIsUserSyncing,
    setIsBackgroundSyncing,
    adjustUnreadBadge,
    refreshUnreadCount,
    clearPeriodicSync,
    startPeriodicSync,
  } = useMailSync({
    accounts,
    accountTokens,
    accountsRef,
    accountTokensRef,
    activeAccountId,
    activeTab,
    activeAccountIdRef,
    expiredAccountsRef,
    tokenExpiredRef,
    appControlsRef,
    activeTabRef,
    syncIntervalRef,
    syncChainIdRef,
    backgroundSyncRef,
    recentNotificationsRef,
    knownEmailIdsRef,
    notificationReadyAccountIdsRef,
    notificationBaselineEpochRef,
    pendingUnreadBadgeDeltasRef,
    syncIntervalSeconds: syncIntervalValue,
    notificationDuration: notifDuration,
    notificationInfinite: notifInfinite,
    otpMode,
    appLanguage,
    locale: tr,
    loadEmails: (options?: { merge?: boolean }) => loadEmails(undefined, options),
    refreshOpenThread: (accountIds: Set<string>) => {
      const mail = activeMailRef.current;
      if (mail && accountIds.has(mail.account_id)) setThreadRefreshKey(version => version + 1);
    },
    shouldDeferNetwork: shouldDeferNetworkForGameMode,
    refreshAccessToken,
    upsertToken,
    clearExpiredAccount,
    setSessionExpired,
    markAccountExpired,
    markSessionExpired,
    showToast,
  });

  useEffect(() => {
    let cancelled = false;
    void Promise.all(accounts.map(async account => {
      const labels = await tauriApi.getGmailLabels(account.id).catch(() => []);
      return [account.id, labels] as const;
    })).then(entries => {
      if (!cancelled) setGmailLabelsByAccount(Object.fromEntries(entries));
    });
    return () => { cancelled = true; };
  }, [accounts, isUserSyncing, isBackgroundSyncing]);

  useEffect(() => {
    let cancelled = false;
    void Promise.all(accounts.map(async account => {
      const mailboxes = await tauriApi.getCustomImapMailboxes(account.id).catch(() => []);
      return [account.id, mailboxes] as const;
    })).then(entries => {
      if (!cancelled) setCustomMailboxesByAccount(Object.fromEntries(entries));
    });
    return () => { cancelled = true; };
  }, [accounts, isUserSyncing, isBackgroundSyncing]);

  const openExternalMailUrlRef = useRef<(url: string) => void>(() => {});

  useEffect(() => {
    let cancelled = false;
    let startupSyncTimer: number | null = null;

    refreshUnreadCount();

    // Multi-account startup: load all accounts and their tokens
    initializeAccounts()
      .then(async (loadedAccounts) => {
        if (loadedAccounts.length === 0) return;

        startupSyncTimer = window.setTimeout(() => {
          void (async () => {
            if (cancelled) return;
            if (await shouldDeferNetworkForGameMode(false)) {
              console.log("System in fullscreen/game mode, delaying startup sync.");
            } else {
              // Refresh sessions for all accounts — even those with no cached
              // access token, since refresh_access_token reads the stored
              // credential directly from the keyring. Password accounts report
              // themselves as authenticated without a renewal round trip.
              for (const acc of loadedAccounts) {
                try {
                  const refreshed = await refreshAccessToken(acc.id);
                  if (cancelled) return;
                  if (refreshed.authenticated) upsertToken(acc.id, "active");
                  clearExpiredAccount(acc.id);
                } catch (refreshError) {
                  // Startup is the worst moment to trust a failure: the network
                  // may not be up yet and the credential store may still be
                  // waking. Only a credential the provider itself rejected ends
                  // the session; anything else keeps it, and the sync will find
                  // out for real when the stored token stops working.
                  if (isSessionRevoked(refreshError)) {
                    markAccountExpired(acc.id);
                  } else {
                    console.warn("Startup refresh failed; the stored session remains in use.");
                  }
                }
              }

              if (!cancelled && Object.keys(accountTokensRef.current).length > 0) {
                await backgroundSyncRef.current();
              }
            }
            if (!cancelled) startPeriodicSync();
          })();
        }, STARTUP_NETWORK_DELAY_MS);
      })
      .catch(console.error);

    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "k") {
        e.preventDefault();
        searchInputRef.current?.focus();
      }
      if (e.key === "Escape") {
        setShowReply(false);
        searchInputRef.current?.blur();
      }
    };
    window.addEventListener("keydown", handleKeyDown);

    const unlistenFocus = listen("focus-main-window", async () => {
      const win = getCurrentWindow();
      await win.unminimize();
      await win.show();
      await win.setFocus();
    });

    // While the window is out of sight and notifications are muted the engine
    // deliberately sits still, so the moment it comes back — or the machine is
    // online again — is exactly when the mail has to be fetched, rather than
    // waiting out the periodic timer and the watcher's backoff.
    let lastWakeAt = 0;
    const wakeMail = () => {
      if (Object.keys(accountTokensRef.current).length === 0) return;
      const now = Date.now();
      if (now - lastWakeAt < MAIL_WAKE_THROTTLE_MS) return;
      lastWakeAt = now;
      void tauriApi.wakeImapWatchers().catch(() => {});
      void backgroundSyncRef.current();
    };
    const unlistenVisible = getCurrentWindow().onFocusChanged(({ payload: focused }) => {
      if (focused) wakeMail();
    });
    window.addEventListener("online", wakeMail);

    const handleIframeMessage = (e: MessageEvent) => {
      if (e.data && e.data.type === "open_url" && typeof e.data.url === "string") {
        openExternalMailUrlRef.current(e.data.url);
      }
    };
    window.addEventListener("message", handleIframeMessage);

    return () => {
      cancelled = true;
      if (startupSyncTimer !== null) window.clearTimeout(startupSyncTimer);
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("message", handleIframeMessage);
      window.removeEventListener("online", wakeMail);
      clearPeriodicSync();
      unlistenFocus.then(f => f());
      unlistenVisible.then(f => f());
    };
  }, [shouldDeferNetworkForGameMode, markSessionExpired]);

  useEffect(() => {
    if (mailListLoadingTimerRef.current !== null) {
      window.clearTimeout(mailListLoadingTimerRef.current);
      mailListLoadingTimerRef.current = null;
    }
    if (!isMailListTab(activeTab)) {
      setIsMailListLoading(false);
      startDataTransition(() => setEmails([]));
      setMailThreadGroups([]);
      return;
    }
    resetMailPagination();
    const label = activeTab;
    const accountId = activeAccountId;
    const cached = readMailListCache(tabEmailCacheRef.current, mailCacheKey(label, accountId));
    if (cached !== undefined) {
      setIsMailListLoading(false);
      setMailThreadGroups(cached);
      setEmails(cached.map(group => group.latestEmail));
    } else {
      setMailThreadGroups([]);
      setEmails([]);
      mailListLoadingTimerRef.current = window.setTimeout(() => {
        mailListLoadingTimerRef.current = null;
        if (isMailContextCurrent(label, accountId)) setIsMailListLoading(true);
      }, 150);
    }
    let cancelled = false;
    void loadEmails(label).finally(() => {
      if (cancelled || !isMailContextCurrent(label, accountId)) return;
      if (mailListLoadingTimerRef.current !== null) {
        window.clearTimeout(mailListLoadingTimerRef.current);
        mailListLoadingTimerRef.current = null;
      }
      setIsMailListLoading(false);
    });
    return () => {
      cancelled = true;
      if (mailListLoadingTimerRef.current !== null) {
        window.clearTimeout(mailListLoadingTimerRef.current);
        mailListLoadingTimerRef.current = null;
      }
    };
  }, [activeTab, activeAccountId]);

  // IMAP synchronizes each mailbox in full rather than paging a remote cursor,
  // so there is no separate backfill phase to poll for.
  useEffect(() => {
    if (!accountsLoaded) return;
    setIsMailboxBackfilling(false);
    setMailboxDownloadPending(false);
    setMailboxDownloadState("completed");
  }, [accountsLoaded]);

  useEffect(() => {
    const unlistenPromise = listen("mail-search-index-ready", () => {
      setSearchIndexVersion(version => version + 1);
    });
    return () => { void unlistenPromise.then(unlisten => unlisten()); };
  }, []);

  useEffect(() => {
    const query = searchQuery.trim();
    const filters = advancedSearch;
    const filtersActive = isAdvancedSearchActive(filters);
    const requestId = ++searchRequestIdRef.current;
    const submitImmediately = searchSubmitVersion !== handledSearchSubmitVersionRef.current;
    handledSearchSubmitVersionRef.current = searchSubmitVersion;
    if (!query && !filtersActive) {
      void tauriApi.cancelLocalSearch().catch(() => {});
      activeSearchQueryRef.current = "";
      activeAdvancedSearchRef.current = createEmptyAdvancedSearch();
      setActiveSearchQuery("");
      setActiveAdvancedSearch(createEmptyAdvancedSearch());
      setIsSearchLoading(false);
      setSearchFailed(false);
      setSearchResults(null);
      setSearchThreadGroups(null);
      resetMailPagination();
      const groups = tabEmailCacheRef.current[mailCacheKey(activeTabRef.current, activeAccountIdRef.current)] ?? mailThreadGroups;
      if (groups.length > 0) mailPageCursorRef.current = groups[groups.length - 1];
      setHasMoreEmails(groups.length === 0 || groups.length % MAIL_PAGE_SIZE === 0);
      return;
    }

    const accountId = activeAccountId;
    mailPageCursorRef.current = null;
    setHasMoreEmails(false);
    let searchTimeout = 0;
    const timer = window.setTimeout(() => {
      void (async () => {
        if (searchRequestIdRef.current !== requestId) return;
        activeSearchQueryRef.current = query;
        activeAdvancedSearchRef.current = filters;
        setActiveSearchQuery(query);
        setActiveAdvancedSearch(filters);
        setIsSearchLoading(true);
        setSearchFailed(false);
        searchTimeout = window.setTimeout(() => {
          if (searchRequestIdRef.current !== requestId) return;
          searchRequestIdRef.current += 1;
          void tauriApi.cancelLocalSearch().catch(() => {});
          console.error("Local email search timed out.");
          setSearchResults([]);
          setSearchThreadGroups([]);
          setHasMoreEmails(false);
          setIsSearchLoading(false);
          setSearchFailed(true);
        }, 8_000);
        try {
          const results = await tauriApi.searchLocalThreadGroups({ query, filters, accountId, limit: MAIL_PAGE_SIZE });
          if (
            searchRequestIdRef.current !== requestId ||
            activeAccountIdRef.current !== accountId ||
            advancedSearchKey(filters) !== advancedSearchKey(advancedSearch)
          ) return;
          const adjusted = results.map(group => applyRecentlyRead(group, recentlyReadRef.current));
          setSearchThreadGroups(adjusted);
          setSearchResults(adjusted.map(group => group.latestEmail));
          mailPageCursorRef.current = adjusted[adjusted.length - 1] ?? null;
          setHasMoreEmails(adjusted.length === MAIL_PAGE_SIZE);
          setSearchFailed(false);
        } catch (error) {
          if (searchRequestIdRef.current !== requestId) return;
          console.error("Local email search failed:", error);
          setSearchResults([]);
          setSearchThreadGroups([]);
          setHasMoreEmails(false);
          setSearchFailed(true);
        } finally {
          window.clearTimeout(searchTimeout);
          if (searchRequestIdRef.current === requestId) setIsSearchLoading(false);
        }
      })();
    }, submitImmediately ? 0 : 400);

    return () => {
      window.clearTimeout(timer);
      window.clearTimeout(searchTimeout);
    };
  }, [searchQuery, advancedSearch, activeAccountId, searchIndexVersion, searchSubmitVersion]);

  useEffect(() => {
    if (activeTab !== "settings") return;
    const timer = window.setTimeout(() => {
      tauriApi.getLaunchAtStartup().then(setLaunchAtStartup).catch(console.error);
    }, 250);
    return () => window.clearTimeout(timer);
  }, [activeTab]);

  const goToTab = (tab: typeof activeTab) => {
    setSelectedMail(null);
    setShowReply(false);
    setSinglePanelView("list");
    setMobileMenuOpen(false);
    activeTabRef.current = tab;
    mailListRequestIdRef.current += 1;
    startTabTransition(() => setActiveTab(tab));
  };

  function startInitialImapSync(accountId: string) {
    setAuthStatus(tr.auth.loggedInSyncing);
    const previewTimer = window.setInterval(() => {
      tabEmailCacheRef.current = {};
      void loadEmails("inbox");
    }, 1500);
    void tauriApi.syncImapEmails(accountId).then(async () => {
      await reloadAccounts();
      tabEmailCacheRef.current = {};
      await loadEmails("inbox");
      await refreshUnreadCount();
      setAuthStatus(tr.auth.syncComplete);
      startPeriodicSync();
    }).catch(error => {
      console.error("Initial IMAP sync failed:", error);
      setAuthStatus(tr.auth.syncFailedAfterLogin);
      const errorKey = String(error).replace(/^Error:\s*/i, "");
      const translated = (tr.mailAccount.errors as Record<string, string>)[errorKey];
      showToast(translated ? `${tr.auth.syncFailedAfterLogin} ${translated}` : tr.auth.syncFailedAfterLogin, "error");
    }).finally(() => {
      window.clearInterval(previewTimer);
    });
  }

  async function handleAddImapAccount(input: ImapAccountInput) {
    setIsConnecting(true);
    try {
      const { account } = await addImapAccount(input);
      showToast(tr.mailAccount.added, "success");
      startInitialImapSync(account.id);
    } finally {
      setIsConnecting(false);
    }
  }

  async function handleAddOAuthAccount(email: string, provider: DiscoveredMailProvider) {
    setIsConnecting(true);
    try {
      const { auth } = await addOAuthMailAccount(email, provider);
      showToast(tr.mailAccount.added, "success");
      startInitialImapSync(auth.email);
    } finally {
      setIsConnecting(false);
    }
  }

  async function handleLogoutAccount(accountId: string) {
    try {
      notificationReadyAccountIdsRef.current.delete(accountId);
      const removedAccountPrefix = `${accountId}\u0000`;
      knownEmailIdsRef.current = new Set(
        [...knownEmailIdsRef.current].filter(key => !key.startsWith(removedAccountPrefix))
      );

      const updatedAccounts = await disconnectAccount(accountId);

      if (updatedAccounts.length === 0) {
        clearPeriodicSync();
        setSessionExpired(false);
        setEmails([]);
        setMailThreadGroups([]);
        setSelectedMail(null);
        setSelectedMailBody("");
        setSelectedMailBodyId(null);
        setAuthStatus(tr.auth.loggedOut);
      } else {
        // Reload emails for remaining account context
        tabEmailCacheRef.current = {};
        await loadEmails(activeTabRef.current);
        await refreshUnreadCount();
      }
      showToast(tr.auth.loggedOut, "success");
    } catch (e) {
      console.error("Logout failed:", e);
      showToast(`${tr.messages.signOutFailed}: ${e}`, "error");
    }
  }

  async function handleReorderAccounts(orderedIds: string[]) {
    try {
      await reorderAndReloadAccounts(orderedIds);
    } catch (e) {
      console.error("Reorder failed:", e);
    }
  }

  function handleSwitchAccount(accountId: string | null) {
    if (accountId === activeAccountIdRef.current) return;
    setSelectedMail(null);
    // A label belongs to the account it was read from, so it cannot follow the
    // switch. A folder can, whenever the account being switched to has one by
    // the same name — which is also true of the combined view.
    const reachableMailboxes = accountId
      ? (customMailboxesByAccount[accountId] ?? [])
      : Object.values(customMailboxesByAccount).flat();
    const keepsFolder = activeTabRef.current.startsWith("custom:")
      && reachableMailboxes.some(mailbox => mailbox.role === activeTabRef.current);
    const nextTab = (activeTabRef.current.startsWith("gmail:")
      || (activeTabRef.current.startsWith("custom:") && !keepsFolder))
      ? "inbox"
      : activeTabRef.current;
    const cached = readMailListCache(tabEmailCacheRef.current, mailCacheKey(nextTab, accountId));
    if (cached !== undefined) {
      setMailThreadGroups(cached);
      setEmails(cached.map(group => group.latestEmail));
    } else {
      setMailThreadGroups([]);
      setEmails([]);
    }
    setSearchResults(null);
    setSearchThreadGroups(null);
    selectAccount(accountId);
    if (nextTab !== activeTabRef.current) {
      activeTabRef.current = nextTab;
      setActiveTab(nextTab);
    }
    void refreshUnreadCount();
  }

  const localizedLabelError = (error: unknown): string => {
    const key = String(error).replace(/^Error:\s*/i, "").trim();
    return (tr.mailAccount.errors as Record<string, string>)[key] ?? key;
  };

  const handleCreateGmailLabel = async (accountId: string, name: string): Promise<GmailLabel | null> => {
    try {
      const label = await tauriApi.createGmailLabel(accountId, name);
      setGmailLabelsByAccount(previous => ({
        ...previous,
        [accountId]: [...(previous[accountId] ?? []).filter(item => item.id !== label.id), label]
          .sort((left, right) => left.name.localeCompare(right.name)),
      }));
      showToast(tr.labels.created, "success");
      return label;
    } catch (error) {
      console.error("Create Gmail label failed:", error);
      showToast(`${tr.labels.createFailed}: ${localizedLabelError(error)}`, "error");
      return null;
    }
  };

  const storeUpdatedGmailLabel = (label: GmailLabel) => {
    setGmailLabelsByAccount(previous => ({
      ...previous,
      [label.account_id]: [...(previous[label.account_id] ?? []).filter(item => item.id !== label.id), label]
        .sort((left, right) => left.name.localeCompare(right.name)),
    }));
  };

  const handleRenameGmailLabel = async (label: GmailLabel, name: string): Promise<boolean> => {
    try {
      const updated = await tauriApi.renameGmailLabel(label.account_id, label.id, name);
      storeUpdatedGmailLabel(updated);
      showToast(tr.labels.renamed, "success");
      return true;
    } catch (error) {
      console.error("Rename Gmail label failed:", error);
      showToast(`${tr.labels.renameFailed}: ${localizedLabelError(error)}`, "error");
      return false;
    }
  };

  const handleMoveGmailLabel = async (label: GmailLabel, name: string): Promise<boolean> => {
    try {
      const updated = await tauriApi.renameGmailLabel(label.account_id, label.id, name);
      storeUpdatedGmailLabel(updated);
      showToast(tr.labels.moved, "success");
      return true;
    } catch (error) {
      console.error("Move Gmail label failed:", error);
      showToast(`${tr.labels.moveFailed}: ${localizedLabelError(error)}`, "error");
      return false;
    }
  };

  const handleSetGmailLabelColor = async (
    label: GmailLabel,
    backgroundColor: string | null,
    textColor: string | null,
  ): Promise<boolean> => {
    try {
      const updated = await tauriApi.setGmailLabelColor(
        label.account_id,
        label.id,
        backgroundColor,
        textColor,
      );
      storeUpdatedGmailLabel(updated);
      showToast(tr.labels.colorUpdated, "success");
      return true;
    } catch (error) {
      console.error("Update Gmail label color failed:", error);
      showToast(`${tr.labels.colorFailed}: ${localizedLabelError(error)}`, "error");
      return false;
    }
  };

  const handleDeleteGmailLabel = async (label: GmailLabel) => {
    try {
      await tauriApi.deleteGmailLabel(label.account_id, label.id);
      setGmailLabelsByAccount(previous => ({
        ...previous,
        [label.account_id]: (previous[label.account_id] ?? []).filter(item => item.id !== label.id),
      }));
      const removeLabel = (groups: ThreadGroup[]) => groups.map(group => ({
        ...group,
        labelIds: group.labelIds.filter(id => id !== label.id),
      }));
      setMailThreadGroups(removeLabel);
      setSearchThreadGroups(previous => previous ? removeLabel(previous) : null);
      for (const [key, groups] of Object.entries(tabEmailCacheRef.current)) {
        if (groups) tabEmailCacheRef.current[key] = removeLabel(groups);
      }
      delete tabEmailCacheRef.current[mailCacheKey(`gmail:${label.id}`, label.account_id)];
      if (activeTabRef.current === `gmail:${label.id}` && activeAccountIdRef.current === label.account_id) {
        activeTabRef.current = "inbox";
        setActiveTab("inbox");
        setSelectedMail(null);
        await loadEmails("inbox");
      }
      showToast(tr.labels.deleted, "success");
    } catch (error) {
      console.error("Delete Gmail label failed:", error);
      showToast(`${tr.labels.deleteFailed}: ${localizedLabelError(error)}`, "error");
    }
  };

  const requestDeleteGmailLabel = (label: GmailLabel) => {
    setConfirmModal({
      message: tr.labels.deleteConfirm.replace("{name}", label.name),
      onConfirm: () => { void handleDeleteGmailLabel(label); },
    });
  };

  const handleSetThreadGmailLabel = async (mail: EmailSummary, labelId: string, applied: boolean) => {
    try {
      await tauriApi.setThreadGmailLabel(mail.account_id, mail.thread_id || mail.id, labelId, applied);
      const updateGroups = (groups: ThreadGroup[]) => groups
        .map(group => updateGroupLabel(group, mail, labelId, applied))
        .filter(group => !(activeTabRef.current === `gmail:${labelId}` && !applied &&
          group.latestEmail.account_id === mail.account_id &&
          (group.latestEmail.thread_id || group.latestEmail.id) === (mail.thread_id || mail.id)));
      setMailThreadGroups(updateGroups);
      setSearchThreadGroups(previous => previous ? updateGroups(previous) : null);
      for (const [key, groups] of Object.entries(tabEmailCacheRef.current)) {
        if (groups) tabEmailCacheRef.current[key] = updateGroups(groups);
      }
      if (activeTabRef.current === `gmail:${labelId}` && !applied) setSelectedMail(null);
      showToast(tr.labels.updated, "success");
    } catch (error) {
      console.error("Update Gmail labels failed:", error);
      showToast(`${tr.labels.updateFailed}: ${localizedLabelError(error)}`, "error");
      throw error;
    }
  };

  const handleToggleStarred = async (mail: EmailSummary, starred: boolean) => {
    const mutationKey = `${mail.account_id}\u0000${mail.thread_id || mail.id}`;
    if (pendingStarMutationsRef.current.has(mutationKey)) return;
    pendingStarMutationsRef.current.add(mutationKey);
    const updateGroups = (groups: ThreadGroup[], applied: boolean) =>
      groups.map(group => updateGroupLabel(group, mail, "STARRED", applied));
    const removeThread = (groups: ThreadGroup[]) => groups.filter(group => {
      const candidate = group.latestEmail;
      return candidate.account_id !== mail.account_id
        || (candidate.thread_id || candidate.id) !== (mail.thread_id || mail.id);
    });
    const applyLocalState = (applied: boolean, removeFromStarred = false) => {
      setMailThreadGroups(groups => activeTabRef.current === "starred" && removeFromStarred
        ? removeThread(groups)
        : updateGroups(groups, applied));
      setSearchThreadGroups(groups => groups
        ? (activeAdvancedSearchRef.current.starred && removeFromStarred ? removeThread(groups) : updateGroups(groups, applied))
        : null);
      for (const [key, groups] of Object.entries(tabEmailCacheRef.current)) {
        if (groups) {
          tabEmailCacheRef.current[key] = key.endsWith("\u0000starred") && removeFromStarred
            ? removeThread(groups)
            : updateGroups(groups, applied);
        }
      }
    };

    applyLocalState(starred);
    try {
      await tauriApi.setThreadStarred(mail.account_id, mail.thread_id || mail.id, starred);
      if (!starred) applyLocalState(false, true);
    } catch (error) {
      applyLocalState(!starred);
      console.error("Update Gmail star failed:", error);
      showToast(tr.messages.starUpdateFailed, "error");
    } finally {
      pendingStarMutationsRef.current.delete(mutationKey);
    }
  };

  const handleRefresh = async () => {
    if (Object.keys(accountTokensRef.current).length === 0) {
      showToast(tr.messages.pleaseSignIn, "error");
      return;
    }
    setAuthStatus(tr.messages.syncing);
    const ok = await backgroundSyncRef.current({ userInitiated: true });
    if (ok) {
      setAuthStatus(tr.messages.upToDate);
      showToast(tr.messages.inboxUpdated, "success");
    } else {
      setAuthStatus(tr.messages.refreshFailed);
      showToast(tr.mail.syncFailed, "error");
      const status = await tauriApi.getMailboxDownloadStatus(activeAccountIdRef.current).catch(() => null);
      if (status) {
        setIsMailboxBackfilling(status.running);
        setMailboxDownloadPending(status.pending);
        setMailboxDownloadState(status.state);
      }
    }
  };

  const handleMailClick = async (mail: EmailSummary) => {
    setSelectedMail(emailKey(mail));
    if (mailViewMode !== "split") setSinglePanelView("reader");
    setShowReply(false);
    setReplyTarget(null);
    setReplyText("");
    if (mail.unread) {
      addBoundedSetValue(recentlyReadRef.current, emailKey(mail), MAX_RECENTLY_READ_EMAILS);
      setEmails(prev => prev.map(m => sameEmail(m, mail) ? { ...m, unread: false } : m));
      setSearchResults(prev => prev?.map(m => sameEmail(m, mail) ? { ...m, unread: false } : m) ?? null);
      setMailThreadGroups(prev => prev.map(group => updateGroupUnread(group, mail, false)));
      setSearchThreadGroups(prev => prev?.map(group => updateGroupUnread(group, mail, false)) ?? null);
      adjustUnreadBadge(mail.account_id, -1);
      try {
        await enqueueMailMutation(
          mailMutationQueueRef.current,
          emailKey(mail),
          () => tauriApi.markAsRead(mail.account_id, mail.id),
        );
        recentlyReadRef.current.delete(emailKey(mail));
      } catch (e) {
        console.error("Failed to mark as read:", e);
        if (!recentlyReadRef.current.has(emailKey(mail))) return;
        recentlyReadRef.current.delete(emailKey(mail));
        setEmails(prev => prev.map(m => sameEmail(m, mail) ? { ...m, unread: true } : m));
        setSearchResults(prev => prev?.map(m => sameEmail(m, mail) ? { ...m, unread: true } : m) ?? null);
        setMailThreadGroups(prev => prev.map(group => updateGroupUnread(group, mail, true)));
        setSearchThreadGroups(prev => prev?.map(group => updateGroupUnread(group, mail, true)) ?? null);
        setThreadEmails(prev => prev.map(m => sameEmail(m, mail) ? { ...m, unread: true } : m));
        adjustUnreadBadge(mail.account_id, 1);
      }
    }
  };

  const handleAppLanguageChange = (language: AppLanguage) => {
    updateAppControls({ appLanguage: language });
  };

  const canLoadRemoteImages = useCallback((mail: EmailSummary) => {
    if (remoteImageMode === "always" || loadedRemoteImageEmails.has(mail.id)) return true;
    if (remoteImageMode !== "trusted") return false;
    const sender = getSenderAddress(mail.sender);
    return !!sender && (trustedImageSenders[mail.account_id] ?? []).includes(sender);
  }, [loadedRemoteImageEmails, remoteImageMode, trustedImageSenders]);

  const handleLoadRemoteImages = useCallback((emailId: string) => {
    setLoadedRemoteImageEmails(previous => {
      const next = new Set(previous);
      addBoundedSetValue(next, emailId, MAX_REMOTE_IMAGE_EMAILS);
      return next;
    });
  }, []);

  const handleTrustRemoteImages = useCallback((mail: EmailSummary) => {
    const sender = getSenderAddress(mail.sender);
    if (!sender) return;
    setTrustedImageSenders(previous => {
      const senders = previous[mail.account_id] ?? [];
      if (senders.includes(sender)) return previous;
      const next = { ...previous, [mail.account_id]: [...senders, sender] };
      localStorage.setItem("fursoy_trusted_image_senders", JSON.stringify(next));
      return next;
    });
    handleLoadRemoteImages(mail.id);
  }, [handleLoadRemoteImages]);

  // --- Derived state ---
  const hasSearchQuery = activeSearchQuery.length > 0 || isAdvancedSearchActive(activeAdvancedSearch);
  const sidebarActiveTab = searchSidebarTab(activeTab, hasSearchQuery, activeAdvancedSearch);
  const displayEmails = hasSearchQuery ? (searchResults ?? []) : emails;
  const threadGroups = hasSearchQuery ? (searchThreadGroups ?? []) : mailThreadGroups;
  const activeMail = [...displayEmails, ...emails].find(m => emailKey(m) === selectedMail);
  // The sync hook reaches the open conversation through this, since it is
  // registered long before the reader has one.
  activeMailRef.current = activeMail ?? null;
  const activeThreadGroup = activeMail
    ? threadGroups.find(group => sameEmail(group.latestEmail, activeMail))
      ?? mailThreadGroups.find(group => sameEmail(group.latestEmail, activeMail))
    : undefined;
  const activeMailLabels = activeMail ? (gmailLabelsByAccount[activeMail.account_id] ?? []) : [];
  const activeMailLabelIds = activeThreadGroup?.labelIds ?? [];
  const sidebarGmailLabels = activeAccountId ? (gmailLabelsByAccount[activeAccountId] ?? []) : [];
  // With every account shown at once, the folders of all of them are listed:
  // a user folder is browsed by its own label, which reads across accounts the
  // same way the inbox does. Two accounts with the same folder name share one
  // row, which is what the combined list is for.
  const sidebarCustomMailboxes = activeAccountId
    ? (customMailboxesByAccount[activeAccountId] ?? [])
    : Object.values(customMailboxesByAccount)
      .flat()
      .filter((mailbox, index, all) => all.findIndex(other => other.role === mailbox.role) === index)
      .sort((left, right) => left.name.localeCompare(right.name));
  const activeMailKey = activeMail ? emailKey(activeMail) : null;
  const selectedMailViewMode = mailViewPreference === "auto" ? getAutoMailViewMode(windowWidth) : mailViewPreference;
  const mailViewMode: MailViewMode = selectedMailViewMode === "single-toggle" ? "split" : selectedMailViewMode;

  const {
    selectedMailBody,
    setSelectedMailBody,
    selectedMailBodyId,
    setSelectedMailBodyId,
    isBodyLoading,
    bodyError,
    threadEmails,
    hasMoreThreadEmails,
    isLoadingOlderThread,
    threadMemoryLimitReached,
    loadOlderThreadEmails,
    setThreadEmails,
    setThreadRefreshKey,
  } = useMailReader({
    selectedMail,
    activeMail,
    activeMailKey,
    locale: tr,
    mailScrollRef,
    recentlyReadRef,
    mailMutationQueueRef,
    setEmails,
    setSearchResults,
    setReadingToolsOpen,
    getTokenForEmail,
    adjustUnreadBadge,
  });

  const {
    showReply, setShowReply, replyTarget, setReplyTarget, replyMode, setReplyMode, replyText, setReplyText,
    isSending, showCompose, setShowCompose, confirmModal, setConfirmModal,
    composeTo, setComposeTo, composeSubject, setComposeSubject, composeBody, setComposeBody,
    composeHtmlAppend, setComposeHtmlAppend, composeAccountId, setComposeAccountId,
    composeSendError, setComposeSendError,
    handleArchive, handleTrash, handleReportSpam, handleMoveToInbox, handleMoveToMailbox,
    handleReply, handleComposeSend, handleMarkAsUnread, handleForward,
  } = useMailActions({
    locale: tr,
    accounts,
    accountTokens,
    activeAccountId,
    activeTabRef,
    recentlyReadRef,
    mailMutationQueueRef,
    setEmails,
    setSelectedMail,
    setThreadRefreshKey,
    getTokenForEmail,
    loadEmails,
    refreshUnreadCount,
    adjustUnreadBadge,
    refreshAccessToken,
    upsertToken,
    clearExpiredAccount,
    markAccountExpired,
    showToast,
  });

  const handleBulkMailAction = useCallback(async (action: BulkMailAction, mails: EmailSummary[]) => {
    if (mails.length === 0) return;
    setSelectedMail(null);

    const runForMail = async (mail: EmailSummary): Promise<boolean> => {
      const currentToken = getTokenForEmail(mail);
      if (!currentToken) return false;

      if (action.kind === "unread") recentlyReadRef.current.delete(emailKey(mail));

      try {
        await enqueueMailMutation(
          mailMutationQueueRef.current,
          emailKey(mail),
          () => runAuthenticatedMailAction({
            accountId: mail.account_id,
            currentToken,
            reloginRequiredMessage: tr.messages.reloginRequired,
            refreshAccessToken,
            upsertToken,
            clearExpiredAccount,
            markAccountExpired,
            action: () => {
              const targetId = mail.thread_id || mail.id;
              switch (action.kind) {
                case "archive": return tauriApi.archiveEmail(mail.account_id, targetId);
                case "inbox": return tauriApi.moveToInbox(mail.account_id, targetId);
                case "read": return tauriApi.markThreadAsRead(mail.account_id, targetId);
                case "unread": return tauriApi.markAsUnread(mail.account_id, targetId);
                case "spam": return tauriApi.reportSpam(mail.account_id, targetId);
                case "trash": return tauriApi.trashEmail(mail.account_id, targetId);
                case "label":
                  return tauriApi.setThreadGmailLabel(mail.account_id, targetId, action.labelId, action.applied);
                case "folder":
                  return tauriApi.moveToMailbox(mail.account_id, targetId, action.role);
              }
            },
          }),
        );
        return true;
      } catch (error) {
        console.error(`Bulk mail action failed (${action.kind}):`, error);
        return false;
      }
    };

    let failedCount = 0;
    for (let offset = 0; offset < mails.length; offset += BULK_MAIL_ACTION_CONCURRENCY) {
      const results = await Promise.all(
        mails.slice(offset, offset + BULK_MAIL_ACTION_CONCURRENCY).map(runForMail),
      );
      failedCount += results.filter(succeeded => !succeeded).length;
    }

    await Promise.all([
      loadEmails(activeTabRef.current),
      refreshUnreadCount(),
    ]);
    if (activeSearchQuery.trim() || isAdvancedSearchActive(activeAdvancedSearch)) {
      setSearchSubmitVersion(version => version + 1);
    }
    if (failedCount > 0) showToast(tr.messages.operationFailed, "error");
  }, [
    activeSearchQuery, activeAdvancedSearch, clearExpiredAccount, getTokenForEmail, loadEmails,
    markAccountExpired, refreshAccessToken, refreshUnreadCount, showToast, tr,
    upsertToken,
  ]);

  const openExternalMailUrl = useCallback((url: string) => {
    if (!url || url.startsWith("#")) return;
    let normalized: string;
    try {
      normalized = new URL(url, "https://mail.google.com/").href;
    } catch {
      showToast(tr.actions.openLinkFailed, "error");
      return;
    }
    if (!/^(https?:|mailto:|tel:)/i.test(normalized)) {
      showToast(tr.actions.openLinkFailed, "error");
      return;
    }

    const mailto = parseMailtoUrl(normalized);
    if (mailto) {
      setComposeTo(mailto.to);
      setComposeSubject(mailto.subject);
      setComposeBody(mailto.body);
      setComposeHtmlAppend("");
      setComposeSendError(null);
      setComposeAccountId(activeAccountId ?? accounts[0]?.id ?? null);
      setShowCompose(true);
      return;
    }

    openUrl(normalized).catch((err) => {
      console.error("Failed to open mail link:", err);
      showToast(tr.actions.openLinkFailed, "error");
    });
  }, [
    accounts, activeAccountId, setComposeAccountId, setComposeBody,
    setComposeHtmlAppend, setComposeSendError, setComposeSubject,
    setComposeTo, setShowCompose, showToast, tr,
  ]);
  openExternalMailUrlRef.current = openExternalMailUrl;
  openMailFromListRef.current = handleMailClick;

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    const openMailtoUrls = (urls: string[]) => {
      for (const url of urls) {
        if (/^mailto:/i.test(url)) openExternalMailUrlRef.current(url);
      }
    };

    void getCurrent()
      .then((urls) => {
        if (!disposed && urls) openMailtoUrls(urls);
      })
      .catch((error) => console.error("Failed to read startup mail link:", error));

    void onOpenUrl(openMailtoUrls)
      .then((stopListening) => {
        if (disposed) stopListening();
        else unlisten = stopListening;
      })
      .catch((error) => console.error("Failed to listen for mail links:", error));

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    if (mailViewPreference !== "auto") {
      previousAutoMailViewModeRef.current = null;
      return;
    }
    const previousMode = previousAutoMailViewModeRef.current;
    if (previousMode && previousMode !== mailViewMode) {
      if (mailViewMode === "split" || !selectedMail) setSinglePanelView("list");
      else setSinglePanelView("reader");
    }
    previousAutoMailViewModeRef.current = mailViewMode;
  }, [mailViewMode, mailViewPreference, selectedMail]);

  const closeReader = () => {
    if (selectedMailViewMode !== "single-toggle") setSelectedMail(null);
    setShowReply(false);
    setReplyTarget(null);
    setSinglePanelView("list");
  };

  const previousFixedMailZoomRef = useRef<Exclude<MailZoom, "fit">>(1);

  useEffect(() => {
    if (mailZoom !== "fit") previousFixedMailZoomRef.current = mailZoom;
  }, [mailZoom]);

  const persistMailZoom = useCallback((zoom: MailZoom) => {
    setMailZoom(current => {
      const next = zoom === "fit" && current === "fit"
        ? previousFixedMailZoomRef.current
        : zoom;
      if (next !== "fit") previousFixedMailZoomRef.current = next;
      localStorage.setItem("fursoy_mail_zoom", next === "fit" ? "fit" : String(next));
      return next;
    });
  }, []);

  const stepMailZoom = useCallback((direction: 1 | -1) => {
    setMailZoom(prev => {
      const current = prev === "fit" ? mailFitScale : prev;
      let index = ZOOM_STEPS.findIndex(step => step >= current - 0.001);
      if (index === -1) index = ZOOM_STEPS.length - 1;
      if (direction < 0 && ZOOM_STEPS[index] > current + 0.001 && index > 0) index -= 1;
      const next = Math.min(ZOOM_STEPS.length - 1, Math.max(0, index + direction));
      const value = ZOOM_STEPS[next];
      localStorage.setItem("fursoy_mail_zoom", String(value));
      return value;
    });
  }, [mailFitScale]);

  const effectiveZoomPct = Math.round((mailZoom === "fit" ? mailFitScale : mailZoom) * 100);

  useEffect(() => { setVerificationCopyState("idle"); }, [selectedMail]);
  const unreadCount = inboxUnread;
  const hasLoadedActiveBody = !!activeMail && selectedMailBodyId === selectedMail;
  const verificationCode = activeMail && hasLoadedActiveBody
    ? extractVerificationCode({ ...activeMail, body_html: selectedMailBody }, otpMode, appLanguage)
    : null;
  const activeMailTab = activeMail?.label ?? activeTab;
  const showArchiveBtn = activeMailTab === "inbox" || activeMailTab === "sent";
  const showSpamBtn = activeMailTab === "inbox" || activeMailTab === "archive";
  const showRestoreBtn = activeMailTab === "trash" || activeMailTab === "spam" || activeMailTab === "archive";
  const showTrashToBinBtn = activeMailTab !== "trash";
  const isCompactSidebarMode =
    mailViewPreference === "single-toggle" ||
    (mailViewPreference === "auto" && windowWidth >= 900 && windowWidth < 1280);
  const usesOverlaySidebar = windowWidth < 900 || isCompactSidebarMode;
  const showMailList = mailViewMode === "split" || !selectedMail || singlePanelView === "list";
  const showMailReader = !!activeMail && (mailViewMode === "split" || singlePanelView === "reader");
  const mailListClassName =
    mailViewMode === "split"
      ? `flex min-w-0 flex-col border-r border-[var(--color-border-subtle)] ${surfaces.app} ${selectedMail ? "hidden md:flex md:w-80 lg:w-96" : "flex-1 md:w-80 lg:w-96 md:flex-none"}`
      : showMailList
      ? `flex min-w-0 flex-1 flex-col border-r border-[var(--color-border-subtle)] ${surfaces.app}`
      : "hidden";
  const mailReaderClassName = showMailReader
    ? `flex-1 min-w-0 flex flex-col ${surfaces.content} relative z-10 select-text`
    : "hidden";

  const handleMailViewPreferenceChange = (mode: MailViewPreference) => {
    setMailViewPreference(mode);
    localStorage.setItem("fursoy_mail_view_mode", mode);
    const nextMode = mode === "auto" ? getAutoMailViewMode(windowWidth) : mode;
    setSinglePanelView(
      nextMode === "split" || nextMode === "inbox-first" || !selectedMail ? "list" : singlePanelView
    );
  };

  if (accountsLoaded && accounts.length === 0) {
    return (
      <LocaleContext.Provider value={tr}>
        <>
          <Onboarding
            onConnect={() => setMailAccountModalOpen(true)}
            onCancelConnect={() => setMailAccountModalOpen(false)}
            isConnecting={false}
            isWindowMaximized={isWindowMaximized}
            onWindowMaximizedChange={setIsWindowMaximized}
          />
          <AddMailAccountModal
            open={mailAccountModalOpen}
            onClose={() => setMailAccountModalOpen(false)}
            onAdd={handleAddImapAccount}
            onOAuth={handleAddOAuthAccount}
          />
        </>
      </LocaleContext.Provider>
    );
  }

  return (
    <LocaleContext.Provider value={tr}>
    <div className={`flex flex-col h-screen ${surfaces.app} text-[var(--color-text-secondary)] font-sans overflow-hidden select-none`}>
      <WindowTitlebar
        isMaximized={isWindowMaximized}
        onMaximizedChange={setIsWindowMaximized}
        onMouseDown={(event) => {
          if (!mobileMenuOpen) return;
          if ((event.target as HTMLElement).closest("button")) return;
          setMobileMenuOpen(false);
        }}
      />

      <div className="flex flex-1 overflow-hidden">
        <Sidebar
          activeTab={sidebarActiveTab}
          goToTab={goToTab}
          mobileMenuOpen={mobileMenuOpen}
          setMobileMenuOpen={setMobileMenuOpen}
          authStatus={authStatus}
          isUserSyncing={isUserSyncing}
          unreadCount={unreadCount}
          onLogin={() => setMailAccountModalOpen(true)}
          usesOverlaySidebar={usesOverlaySidebar}
          accounts={accounts}
          activeAccountId={activeAccountId}
          onSwitchAccount={handleSwitchAccount}
          onAddAccount={() => setMailAccountModalOpen(true)}
          onLogoutAccount={handleLogoutAccount}
          expiredAccountIds={expiredAccountIds}
          customMailboxes={sidebarCustomMailboxes}
          gmailLabels={sidebarGmailLabels}
          onRenameGmailLabel={handleRenameGmailLabel}
          onMoveGmailLabel={handleMoveGmailLabel}
          onSetGmailLabelColor={handleSetGmailLabelColor}
          onDeleteGmailLabel={requestDeleteGmailLabel}
        />

        {/* Compose FAB */}
        {accounts.length > 0 && (
          <div className="fixed bottom-6 right-6 z-50">
            <ToolbarTip label={tr.actions.newEmail}>
              <button
                type="button"
                onClick={() => {
                  setComposeTo("");
                  setComposeSubject("");
                  setComposeBody("");
                  setComposeHtmlAppend("");
                  setComposeSendError(null);
                  setComposeAccountId(activeAccountId ?? accounts[0]?.id ?? null);
                  setShowCompose(true);
                }}
                className="w-12 h-12 rounded-full bg-[var(--app-accent)] hover:bg-[var(--app-accent-hover)] text-[var(--color-text-on-accent)] flex items-center justify-center shadow-[var(--shadow-accent-lg)] transition-all hover:scale-105 active:scale-95"
              >
                <Edit3 className="w-5 h-5" />
              </button>
            </ToolbarTip>
          </div>
        )}

        {showCompose && (
          <ComposeModal
            composeTo={composeTo} setComposeTo={setComposeTo}
            composeSubject={composeSubject} setComposeSubject={setComposeSubject}
            composeBody={composeBody} setComposeBody={setComposeBody}
            composeHtmlAppend={composeHtmlAppend}
            isSending={isSending}
            sendError={composeSendError}
            onSend={handleComposeSend}
            onClose={(saved) => {
              setShowCompose(false);
              setComposeTo("");
              setComposeSubject("");
              setComposeBody("");
              setComposeHtmlAppend("");
              setComposeSendError(null);
              if (saved) showToast(tr.compose.draftSaved, "success");
            }}
            onClear={() => {
              setComposeHtmlAppend("");
              setComposeSendError(null);
            }}
            accounts={accounts}
            composeAccountId={composeAccountId}
            setComposeAccountId={setComposeAccountId}
          />
        )}

        <SettingsPanel
          isVisible={activeTab === "settings"}
          usesOverlaySidebar={usesOverlaySidebar}
          onMenuOpen={() => setMobileMenuOpen(open => !open)}
          themePreset={themePreset} setThemePreset={setThemePreset}
          densityMode={densityMode} setDensityMode={setDensityMode}
          syncIntervalValue={syncIntervalValue} setSyncIntervalValue={setSyncIntervalValue}
          launchAtStartup={launchAtStartup}
          startupSettingLoading={startupSettingLoading}
          onLaunchAtStartupChange={handleLaunchAtStartupChange}
          appControls={appControls} onUpdateAppControls={updateAppControls}
          notifDuration={notifDuration} setNotifDuration={setNotifDuration}
          notifInfinite={notifInfinite} setNotifInfinite={setNotifInfinite}
          lazyBodyLoading={lazyBodyLoading} setLazyBodyLoading={setLazyBodyLoading}
          renderMode={renderMode} setRenderMode={setRenderMode}
          remoteImageMode={remoteImageMode} setRemoteImageMode={setRemoteImageMode}
          otpMode={otpMode} setOtpMode={setOtpMode}
          appLanguage={appLanguage} setAppLanguage={handleAppLanguageChange}
          pauseOnFullscreen={pauseOnFullscreen} setPauseOnFullscreen={setPauseOnFullscreen}
          onResetLocalMailbox={resetLocalMailbox}
          isResettingLocalMailbox={isResettingLocalMailbox}
          onShowToast={showToast}
          currentVersion={currentVersion}
          isCheckingUpdate={isCheckingUpdate}
          updateAvailable={updateAvailable}
          updateProgress={updateProgress}
          updateError={updateError}
          updateStatus={updateStatus}
          onCheckForUpdates={checkForUpdates}
          onInstallUpdate={installUpdate}
          accounts={accounts}
          onAddAccount={() => setMailAccountModalOpen(true)}
          onLogoutAccount={handleLogoutAccount}
          onReorderAccounts={handleReorderAccounts}
        />

        {activeTab !== "settings" && (
          <>
            <EmailList
              className={mailListClassName}
              threadGroups={threadGroups}
              selectedMail={selectedMail}
              onMailClick={handleMailClick}
              onToggleStarred={handleToggleStarred}
              onBulkAction={handleBulkMailAction}
              isUserSyncing={isUserSyncing}
              isBackgroundSyncing={isBackgroundSyncing}
              searchQuery={searchQuery}
              highlightQuery={searchQuery.trim()}
              isSearchLoading={isSearchLoading}
              searchFailed={searchFailed}
              setSearchQuery={setSearchQuery}
              onSearchSubmit={() => setSearchSubmitVersion(version => version + 1)}
              advancedSearch={advancedSearch}
              onAdvancedSearch={(criteria) => {
                setAdvancedSearch(criteria);
                setSearchSubmitVersion(version => version + 1);
              }}
              onClearSearch={() => {
                setSearchQuery("");
                setAdvancedSearch(createEmptyAdvancedSearch());
              }}
              searchInputRef={searchInputRef}
              activeTab={activeTab}
              usesOverlaySidebar={usesOverlaySidebar}
              onMenuOpen={() => setMobileMenuOpen(open => !open)}
              mailViewPreference={mailViewPreference}
              onViewPreferenceChange={handleMailViewPreferenceChange}
              onRefresh={handleRefresh}
              onLoadMore={loadOlderEmails}
              hasMoreEmails={hasMoreEmails}
              isLoadingMoreEmails={isLoadingMoreEmails}
              isMailListLoading={isMailListLoading && !hasSearchQuery}
              mailAppendVersion={mailAppendVersion}
              notificationFocusVersion={notificationFocusVersion}
              isMailboxBackfilling={isMailboxBackfilling}
              mailboxDownloadPending={mailboxDownloadPending}
              mailboxDownloadState={mailboxDownloadState}
              accessToken={accessToken}
              accounts={accounts}
              activeAccountId={activeAccountId}
              gmailLabelsByAccount={gmailLabelsByAccount}
              customMailboxesByAccount={customMailboxesByAccount}
              onToggleThreadLabel={handleSetThreadGmailLabel}
              onCreateGmailLabel={handleCreateGmailLabel}
            />
            {activeMail ? (
              <EmailReader
                className={mailReaderClassName}
                activeMail={activeMail}
                activeMailBody={selectedMailBody}
                isBodyLoading={isBodyLoading}
                bodyError={bodyError}
                hasLoadedActiveBody={hasLoadedActiveBody}
                mailViewMode={mailViewMode}
                activeTab={activeMailTab}
                closeReader={closeReader}
                showReply={showReply} setShowReply={setShowReply}
                replyTarget={replyTarget} setReplyTarget={setReplyTarget}
                replyMode={replyMode} setReplyMode={setReplyMode}
                replyText={replyText} setReplyText={setReplyText}
                isSending={isSending}
                onSendReply={handleReply}
                mailZoom={mailZoom}
                setMailFitScale={setMailFitScale}
                stepMailZoom={stepMailZoom}
                persistMailZoom={persistMailZoom}
                effectiveZoomPct={effectiveZoomPct}
                readingToolsOpen={readingToolsOpen} setReadingToolsOpen={setReadingToolsOpen}
                renderMode={renderMode} setRenderMode={setRenderMode}
                remoteImagesAllowedForEmail={canLoadRemoteImages}
                onLoadRemoteImages={handleLoadRemoteImages}
                onTrustRemoteImages={handleTrustRemoteImages}
                verificationCode={verificationCode}
                verificationCopyState={verificationCopyState}
                setVerificationCopyState={setVerificationCopyState}
                showArchiveBtn={showArchiveBtn}
                showSpamBtn={showSpamBtn}
                showRestoreBtn={showRestoreBtn}
                showTrashToBinBtn={showTrashToBinBtn}
                isStarred={activeMailLabelIds.includes("STARRED")}
                onArchive={handleArchive}
                onToggleStarred={handleToggleStarred}
                onReportSpam={handleReportSpam}
                onTrash={handleTrash}
                onMoveToInbox={handleMoveToInbox}
                customMailboxes={customMailboxesByAccount[activeMail.account_id] ?? []}
                onMoveToMailbox={handleMoveToMailbox}
                onMarkAsUnread={handleMarkAsUnread}
                onForward={(mail) => { void handleForward(mail); }}
                onOpenUrl={openExternalMailUrl}
                mailScrollRef={mailScrollRef}
                relayoutKey={`${mailViewMode}|${singlePanelView}|${windowWidth}`}
                threadEmails={threadEmails}
                hasMoreThreadEmails={hasMoreThreadEmails}
                isLoadingOlderThread={isLoadingOlderThread}
                threadMemoryLimitReached={threadMemoryLimitReached}
                onLoadOlderThread={() => { void loadOlderThreadEmails(); }}
                accessToken={getTokenForEmail(activeMail) ?? null}
                showToast={showToast}
                searchQuery={activeSearchQuery}
                gmailLabels={activeMailLabels}
                gmailLabelIds={activeMailLabelIds}
                onToggleGmailLabel={(labelId, applied) => handleSetThreadGmailLabel(activeMail, labelId, applied)}
                onCreateGmailLabel={(name) => handleCreateGmailLabel(activeMail.account_id, name)}
              />
            ) : (
              <main
                className={`${mailViewMode === "split" ? "hidden md:flex" : "hidden"} flex-1 items-center justify-center ${surfaces.content}`}
              >
                <div className="text-center">
                  <div className="w-14 h-14 rounded-2xl bg-white/[0.03] flex items-center justify-center mx-auto mb-3">
                    <Inbox className="w-7 h-7 text-zinc-700" />
                  </div>
                  <h3 className="text-zinc-500 font-medium text-sm">{tr.mail.noSelection}</h3>
                  <p className="text-xs text-zinc-700 mt-1">{tr.mail.noSelectionHint}</p>
                </div>
              </main>
            )}
          </>
        )}
      </div>

      {/* Token expired banner */}
      {tokenExpired && (
        <div className="absolute top-9 left-0 right-0 bg-red-500/90 backdrop-blur-sm px-4 py-2 flex items-center justify-between z-50">
          <div className="flex items-center gap-2 text-white text-xs font-medium">
            <AlertTriangle className="w-3.5 h-3.5" />
            {accounts.length > 1
              ? tr.messages.multipleSessionsExpired.replace(
                  "{emails}",
                  [...expiredAccountIds].map(id => accounts.find(a => a.id === id)?.email ?? id).join(", "),
                )
              : tr.messages.reloginRequired}
          </div>
          <div className="flex items-center gap-1">
            <button
              onClick={() => setMailAccountModalOpen(true)}
              className="px-3 py-1 bg-white text-red-600 text-xs font-semibold rounded hover:bg-red-50 transition-colors"
            >
              {tr.messages.signIn}
            </button>
            <ToolbarTip label={tr.common.close}>
              <button
                type="button"
                onClick={() => setSessionExpired(false)}
                className="flex h-6 w-6 items-center justify-center rounded text-white/80 transition-colors hover:bg-white/20 hover:text-white"
              >
                <X className="h-3.5 w-3.5" />
              </button>
            </ToolbarTip>
          </div>
        </div>
      )}

      <ConfirmModal modal={confirmModal} onClose={() => setConfirmModal(null)} />
      <AddMailAccountModal
        open={mailAccountModalOpen}
        onClose={() => setMailAccountModalOpen(false)}
        onAdd={handleAddImapAccount}
        onOAuth={handleAddOAuthAccount}
      />

      {/* Toast notifications */}
      <div className="fixed bottom-4 right-4 z-[100] flex flex-col gap-2 pointer-events-none max-w-sm">
        {toasts.map(toast => (
          <div
            key={toast.id}
            className={`pointer-events-auto flex items-center gap-2 px-4 py-2.5 rounded-lg shadow-lg text-xs font-medium backdrop-blur-md animate-[slideIn_0.3s_ease] ${
              toast.type === "error"
                ? "bg-red-500/90 text-white"
                : toast.type === "success"
                ? "bg-emerald-500/90 text-white"
                : "bg-zinc-800/90 text-zinc-200 border border-white/10"
            }`}
          >
            {toast.type === "error" && <XCircle className="w-3.5 h-3.5 shrink-0" />}
            {toast.type === "success" && <CheckCircle className="w-3.5 h-3.5 shrink-0" />}
            <span className="flex-1 min-w-0 break-words">{toast.msg}</span>
          </div>
        ))}
      </div>
    </div>
    </LocaleContext.Provider>
  );
}

export default App;

import { Search, X, RefreshCw, Settings, Columns2, PanelLeft, Rows3, Menu } from "lucide-react";
import { useLayoutEffect, useRef } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useLocale } from "../i18n";
import type { Account, EmailSummary, ThreadGroup, MailViewPreference } from "../types";
import { formatDate, splitSearchHighlight } from "../utils";
import { ToolbarTip } from "./ToolbarTip";

interface EmailListProps {
  className: string;
  threadGroups: ThreadGroup[];
  selectedMail: string | null;
  onMailClick: (mail: EmailSummary) => void;
  isUserSyncing: boolean;
  isBackgroundSyncing: boolean;
  searchQuery: string;
  highlightQuery: string;
  isSearchLoading: boolean;
  searchFailed: boolean;
  setSearchQuery: (q: string) => void;
  onSearchSubmit: () => void;
  searchInputRef: React.RefObject<HTMLInputElement | null>;
  activeTab: string;
  usesOverlaySidebar: boolean;
  onMenuOpen: () => void;
  mailViewPreference: MailViewPreference;
  onViewPreferenceChange: (mode: MailViewPreference) => void;
  onRefresh: () => void;
  onLoadMore: () => Promise<boolean>;
  hasMoreEmails: boolean;
  isLoadingMoreEmails: boolean;
  mailAppendVersion: number;
  notificationFocusVersion: number;
  isMailboxBackfilling: boolean;
  mailboxDownloadPending: boolean;
  mailboxDownloadState: "waiting" | "running" | "paused" | "error" | "completed" | "relogin_required" | "rate_limited";
  accessToken: string | null;
  accounts?: Account[];
  activeAccountId?: string | null;
}

function HighlightedText({ text, query }: { text: string; query: string }) {
  return <>{splitSearchHighlight(text, query).map((segment, index) => segment.match
    ? <mark key={`${index}-${segment.text}`} className="rounded-sm bg-yellow-300 px-px text-zinc-950">{segment.text}</mark>
    : <span key={`${index}-${segment.text}`}>{segment.text}</span>
  )}</>;
}

export function EmailList({
  className, threadGroups, selectedMail, onMailClick,
  isUserSyncing, isBackgroundSyncing,
  searchQuery, highlightQuery, isSearchLoading, searchFailed, setSearchQuery, onSearchSubmit, searchInputRef,
  activeTab, usesOverlaySidebar, onMenuOpen,
  mailViewPreference, onViewPreferenceChange,
  onRefresh, onLoadMore, hasMoreEmails, isLoadingMoreEmails, mailAppendVersion, notificationFocusVersion, isMailboxBackfilling, mailboxDownloadPending, mailboxDownloadState, accessToken,
  accounts, activeAccountId,
}: EmailListProps) {
  const tr = useLocale();
  const activeFolderLabel = ({
    inbox: tr.nav.inbox,
    sent: tr.nav.sent,
    archive: tr.nav.archive,
    spam: tr.nav.spam,
    trash: tr.nav.trash,
  } as Record<string, string>)[activeTab] ?? activeTab;
  const showAccountBadge = activeAccountId === null && (accounts?.length ?? 0) > 1;
  const listRef = useRef<HTMLDivElement>(null);
  const pendingLoadScrollTop = useRef<number | null>(null);
  const loadRequestInFlight = useRef(false);
  const ignoreAutoLoadUntil = useRef(0);
  const completedNotificationFocusVersion = useRef(0);
  const rowVirtualizer = useVirtualizer({
    count: threadGroups.length,
    getScrollElement: () => listRef.current,
    estimateSize: () => showAccountBadge ? 98 : 76,
    overscan: 10,
    getItemKey: index => {
      const mail = threadGroups[index]?.latestEmail;
      return mail ? `${mail.account_id}\u0000${mail.thread_id || mail.id}` : index;
    },
  });

  const requestOlderEmails = async () => {
    if (loadRequestInFlight.current || isLoadingMoreEmails) return;
    const list = listRef.current;
    if (list) {
      // Older messages are appended below the current list. Preserve the current
      // viewport anchor instead of the distance from the bottom, which would
      // incorrectly pull a reader back to the new bottom of the list.
      pendingLoadScrollTop.current = list.scrollTop;
    }
    loadRequestInFlight.current = true;
    const appended = await onLoadMore();
    if (!appended) {
      pendingLoadScrollTop.current = null;
      loadRequestInFlight.current = false;
    }
  };

  useLayoutEffect(() => {
    if (pendingLoadScrollTop.current === null) return;
    const list = listRef.current;
    if (list) {
      list.scrollTop = Math.min(
        pendingLoadScrollTop.current,
        Math.max(0, list.scrollHeight - list.clientHeight)
      );
      ignoreAutoLoadUntil.current = Date.now() + 300;
    }
    pendingLoadScrollTop.current = null;
    loadRequestInFlight.current = false;
  }, [mailAppendVersion]);

  useLayoutEffect(() => {
    if (
      notificationFocusVersion === 0 ||
      notificationFocusVersion === completedNotificationFocusVersion.current ||
      !selectedMail
    ) return;
    const index = threadGroups.findIndex(group => `${group.latestEmail.account_id}\u0000${group.latestEmail.id}` === selectedMail);
    if (index < 0) return;
    rowVirtualizer.scrollToIndex(index, { align: "center", behavior: "smooth" });
    completedNotificationFocusVersion.current = notificationFocusVersion;
  }, [notificationFocusVersion, selectedMail, threadGroups, rowVirtualizer]);

  return (
    <section className={className}>
      <div className="h-12 flex items-center px-4 border-b border-white/5 justify-between shrink-0">
        <div className="flex min-w-0 items-center gap-2.5">
          {usesOverlaySidebar && (
            <button
              type="button"
              onClick={onMenuOpen}
              aria-label={tr.settings.openMenu}
              className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md text-zinc-500 hover:bg-white/10 hover:text-zinc-200"
            >
              <Menu className="h-4 w-4" />
            </button>
          )}
          <h2 className="min-w-0 truncate font-semibold text-zinc-100 text-sm" title={activeFolderLabel}>
            {activeFolderLabel}
          </h2>
          {isUserSyncing && (
            <span className="text-[length:var(--font-size-caption)] uppercase tracking-wider text-blue-500 font-semibold animate-pulse">
              {tr.messages.syncing}
            </span>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-1">
          <div className="inline-flex rounded-md border border-white/10 bg-white/[0.03] p-0.5">
            {(
              [
                ["auto", Settings, tr.mail.viewAuto],
                ["split", Columns2, tr.mail.viewSideBySide],
                ["single-toggle", PanelLeft, tr.mail.viewCompact],
                ["inbox-first", Rows3, tr.mail.viewListFocus],
              ] as const
            ).map(([mode, Icon, label]) => (
              <button
                key={mode}
                type="button"
                title={label}
                aria-label={label}
                onClick={() => onViewPreferenceChange(mode)}
                className={`flex h-7 w-7 items-center justify-center rounded text-zinc-500 transition-colors ${
                  mailViewPreference === mode
                    ? "bg-white/10 text-zinc-100"
                    : "hover:bg-white/5 hover:text-zinc-300"
                }`}
              >
                <Icon className="h-3.5 w-3.5" />
              </button>
            ))}
          </div>
          <ToolbarTip label={tr.mail.forceRefresh}>
            <button
              type="button"
              onClick={onRefresh}
              disabled={isUserSyncing || !accessToken}
              className="p-1.5 rounded-md hover:bg-white/10 text-zinc-500 transition-all disabled:opacity-20"
            >
              <RefreshCw className={`w-3.5 h-3.5 ${isUserSyncing ? "animate-spin text-blue-500" : ""}`} />
            </button>
          </ToolbarTip>
        </div>
      </div>

      {/* Search Bar */}
      <div className="p-2 border-b border-white/5">
        <div className="relative group">
          <Search className="w-3.5 h-3.5 absolute left-2.5 top-1/2 -translate-y-1/2 text-zinc-600 group-focus-within:text-blue-500 transition-colors" />
          <input
            ref={searchInputRef}
            type="text"
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") onSearchSubmit();
            }}
            placeholder={tr.mail.searchPlaceholder}
            aria-label={tr.mail.searchPlaceholder}
            className="w-full bg-white/[0.03] border border-white/5 rounded-lg pl-8 pr-7 py-1.5 text-xs outline-none focus:border-blue-500/40 focus:bg-white/[0.02] transition-colors text-zinc-200 placeholder:text-zinc-600 select-text"
          />
          {searchQuery && (
            <button
              type="button"
              onClick={() => setSearchQuery("")}
              aria-label={tr.common.clear}
              className="absolute right-2 top-1/2 -translate-y-1/2 p-0.5 text-zinc-500 hover:text-zinc-300 rounded hover:bg-white/10 transition-colors"
            >
              <X className="w-3 h-3" />
            </button>
          )}
          {isSearchLoading && (
            <span className="absolute right-7 top-1/2 h-2 w-2 -translate-y-1/2 animate-pulse rounded-full bg-blue-500" />
          )}
        </div>
      </div>

      {/* Thread List */}
      <div
        ref={listRef}
        className="flex-1 overflow-y-auto"
        onScroll={(event) => {
          if (Date.now() < ignoreAutoLoadUntil.current) return;
          const element = event.currentTarget;
          if (
            hasMoreEmails &&
            !isLoadingMoreEmails &&
            element.scrollTop + element.clientHeight >= element.scrollHeight - 160
          ) {
            void requestOlderEmails();
          }
        }}
      >
        {threadGroups.length === 0 && !isUserSyncing && !isBackgroundSyncing && (
          <div className="p-8 text-center text-zinc-600 text-xs">
            {isSearchLoading
              ? tr.mail.searching
              : searchFailed
              ? tr.mail.searchFailed
              : searchQuery
              ? tr.mail.searchEmpty
              : activeTab === "inbox"
              ? tr.mail.emptyInbox
              : tr.mail.emptyFolder}
          </div>
        )}
        <div className="relative w-full" style={{ height: `${rowVirtualizer.getTotalSize()}px` }}>
        {rowVirtualizer.getVirtualItems().map((virtualRow) => {
          const group = threadGroups[virtualRow.index];
          const mail = group.latestEmail;
          const isSelected = selectedMail === `${mail.account_id}\u0000${mail.id}`;
          const senderDisplay = activeTab === "sent"
            ? `${tr.compose.toLabelShort}: ${(mail.recipient || "").split("<")[0].replace(/"/g, "").trim() || mail.recipient}`
            : group.participants.slice(0, 3).join(", ");
          const snippetPrefix = group.count > 1
            ? `${mail.sender.split("<")[0].replace(/"/g, "").trim()}: `
            : "";

          return (
            <div
              key={`${mail.account_id}\u0000${mail.thread_id || mail.id}`}
              data-index={virtualRow.index}
              ref={rowVirtualizer.measureElement}
              className="absolute left-0 top-0 w-full"
              style={{ transform: `translateY(${virtualRow.start}px)` }}
            >
            <button
              type="button"
              data-mail-selected={isSelected ? "true" : undefined}
              onClick={() => onMailClick(mail)}
              aria-pressed={isSelected}
              className={`block w-full text-left px-4 py-[var(--mail-row-py)] border-b border-white/[0.03] cursor-pointer transition-all duration-200 relative focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-[var(--app-accent)] ${
                isSelected
                  ? "bg-[var(--app-accent-soft)] border-l-2 border-l-[var(--app-accent)]"
                  : "hover:bg-white/[0.02] border-l-2 border-l-transparent"
              }`}
            >
              {/* Unread dot */}
              {group.hasUnread && (
                <div className="absolute left-1 top-4 w-1.5 h-1.5 rounded-full bg-blue-500" />
              )}

              {/* Row 1: participants + count + date */}
              <div className="flex justify-between items-baseline mb-0.5 gap-2 min-w-0">
                <span
                  className={`min-w-0 truncate text-xs ${group.hasUnread ? "font-semibold text-zinc-100" : "text-zinc-400"}`}
                  title={senderDisplay}
                >
                  <HighlightedText text={senderDisplay} query={highlightQuery} />
                </span>
                <div className="flex items-center gap-1.5 shrink-0">
                  {group.count > 1 && (
                    <span className="text-[length:var(--font-size-caption)] text-zinc-600 tabular-nums">
                      {group.count}
                    </span>
                  )}
                  <span className="text-[length:var(--font-size-caption)] text-zinc-600">{formatDate(mail.date)}</span>
                </div>
              </div>

              {/* Row 2: subject */}
              <h3
                className={`min-w-0 truncate text-xs ${group.hasUnread ? "text-zinc-200 font-medium" : "text-zinc-500"}`}
                title={mail.subject}
              >
                <HighlightedText text={mail.subject} query={highlightQuery} />
              </h3>

              {/* Row 3: snippet */}
              <p className="mt-0.5 min-w-0 truncate text-[length:var(--font-size-metadata)] text-zinc-600" title={mail.snippet}>
                <HighlightedText text={`${snippetPrefix}${mail.snippet}`} query={highlightQuery} />
              </p>

              {/* Account badge (multi-account "all" view) */}
              {showAccountBadge && (() => {
                const acc = accounts?.find(a => a.id === mail.account_id);
                if (!acc) return null;
                return (
                  <div className="mt-1 flex items-center gap-1">
                    {acc.picture ? (
                      <img src={acc.picture} className="w-3.5 h-3.5 rounded-full shrink-0" alt="" />
                    ) : (
                      <div className="w-3.5 h-3.5 rounded-full bg-zinc-700 flex items-center justify-center text-[length:var(--font-size-micro)] font-bold text-zinc-400 shrink-0">
                        {acc.email[0]?.toUpperCase()}
                      </div>
                    )}
                    <span className="text-[length:var(--font-size-caption)] text-zinc-600 truncate">{acc.email}</span>
                  </div>
                );
              })()}
            </button>
            </div>
          );
        })}
        </div>
        {(threadGroups.length > 0 || ["error", "paused", "relogin_required", "rate_limited"].includes(mailboxDownloadState)) && (
          <div className="flex min-h-14 items-center justify-center px-4 text-xs text-zinc-600">
            {isLoadingMoreEmails ? (
              <span className="animate-pulse">{tr.mail.loadingOlder}</span>
            ) : isMailboxBackfilling ? (
              <span className="animate-pulse">{tr.mail.downloadingHistory}</span>
            ) : mailboxDownloadState === "relogin_required" ? (
              <span>{tr.mail.historyDownloadRelogin}</span>
            ) : mailboxDownloadState === "rate_limited" ? (
              <span>{tr.mail.historyDownloadRateLimited}</span>
            ) : mailboxDownloadState === "error" || mailboxDownloadState === "paused" ? (
              <span>{tr.mail.historyDownloadFailed}</span>
            ) : mailboxDownloadPending ? (
              <span>{tr.mail.historyDownloadPending}</span>
            ) : hasMoreEmails ? (
              <button
                type="button"
                onClick={() => { void requestOlderEmails(); }}
                className="rounded-md px-3 py-1.5 text-zinc-500 transition-colors hover:bg-white/5 hover:text-zinc-300"
              >
                {tr.mail.loadOlder}
              </button>
            ) : (
              <span>{tr.mail.allLoaded}</span>
            )}
          </div>
        )}
      </div>
    </section>
  );
}

import {
  Archive, Check, Inbox, Mail, MailOpen, Menu, PanelLeft, RefreshCw,
  Rows3, Search, Settings, ShieldAlert, SlidersHorizontal, Star, Trash2, X, Columns2,
} from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useLocale } from "../i18n";
import type { Account, EmailSummary, GmailLabel, ThreadGroup, MailViewPreference } from "../types";
import { formatDate, splitSearchHighlight } from "../utils";
import { ToolbarTip } from "./ToolbarTip";
import { LabelChips } from "./MailLabels";
import { ProfileAvatar } from "./ProfileAvatar";
import { AdvancedSearchPanel } from "./AdvancedSearchPanel";
import type { AdvancedSearchCriteria } from "../advancedSearch";
import { isAdvancedSearchActive } from "../advancedSearch";

interface EmailListProps {
  className: string;
  threadGroups: ThreadGroup[];
  selectedMail: string | null;
  onMailClick: (mail: EmailSummary) => void;
  onToggleStarred: (mail: EmailSummary, starred: boolean) => Promise<void>;
  onBulkAction: (action: BulkMailAction, mails: EmailSummary[]) => Promise<void>;
  isUserSyncing: boolean;
  isBackgroundSyncing: boolean;
  searchQuery: string;
  highlightQuery: string;
  isSearchLoading: boolean;
  searchFailed: boolean;
  setSearchQuery: (q: string) => void;
  onSearchSubmit: () => void;
  advancedSearch: AdvancedSearchCriteria;
  onAdvancedSearch: (criteria: AdvancedSearchCriteria) => void;
  onClearSearch: () => void;
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
  isMailListLoading: boolean;
  mailAppendVersion: number;
  notificationFocusVersion: number;
  isMailboxBackfilling: boolean;
  mailboxDownloadPending: boolean;
  mailboxDownloadState: "waiting" | "running" | "paused" | "error" | "completed" | "relogin_required" | "rate_limited";
  accessToken: string | null;
  accounts?: Account[];
  activeAccountId?: string | null;
  gmailLabelsByAccount: Record<string, GmailLabel[]>;
}

export type BulkMailAction = "archive" | "inbox" | "read" | "unread" | "spam" | "trash";

function HighlightedText({ text, query }: { text: string; query: string }) {
  return <>{splitSearchHighlight(text, query).map((segment, index) => segment.match
    ? <mark key={`${index}-${segment.text}`} className="rounded-sm bg-yellow-300 px-px text-zinc-950">{segment.text}</mark>
    : <span key={`${index}-${segment.text}`}>{segment.text}</span>
  )}</>;
}

function BulkActionButton({
  label, disabled, onClick, hoverClassName = "hover:text-zinc-100", children,
}: {
  label: string;
  disabled: boolean;
  onClick: () => void;
  hoverClassName?: string;
  children: React.ReactNode;
}) {
  return (
    <ToolbarTip label={label}>
      <button
        type="button"
        onClick={onClick}
        disabled={disabled}
        className={`flex h-7 w-7 items-center justify-center rounded-md text-zinc-500 transition-colors hover:bg-white/5 disabled:pointer-events-none disabled:opacity-25 ${hoverClassName}`}
      >
        {children}
      </button>
    </ToolbarTip>
  );
}

function SelectionMenuItem({ label, onClick }: { label: string; onClick: () => void }) {
  return (
    <button
      type="button"
      role="menuitem"
      onClick={onClick}
      className="block w-full px-3 py-2 text-left text-xs text-[var(--color-text-secondary)] transition-colors hover:bg-[var(--color-surface-hover)] hover:text-[var(--color-text-primary)]"
    >
      {label}
    </button>
  );
}

export function EmailList({
  className, threadGroups, selectedMail, onMailClick, onToggleStarred, onBulkAction,
  isUserSyncing, isBackgroundSyncing,
  searchQuery, highlightQuery, isSearchLoading, searchFailed, setSearchQuery, onSearchSubmit,
  advancedSearch, onAdvancedSearch, onClearSearch, searchInputRef,
  activeTab, usesOverlaySidebar, onMenuOpen,
  mailViewPreference, onViewPreferenceChange,
  onRefresh, onLoadMore, hasMoreEmails, isLoadingMoreEmails, isMailListLoading, mailAppendVersion, notificationFocusVersion, isMailboxBackfilling, mailboxDownloadPending, mailboxDownloadState, accessToken,
  accounts, activeAccountId, gmailLabelsByAccount,
}: EmailListProps) {
  const tr = useLocale();
  const activeGmailLabelId = activeTab.startsWith("gmail:") ? activeTab.slice(6) : null;
  const activeFolderLabel = ({
    inbox: tr.nav.inbox,
    starred: tr.nav.starred,
    all: tr.nav.allMail,
    sent: tr.nav.sent,
    archive: tr.nav.archive,
    spam: tr.nav.spam,
    trash: tr.nav.trash,
  } as Record<string, string>)[activeTab]
    ?? (activeGmailLabelId && activeAccountId
      ? gmailLabelsByAccount[activeAccountId]?.find(label => label.id === activeGmailLabelId)?.name
      : null)
    ?? tr.labels.title;
  const showAccountBadge = activeAccountId === null && (accounts?.length ?? 0) > 1;
  const listRef = useRef<HTMLDivElement>(null);
  const pendingLoadScrollTop = useRef<number | null>(null);
  const loadRequestInFlight = useRef(false);
  const ignoreAutoLoadUntil = useRef(0);
  const completedNotificationFocusVersion = useRef(0);
  const selectionMenuRef = useRef<HTMLDivElement>(null);
  const advancedSearchRef = useRef<HTMLDivElement>(null);
  const [selectedMailKeys, setSelectedMailKeys] = useState<Set<string>>(() => new Set());
  const [bulkActionPending, setBulkActionPending] = useState(false);
  const [selectionMenuOpen, setSelectionMenuOpen] = useState(false);
  const [advancedSearchOpen, setAdvancedSearchOpen] = useState(false);
  const selectionMode = selectedMailKeys.size > 0;
  const loadedMailKeys = threadGroups.map(group => `${group.latestEmail.account_id}\u0000${group.latestEmail.id}`);
  const allLoadedSelected = loadedMailKeys.length > 0 && loadedMailKeys.every(key => selectedMailKeys.has(key));
  const selectedGroups = threadGroups.filter(group => selectedMailKeys.has(
    `${group.latestEmail.account_id}\u0000${group.latestEmail.id}`,
  ));
  const selectedMails = selectedGroups.map(group => group.latestEmail);
  const allSelectedRead = selectedGroups.length > 0 && selectedGroups.every(group => !group.hasUnread);
  const allSelectedUnread = selectedGroups.length > 0 && selectedGroups.every(group => group.hasUnread);

  const clearSelection = () => setSelectedMailKeys(new Set());

  const selectGroups = (predicate: (group: ThreadGroup) => boolean) => {
    setSelectedMailKeys(new Set(
      threadGroups
        .filter(predicate)
        .map(group => `${group.latestEmail.account_id}\u0000${group.latestEmail.id}`),
    ));
    setSelectionMenuOpen(false);
  };

  const toggleMailSelection = (mail: EmailSummary) => {
    const key = `${mail.account_id}\u0000${mail.id}`;
    setSelectedMailKeys(previous => {
      const next = new Set(previous);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  const runBulkAction = async (action: BulkMailAction) => {
    if (bulkActionPending || selectedMails.length === 0) return;
    setBulkActionPending(true);
    try {
      await onBulkAction(action, selectedMails);
      if (action !== "read" && action !== "unread") clearSelection();
    } finally {
      setBulkActionPending(false);
    }
  };

  useEffect(() => {
    clearSelection();
    setSelectionMenuOpen(false);
  }, [activeTab, activeAccountId]);

  useEffect(() => {
    const visible = new Set(loadedMailKeys);
    setSelectedMailKeys(previous => {
      const next = new Set([...previous].filter(key => visible.has(key)));
      if (next.size === previous.size && [...next].every(key => previous.has(key))) return previous;
      return next;
    });
  }, [threadGroups]);

  useEffect(() => {
    if (!selectionMode) return;
    const handleEscape = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || bulkActionPending) return;
      if (advancedSearchOpen) return;
      if (selectionMenuOpen) setSelectionMenuOpen(false);
      else clearSelection();
    };
    window.addEventListener("keydown", handleEscape);
    return () => window.removeEventListener("keydown", handleEscape);
  }, [advancedSearchOpen, bulkActionPending, selectionMenuOpen, selectionMode]);

  useEffect(() => {
    if (!selectionMenuOpen) return;
    const handleOutsideClick = (event: MouseEvent) => {
      if (!selectionMenuRef.current?.contains(event.target as Node)) setSelectionMenuOpen(false);
    };
    window.addEventListener("mousedown", handleOutsideClick);
    return () => window.removeEventListener("mousedown", handleOutsideClick);
  }, [selectionMenuOpen]);

  useEffect(() => {
    if (!advancedSearchOpen) return;
    const handlePointerDown = (event: PointerEvent) => {
      if (!advancedSearchRef.current?.contains(event.target as Node)) setAdvancedSearchOpen(false);
    };
    const handleEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setAdvancedSearchOpen(false);
    };
    window.addEventListener("pointerdown", handlePointerDown);
    window.addEventListener("keydown", handleEscape);
    return () => {
      window.removeEventListener("pointerdown", handlePointerDown);
      window.removeEventListener("keydown", handleEscape);
    };
  }, [advancedSearchOpen]);
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
        <div ref={advancedSearchRef} className="relative group">
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
            className="w-full bg-white/[0.03] border border-white/5 rounded-lg pl-8 pr-16 py-1.5 text-xs outline-none focus:border-blue-500/40 focus:bg-white/[0.02] transition-colors text-zinc-200 placeholder:text-zinc-600 select-text"
          />
          <div className="absolute right-1.5 top-1/2 flex -translate-y-1/2 items-center gap-0.5">
            {(searchQuery || isAdvancedSearchActive(advancedSearch)) && (
              <button
                type="button"
                onClick={onClearSearch}
                aria-label={tr.common.clear}
                className="rounded p-0.5 text-zinc-500 transition-colors hover:bg-white/10 hover:text-zinc-300"
              >
                <X className="w-3 h-3" />
              </button>
            )}
            <button
              type="button"
              aria-label={tr.mail.advancedSearch.title}
              aria-expanded={advancedSearchOpen}
              onClick={() => setAdvancedSearchOpen(open => !open)}
              className={`relative rounded p-1 transition-colors hover:bg-white/10 ${advancedSearchOpen || isAdvancedSearchActive(advancedSearch) ? "text-[var(--app-accent)]" : "text-zinc-500 hover:text-zinc-300"}`}
            >
              <SlidersHorizontal className="h-3.5 w-3.5" />
              {isAdvancedSearchActive(advancedSearch) && <span className="absolute -right-0.5 -top-0.5 h-1.5 w-1.5 rounded-full bg-[var(--app-accent)]" />}
            </button>
          </div>
          {isSearchLoading && (
            <span className="absolute right-16 top-1/2 h-2 w-2 -translate-y-1/2 animate-pulse rounded-full bg-blue-500" />
          )}
          {advancedSearchOpen && (
            <AdvancedSearchPanel
              criteria={advancedSearch}
              gmailLabels={activeAccountId ? (gmailLabelsByAccount[activeAccountId] ?? []) : []}
              onClose={() => setAdvancedSearchOpen(false)}
              onApply={(criteria) => {
                clearSelection();
                setSelectionMenuOpen(false);
                onAdvancedSearch(criteria);
                setAdvancedSearchOpen(false);
              }}
            />
          )}
        </div>
      </div>

      {selectionMode && (
        <div
          className="flex min-h-10 items-center gap-1 border-b border-[var(--color-border-subtle)] bg-[var(--color-surface-panel)] px-2"
          role="toolbar"
          aria-label={tr.mail.bulkActions}
        >
          <ToolbarTip label={tr.actions.clearSelection}>
            <button
              type="button"
              onClick={clearSelection}
              disabled={bulkActionPending}
              className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md text-zinc-500 transition-colors hover:bg-white/5 hover:text-zinc-200 disabled:opacity-40"
            >
              <X className="h-3.5 w-3.5" />
            </button>
          </ToolbarTip>
          <div ref={selectionMenuRef} className="relative flex shrink-0">
            <ToolbarTip label={tr.actions.selectionOptions}>
              <button
                type="button"
                aria-haspopup="menu"
                aria-expanded={selectionMenuOpen}
                onClick={() => setSelectionMenuOpen(open => !open)}
                disabled={bulkActionPending}
                className={`flex h-5 w-5 items-center justify-center rounded-[4px] border transition-colors disabled:opacity-40 ${
                  allLoadedSelected
                    ? "border-[var(--app-accent)] bg-[var(--app-accent)] text-white"
                    : "border-zinc-600 text-zinc-400 hover:border-zinc-400"
                }`}
              >
                {allLoadedSelected ? <Check className="h-3.5 w-3.5" /> : <span className="h-px w-2 bg-current" />}
              </button>
            </ToolbarTip>
            {selectionMenuOpen && (
              <div
                role="menu"
                className="absolute left-0 top-7 z-[210] w-52 overflow-hidden rounded-[var(--radius-md)] border border-[var(--color-border-default)] bg-[var(--color-surface-popover)] py-1 shadow-2xl"
              >
                <SelectionMenuItem label={tr.actions.selectLoaded} onClick={() => selectGroups(() => true)} />
                <SelectionMenuItem label={tr.actions.selectUnread} onClick={() => selectGroups(group => group.hasUnread)} />
                <SelectionMenuItem label={tr.actions.selectRead} onClick={() => selectGroups(group => !group.hasUnread)} />
                <div className="my-1 border-t border-[var(--color-border-subtle)]" />
                <SelectionMenuItem label={tr.actions.selectStarred} onClick={() => selectGroups(group => group.labelIds.includes("STARRED"))} />
                <SelectionMenuItem label={tr.actions.selectUnstarred} onClick={() => selectGroups(group => !group.labelIds.includes("STARRED"))} />
              </div>
            )}
          </div>
          <span className="mr-auto min-w-0 truncate px-1 text-[length:var(--font-size-caption)] font-medium text-zinc-300 tabular-nums">
            {tr.mail.selectedCount.replace("{count}", String(selectedMailKeys.size))}
          </span>
          <div className="flex shrink-0 items-center gap-0.5">
            {(activeTab === "inbox" || activeTab === "sent") && (
              <BulkActionButton label={tr.actions.archive} disabled={bulkActionPending} onClick={() => void runBulkAction("archive")}>
                <Archive className="h-3.5 w-3.5" />
              </BulkActionButton>
            )}
            {(activeTab === "archive" || activeTab === "spam" || activeTab === "trash") && (
              <BulkActionButton label={activeTab === "spam" ? tr.actions.notSpam : tr.actions.restoreInbox} disabled={bulkActionPending} onClick={() => void runBulkAction("inbox")} hoverClassName="hover:text-emerald-400">
                <Inbox className="h-3.5 w-3.5" />
              </BulkActionButton>
            )}
            {!allSelectedRead && (
              <BulkActionButton label={tr.actions.markAsRead} disabled={bulkActionPending} onClick={() => void runBulkAction("read")}>
                <MailOpen className="h-3.5 w-3.5" />
              </BulkActionButton>
            )}
            {!allSelectedUnread && (
              <BulkActionButton label={tr.actions.markAsUnread} disabled={bulkActionPending} onClick={() => void runBulkAction("unread")}>
                <Mail className="h-3.5 w-3.5" />
              </BulkActionButton>
            )}
            {(activeTab === "inbox" || activeTab === "archive") && (
              <BulkActionButton label={tr.actions.reportSpam} disabled={bulkActionPending} onClick={() => void runBulkAction("spam")} hoverClassName="hover:text-orange-400">
                <ShieldAlert className="h-3.5 w-3.5" />
              </BulkActionButton>
            )}
            {activeTab !== "trash" && (
              <BulkActionButton label={tr.actions.moveTrash} disabled={bulkActionPending} onClick={() => void runBulkAction("trash")} hoverClassName="hover:text-red-400">
                <Trash2 className="h-3.5 w-3.5" />
              </BulkActionButton>
            )}
          </div>
        </div>
      )}

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
        {isMailListLoading && threadGroups.length === 0 && (
          <div className="space-y-3 p-4" role="status" aria-live="polite">
            <div className="text-center text-xs text-zinc-500">{tr.mail.loadingMailbox}</div>
            {[0, 1, 2].map(index => (
              <div key={index} className="animate-pulse space-y-2 border-b border-white/[0.03] px-2 pb-3">
                <div className="h-2.5 w-1/3 rounded bg-white/[0.06]" />
                <div className="h-2.5 w-2/3 rounded bg-white/[0.04]" />
                <div className="h-2 w-5/6 rounded bg-white/[0.03]" />
              </div>
            ))}
          </div>
        )}
        {threadGroups.length === 0 && !isMailListLoading && !isUserSyncing && !isBackgroundSyncing && (
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
          const isBulkSelected = selectedMailKeys.has(`${mail.account_id}\u0000${mail.id}`);
          const isStarred = group.labelIds.includes("STARRED");
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
            <div className={`relative border-b border-white/[0.03] transition-colors duration-200 ${
              isSelected || isBulkSelected ? "bg-[var(--app-accent-soft)]" : "hover:bg-white/[0.02]"
            }`}>
              <div className="absolute left-3 top-[var(--mail-row-py)] z-10 flex w-4 flex-col items-center gap-1">
                <ToolbarTip label={isBulkSelected ? tr.actions.deselect : tr.actions.select}>
                  <button
                    type="button"
                    aria-pressed={isBulkSelected}
                    onClick={() => toggleMailSelection(mail)}
                    className={`flex h-3.5 w-3.5 items-center justify-center rounded-[3px] border transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--app-accent)] ${
                      isBulkSelected
                        ? "border-[var(--app-accent)] bg-[var(--app-accent)] text-white"
                        : "border-zinc-600 bg-transparent text-transparent hover:border-zinc-400"
                    }`}
                  >
                    <Check className="h-3 w-3" />
                  </button>
                </ToolbarTip>
                <ToolbarTip label={isStarred ? tr.actions.unstar : tr.actions.star}>
                  <button
                    type="button"
                    aria-pressed={isStarred}
                    onClick={() => { void onToggleStarred(mail, !isStarred); }}
                    className={`flex h-4 w-4 items-center justify-center rounded-sm transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--app-accent)] ${
                      isStarred ? "text-amber-400" : "text-zinc-600 hover:text-amber-400"
                    }`}
                  >
                    <Star className={`h-3.5 w-3.5 ${isStarred ? "fill-current" : ""}`} />
                  </button>
                </ToolbarTip>
              </div>
              <button
                type="button"
                data-mail-selected={isSelected ? "true" : undefined}
                onClick={() => selectionMode ? toggleMailSelection(mail) : onMailClick(mail)}
                aria-pressed={selectionMode ? isBulkSelected : isSelected}
                className={`relative block w-full cursor-pointer py-[var(--mail-row-py)] pl-11 pr-4 text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-[var(--app-accent)] ${
                  isSelected || isBulkSelected ? "border-l-2 border-l-[var(--app-accent)]" : "border-l-2 border-l-transparent"
                }`}
              >
              {/* Unread dot */}
              {group.hasUnread && (
                <div className="absolute left-8 top-4 h-1.5 w-1.5 rounded-full bg-blue-500" />
              )}

              {/* Row 1: participants + count + date */}
              <div className="flex items-center mb-0.5 gap-2 min-w-0">
                <span
                  className={`min-w-[4rem] flex-1 truncate text-xs ${group.hasUnread ? "font-semibold text-zinc-100" : "text-zinc-400"}`}
                  title={senderDisplay}
                >
                  <HighlightedText text={senderDisplay} query={highlightQuery} />
                </span>
                {group.labelIds.length > 0 && (
                  <LabelChips
                    labels={gmailLabelsByAccount[mail.account_id] ?? []}
                    labelIds={group.labelIds}
                    max={3}
                    compact
                  />
                )}
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
                    <ProfileAvatar picture={acc.picture} email={acc.email} className="w-3.5 h-3.5 rounded-full object-cover shrink-0 text-[length:var(--font-size-micro)]" fallbackClassName="bg-zinc-700 text-zinc-400" />
                    <span className="text-[length:var(--font-size-caption)] text-zinc-600 truncate">{acc.email}</span>
                  </div>
                );
              })()}
              </button>
            </div>
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

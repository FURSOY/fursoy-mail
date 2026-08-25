import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Inbox, Send, Archive, Mail, Star, ShieldAlert, Trash2, Settings, LogOut, RefreshCw, Plus, Users, AlertTriangle, Tag, Tags, Folder, ChevronDown, ChevronLeft, ChevronRight, MoreVertical, Pencil, Palette, Ban, Check } from "lucide-react";
import { useLocale } from "../i18n";
import { buildLabelHierarchy, canNestLabelUnder, labelAncestorIds, labelLeafName, labelParentName, nestedLabelName, type LabelHierarchyRow } from "../labelHierarchy";
import { surfaces, ui } from "../theme";
import type { Account, CustomMailbox, GmailLabel } from "../types";
import { ProfileAvatar } from "./ProfileAvatar";

type TabName = "inbox" | "starred" | "all" | "sent" | "archive" | "spam" | "trash" | "settings" | `gmail:${string}` | `custom:${string}`;

const GMAIL_LABEL_COLORS = [
  ["#000000", "#ffffff"], ["#434343", "#ffffff"], ["#666666", "#ffffff"], ["#999999", "#ffffff"],
  ["#cccccc", "#000000"], ["#efefef", "#000000"], ["#fb4c2f", "#ffffff"], ["#ffad47", "#000000"],
  ["#fad165", "#000000"], ["#16a766", "#ffffff"], ["#43d692", "#000000"], ["#4a86e8", "#ffffff"],
  ["#a479e2", "#ffffff"], ["#f691b3", "#000000"], ["#e66550", "#ffffff"], ["#285bac", "#ffffff"],
] as const;

interface SidebarProps {
  activeTab: string;
  goToTab: (tab: TabName) => void;
  mobileMenuOpen: boolean;
  setMobileMenuOpen: (open: boolean | ((prev: boolean) => boolean)) => void;
  authStatus: string;
  isUserSyncing: boolean;
  unreadCount: number;
  onLogin: () => void;
  usesOverlaySidebar: boolean;
  // multi-account
  accounts: Account[];
  activeAccountId: string | null;
  onSwitchAccount: (id: string | null) => void;
  onAddAccount: () => void;
  onLogoutAccount: (accountId: string) => void;
  expiredAccountIds: Set<string>;
  customMailboxes: CustomMailbox[];
  gmailLabels: GmailLabel[];
  onRenameGmailLabel: (label: GmailLabel, name: string) => Promise<boolean>;
  onMoveGmailLabel: (label: GmailLabel, name: string) => Promise<boolean>;
  onSetGmailLabelColor: (label: GmailLabel, backgroundColor: string | null, textColor: string | null) => Promise<boolean>;
  onDeleteGmailLabel: (label: GmailLabel) => void;
}

export function Sidebar({
  activeTab, goToTab, mobileMenuOpen, setMobileMenuOpen,
  authStatus, isUserSyncing, unreadCount, onLogin, usesOverlaySidebar,
  accounts, activeAccountId, onSwitchAccount, onAddAccount, onLogoutAccount,
  expiredAccountIds, customMailboxes, gmailLabels, onRenameGmailLabel, onMoveGmailLabel, onSetGmailLabelColor, onDeleteGmailLabel,
}: SidebarProps) {
  const tr = useLocale();
  const [hoveredAccount, setHoveredAccount] = useState<string | null>(null);
  const [labelsOpen, setLabelsOpen] = useState(true);
  const [collapsedLabelIds, setCollapsedLabelIds] = useState<Set<string>>(() => new Set());
  const [labelMenu, setLabelMenu] = useState<{
    label: GmailLabel;
    top: number;
    left: number;
    view: "main" | "color" | "parent";
  } | null>(null);
  const [renameLabel, setRenameLabel] = useState<GmailLabel | null>(null);
  const [renameName, setRenameName] = useState("");
  const [renameBusy, setRenameBusy] = useState(false);
  const [colorBusy, setColorBusy] = useState(false);
  const [moveBusy, setMoveBusy] = useState(false);
  const labelMenuRef = useRef<HTMLDivElement>(null);
  const activeLabelRef = useRef<HTMLButtonElement>(null);
  const labelRows = useMemo(
    () => buildLabelHierarchy(gmailLabels, collapsedLabelIds),
    [collapsedLabelIds, gmailLabels],
  );

  useEffect(() => {
    if (!activeTab.startsWith("gmail:")) return;
    const labelId = activeTab.slice(6);
    if (!gmailLabels.some(label => label.id === labelId)) return;
    setLabelsOpen(true);
    const ancestors = new Set(labelAncestorIds(gmailLabels, labelId));
    if (ancestors.size === 0) return;
    setCollapsedLabelIds(current => {
      const next = new Set([...current].filter(id => !ancestors.has(id)));
      return next.size === current.size ? current : next;
    });
  }, [activeTab, gmailLabels]);

  useEffect(() => {
    if (!activeTab.startsWith("gmail:")) return;
    activeLabelRef.current?.scrollIntoView({ block: "nearest" });
  }, [activeTab, labelRows, labelsOpen]);

  const moveOpenLabel = async (parentName: string | null) => {
    if (!labelMenu || moveBusy) return;
    const nextName = nestedLabelName(parentName, labelLeafName(labelMenu.label.name));
    if (nextName === labelMenu.label.name) {
      setLabelMenu(null);
      return;
    }
    setMoveBusy(true);
    try {
      const success = await onMoveGmailLabel(labelMenu.label, nextName);
      if (success) setLabelMenu(null);
    } finally {
      setMoveBusy(false);
    }
  };

  useEffect(() => {
    if (!labelMenu) return;
    const closeOutside = (event: MouseEvent) => {
      if (!labelMenuRef.current?.contains(event.target as Node)) setLabelMenu(null);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setLabelMenu(null);
    };
    document.addEventListener("mousedown", closeOutside);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("mousedown", closeOutside);
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [labelMenu]);

  const backdropCls = `fixed inset-x-0 bottom-0 top-9 z-40 bg-black/55 transition-opacity duration-200 ${
    usesOverlaySidebar && mobileMenuOpen ? "pointer-events-auto opacity-100" : "pointer-events-none opacity-0"
  }`;
  const asideCls = usesOverlaySidebar
    ? `fixed left-0 top-9 bottom-0 z-50 flex w-56 flex-col border-r border-[var(--color-border-subtle)] ${surfaces.sidebarOverlay} shadow-2xl shadow-black/40 backdrop-blur-xl transition-transform duration-200 ease-out ${mobileMenuOpen ? "translate-x-0" : "-translate-x-full pointer-events-none"}`
    : `static z-auto flex w-56 flex-col border-r border-[var(--color-border-subtle)] ${surfaces.sidebar} shadow-none`;

  const navItem = (tab: TabName, icon: React.ReactNode, label: string, badge?: React.ReactNode) => (
    <button
      type="button"
      onClick={() => goToTab(tab)}
      aria-current={activeTab === tab ? "page" : undefined}
      title={label}
      className={`w-full min-w-0 overflow-hidden flex items-center gap-3 px-3 py-2 text-sm font-medium rounded-lg transition-all duration-200 ${
        activeTab === tab
          ? "bg-[var(--app-accent-soft)] text-zinc-100 shadow-[inset_2px_0_0_var(--app-accent)]"
          : "text-zinc-400 hover:bg-white/5 hover:text-zinc-200"
      }`}
    >
      <span className="inline-flex shrink-0">{icon}</span>
      <span className="min-w-0 flex-1 truncate text-left">{label}</span>
      {badge}
    </button>
  );

  const labelNavItem = ({ label, displayName, depth, hasChildren }: LabelHierarchyRow) => {
    const tab: TabName = `gmail:${label.id}`;
    const isActive = activeTab === tab;
    return (
      <div key={label.id} className="group/label-row relative min-w-0">
        <button
          ref={isActive ? activeLabelRef : undefined}
          type="button"
          onClick={() => goToTab(tab)}
          aria-current={isActive ? "page" : undefined}
          title={label.name}
          style={{ paddingLeft: 26 + (depth * 14) }}
          className={`flex w-full min-w-0 items-center gap-3 overflow-hidden rounded-lg py-2 pr-9 text-sm font-medium transition-all duration-200 ${
            isActive
              ? "bg-[var(--app-accent-soft)] text-zinc-100 shadow-[inset_2px_0_0_var(--app-accent)]"
              : "text-zinc-400 hover:bg-white/5 hover:text-zinc-200"
          }`}
        >
          <Tag className="h-3.5 w-3.5 shrink-0" style={{ color: label.background_color ?? undefined }} />
          <span className="min-w-0 flex-1 truncate text-left">{displayName}</span>
        </button>
        {hasChildren && (
          <button
            type="button"
            aria-label={collapsedLabelIds.has(label.id) ? tr.labels.expandLabel : tr.labels.collapseLabel}
            aria-expanded={!collapsedLabelIds.has(label.id)}
            onClick={event => {
              event.stopPropagation();
              setCollapsedLabelIds(current => {
                const next = new Set(current);
                if (next.has(label.id)) next.delete(label.id);
                else next.add(label.id);
                return next;
              });
            }}
            className="absolute top-1/2 z-10 flex h-5 w-5 -translate-y-1/2 items-center justify-center rounded text-zinc-600 hover:bg-white/5 hover:text-zinc-300"
            style={{ left: 4 + (depth * 14) }}
          >
            <ChevronRight className={`h-2.5 w-2.5 transition-transform ${collapsedLabelIds.has(label.id) ? "" : "rotate-90"}`} />
          </button>
        )}
        <button
          type="button"
          aria-label={`${tr.labels.moreActions}: ${label.name}`}
          aria-haspopup="menu"
          onClick={event => {
            event.stopPropagation();
            const rect = event.currentTarget.getBoundingClientRect();
            const width = 184;
            setLabelMenu({
              label,
              top: Math.min(window.innerHeight - 280, rect.bottom + 4),
              left: Math.max(8, Math.min(window.innerWidth - width - 8, rect.right - width)),
              view: "main",
            });
          }}
          className="group/more absolute right-1 top-1/2 flex h-7 w-7 -translate-y-1/2 items-center justify-center text-zinc-500 opacity-0 transition-all hover:text-zinc-200 focus-visible:opacity-100 group-hover/label-row:opacity-100"
        >
          <span className="flex h-5 w-4 items-center justify-center rounded transition-colors group-hover/more:bg-white/10 group-focus-visible/more:bg-white/10">
            <MoreVertical className="h-3 w-3" strokeWidth={1.5} />
          </span>
        </button>
      </div>
    );
  };

  const accountItem = (accountId: string | null, picture: string | null, email: string, isAll = false) => {
    const isActive = accountId === null ? activeAccountId === null : activeAccountId === accountId;
    const isHovered = accountId !== null && hoveredAccount === accountId;
    const isExpired = accountId !== null && expiredAccountIds.has(accountId);

    const avatarRingCls = isExpired
      ? "ring-2 ring-orange-500 ring-offset-1 ring-offset-[var(--color-surface-sidebar)]"
      : isActive
      ? "ring-2 ring-[var(--app-accent)] ring-offset-1 ring-offset-[var(--color-surface-sidebar)]"
      : "";

    return (
      <div
        key={accountId ?? "__all__"}
        className="relative"
        onMouseEnter={() => accountId && setHoveredAccount(accountId)}
        onMouseLeave={() => setHoveredAccount(null)}
      >
        <button
          onClick={() => onSwitchAccount(accountId)}
          className={`w-full flex items-center gap-2.5 px-2.5 py-1.5 rounded-lg transition-all duration-200 ${
            isActive
              ? "bg-[var(--app-accent-soft)] shadow-[inset_2px_0_0_var(--app-accent)]"
              : "hover:bg-white/5"
          }`}
        >
          {/* Avatar with optional expired indicator */}
          <div className="relative shrink-0">
            {isAll ? (
              <div className={`w-7 h-7 rounded-full bg-zinc-800 border border-zinc-700 flex items-center justify-center ${isActive ? "ring-2 ring-[var(--app-accent)] ring-offset-1 ring-offset-[var(--color-surface-sidebar)]" : ""}`}>
                <Users className="w-3.5 h-3.5 text-zinc-400" />
              </div>
            ) : (
              <ProfileAvatar
                picture={picture}
                email={email}
                alt={email}
                className={`w-7 h-7 rounded-full object-cover text-xs ${avatarRingCls}`}
              />
            )}
            {isExpired && (
              <span className="absolute -bottom-0.5 -right-0.5 w-3 h-3 rounded-full bg-orange-500 flex items-center justify-center">
                <AlertTriangle className="w-2 h-2 text-white" />
              </span>
            )}
          </div>

          <div className="flex-1 min-w-0">
            <div className={`text-xs font-medium truncate ${isExpired ? "text-orange-400" : isActive ? "text-zinc-100" : "text-zinc-300"}`}>
              {isAll ? tr.mail.allAccounts : email.split("@")[0]}
            </div>
            {!isAll && (
              <div className={`text-[length:var(--font-size-caption)] truncate ${isExpired ? "text-orange-600" : "text-[var(--color-text-disabled)]"}`}>
                {isExpired ? tr.mail.sessionExpired : email}
              </div>
            )}
          </div>
        </button>

        {/* Hover action button */}
        {!isAll && accountId && isHovered && (
          isExpired ? (
            <div className="absolute right-1.5 top-1/2 -translate-y-1/2 group/relogin">
              <button
                type="button"
                aria-label={tr.accounts.reauthenticate}
                onClick={(e) => { e.stopPropagation(); onLogin(); }}
                className="p-1 rounded hover:bg-white/10 text-orange-500 hover:text-orange-300 transition-all"
              >
                <RefreshCw className="w-3 h-3" />
              </button>
              <span className={`pointer-events-none absolute right-0 top-full mt-1 z-[200] w-max opacity-0 transition-opacity duration-150 delay-75 group-hover/relogin:opacity-100 ${ui.tooltip}`}>
                {tr.accounts.reauthenticate}
              </span>
            </div>
          ) : (
            <div className="absolute right-1.5 top-1/2 -translate-y-1/2 group/logout">
              <button
                type="button"
                aria-label={`${tr.accounts.signOut}: ${email}`}
                onClick={(e) => { e.stopPropagation(); onLogoutAccount(accountId); }}
                className="p-1 rounded hover:bg-white/10 text-zinc-500 hover:text-red-400 transition-all"
              >
                <LogOut className="w-3 h-3" />
              </button>
              <span className={`pointer-events-none absolute right-0 top-full mt-1 z-[200] w-max opacity-0 transition-opacity duration-150 delay-75 group-hover/logout:opacity-100 ${ui.tooltip}`}>
                {tr.accounts.signOut}
              </span>
            </div>
          )
        )}
      </div>
    );
  };

  return (
    <>
      <div className={backdropCls} onClick={() => setMobileMenuOpen(false)} aria-hidden={!mobileMenuOpen} />
      <aside className={asideCls}>
        {/* Navigation */}
        <nav className="flex-1 p-2 pt-3 space-y-0.5">
          {navItem(
            "inbox",
            <Inbox className="w-4 h-4" />,
            tr.nav.inbox,
            unreadCount > 0 ? (
              <span className="ml-auto text-[10px] bg-blue-500 text-white min-w-[18px] text-center py-0.5 px-1 rounded-full font-bold">
                {unreadCount}
              </span>
            ) : undefined
          )}
          {navItem("starred", <Star className="w-4 h-4" />, tr.nav.starred)}
          {navItem("all", <Mail className="w-4 h-4" />, tr.nav.allMail)}
          {navItem("sent", <Send className="w-4 h-4" />, tr.nav.sent)}

          <div className="my-2 border-t border-white/5" />

          {navItem("archive", <Archive className="w-4 h-4" />, tr.nav.archive)}
          {navItem("spam", <ShieldAlert className="w-4 h-4" />, tr.nav.spam)}
          {navItem("trash", <Trash2 className="w-4 h-4" />, tr.nav.trash)}

          {customMailboxes.length > 0 && (
            <>
              <div className="my-2 border-t border-white/5" />
              <div className="px-3 pb-1 pt-1 text-[10px] font-semibold uppercase tracking-wide text-zinc-600">
                {tr.nav.folders}
              </div>
              {customMailboxes.map(mailbox =>
                navItem(mailbox.role as TabName, <Folder className="w-4 h-4" />, mailbox.name)
              )}
            </>
          )}

          <div className="my-2 border-t border-white/5" />

          <button
            type="button"
            onClick={() => setLabelsOpen(open => !open)}
            aria-expanded={labelsOpen}
            aria-label={labelsOpen ? tr.labels.collapse : tr.labels.expand}
            className="flex w-full min-w-0 items-center gap-3 rounded-lg px-3 py-2 text-sm font-medium text-zinc-400 transition-colors hover:bg-white/5 hover:text-zinc-200"
          >
            <Tags className="h-4 w-4 shrink-0" />
            <span className="min-w-0 flex-1 truncate text-left">{tr.labels.title}</span>
            <ChevronDown className={`h-3.5 w-3.5 shrink-0 text-zinc-600 transition-transform ${labelsOpen ? "rotate-0" : "-rotate-90"}`} />
          </button>
          {labelsOpen && (
            <div className="label-scrollbar max-h-[188px] space-y-0.5 overflow-y-auto pr-0.5">
              {activeAccountId === null ? (
                <div className="px-3 pb-2 text-[10px] leading-4 text-zinc-600">{tr.labels.chooseAccount}</div>
              ) : labelRows.length > 0 ? (
                labelRows.map(labelNavItem)
              ) : (
                <div className="px-3 pb-2 text-[10px] text-zinc-600">{tr.labels.none}</div>
              )}
            </div>
          )}

          <div className="my-2 border-t border-white/5" />

          {navItem("settings", <Settings className="w-4 h-4" />, tr.nav.settings)}
        </nav>

        {/* Account section */}
        <div className="p-2 border-t border-white/5 space-y-0.5">
          {accounts.length === 0 ? (
            /* No accounts — show login prompt */
            <>
              {authStatus && (
                <div className="px-2 py-1 text-[10px] text-zinc-600">{authStatus}</div>
              )}
              <button
                onClick={onLogin}
                disabled={isUserSyncing}
                className="w-full flex items-center gap-3 px-3 py-2 text-sm font-medium rounded-lg text-zinc-400 hover:bg-white/5 hover:text-zinc-200 transition-colors disabled:opacity-50"
              >
                <Settings className="w-4 h-4" />
                {tr.auth.loginWithGoogle}
                {isUserSyncing && <RefreshCw className="w-3.5 h-3.5 animate-spin text-blue-500 ml-auto" />}
              </button>
            </>
          ) : (
            <>
              {/* "All accounts" combined view — only when 2+ accounts */}
              {accounts.length > 1 && accountItem(null, null, tr.mail.allAccounts, true)}

              {/* Individual accounts */}
              {accounts.map(acc => accountItem(acc.id, acc.picture || null, acc.email))}

              {/* Add account */}
              <button
                onClick={onAddAccount}
                className="w-full flex items-center gap-2.5 px-2.5 py-1.5 rounded-lg text-zinc-600 hover:text-zinc-300 hover:bg-white/5 transition-colors"
              >
                <div className="w-7 h-7 rounded-full border border-dashed border-zinc-700 flex items-center justify-center shrink-0">
                  <Plus className="w-3.5 h-3.5" />
                </div>
                <span className="text-xs">{tr.accounts.add}</span>
              </button>
            </>
          )}
        </div>
      </aside>
      {labelMenu && createPortal(
        <div
          ref={labelMenuRef}
          role="menu"
          aria-label={`${tr.labels.moreActions}: ${labelMenu.label.name}`}
          className={`fixed z-[230] p-1.5 ${ui.modal}`}
          style={{ top: Math.max(8, labelMenu.top), left: labelMenu.left, width: 184 }}
        >
          {labelMenu.view === "color" ? (
            <>
              <div className="mb-1 flex items-center gap-1 border-b border-[var(--color-border-subtle)] pb-1">
                <button
                  type="button"
                  onClick={() => setLabelMenu(current => current ? { ...current, view: "main" } : null)}
                  aria-label={tr.common.back}
                  className="flex h-7 w-7 items-center justify-center rounded-md text-zinc-500 hover:bg-white/5 hover:text-zinc-200"
                >
                  <ChevronLeft className="h-3.5 w-3.5" />
                </button>
                <span className="text-xs font-medium text-zinc-300">{tr.labels.color}</span>
              </div>
              <div className="grid grid-cols-4 gap-1.5 p-1">
                {GMAIL_LABEL_COLORS.map(([backgroundColor, textColor], index) => {
                  const selected = labelMenu.label.background_color === backgroundColor;
                  return (
                    <button
                      key={backgroundColor}
                      type="button"
                      disabled={colorBusy}
                      aria-label={`${tr.labels.color} ${index + 1}`}
                      aria-pressed={selected}
                      onClick={async () => {
                        setColorBusy(true);
                        const success = await onSetGmailLabelColor(labelMenu.label, backgroundColor, textColor);
                        setColorBusy(false);
                        if (success) setLabelMenu(null);
                      }}
                      className={`h-7 rounded-md border transition-transform hover:scale-105 disabled:opacity-50 ${selected ? "border-white ring-1 ring-white/40" : "border-white/10"}`}
                      style={{ backgroundColor }}
                    />
                  );
                })}
              </div>
              <button
                type="button"
                disabled={colorBusy}
                onClick={async () => {
                  setColorBusy(true);
                  const success = await onSetGmailLabelColor(labelMenu.label, null, null);
                  setColorBusy(false);
                  if (success) setLabelMenu(null);
                }}
                className="mt-1 flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs text-zinc-400 hover:bg-white/5 hover:text-zinc-200 disabled:opacity-50"
              >
                <Ban className="h-3.5 w-3.5" />
                {tr.labels.noColor}
              </button>
            </>
          ) : labelMenu.view === "parent" ? (
            <>
              <div className="mb-1 flex items-center gap-1 border-b border-[var(--color-border-subtle)] pb-1">
                <button
                  type="button"
                  onClick={() => setLabelMenu(current => current ? { ...current, view: "main" } : null)}
                  aria-label={tr.common.back}
                  className="flex h-7 w-7 items-center justify-center rounded-md text-zinc-500 hover:bg-white/5 hover:text-zinc-200"
                >
                  <ChevronLeft className="h-3.5 w-3.5" />
                </button>
                <span className="min-w-0 flex-1 truncate text-xs font-medium text-zinc-300">{tr.labels.nestUnder}</span>
              </div>
              <div className="label-scrollbar max-h-52 overflow-y-auto">
                <button
                  type="button"
                  disabled={moveBusy}
                  onClick={() => void moveOpenLabel(null)}
                  className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs text-zinc-300 hover:bg-white/5 disabled:opacity-50"
                >
                  <span className="flex h-3.5 w-3.5 items-center justify-center">
                    {labelParentName(labelMenu.label.name) === null && <Check className="h-3.5 w-3.5 text-[var(--app-accent)]" />}
                  </span>
                  <span className="truncate">{tr.labels.topLevel}</span>
                </button>
                {gmailLabels
                  .filter(candidate => canNestLabelUnder(labelMenu.label, candidate))
                  .map(candidate => (
                    <button
                      key={candidate.id}
                      type="button"
                      disabled={moveBusy}
                      title={candidate.name}
                      onClick={() => void moveOpenLabel(candidate.name)}
                      className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs text-zinc-300 hover:bg-white/5 disabled:opacity-50"
                    >
                      <span className="flex h-3.5 w-3.5 shrink-0 items-center justify-center">
                        {labelParentName(labelMenu.label.name) === candidate.name && <Check className="h-3.5 w-3.5 text-[var(--app-accent)]" />}
                      </span>
                      <span className="min-w-0 flex-1 truncate">{candidate.name}</span>
                    </button>
                  ))}
              </div>
            </>
          ) : (
            <>
              <button
                type="button"
                role="menuitem"
                onClick={() => {
                  setRenameLabel(labelMenu.label);
                  setRenameName(labelLeafName(labelMenu.label.name));
                  setLabelMenu(null);
                }}
                className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs text-zinc-300 hover:bg-white/5"
              >
                <Pencil className="h-3.5 w-3.5 text-zinc-500" />
                {tr.labels.rename}
              </button>
              <button
                type="button"
                role="menuitem"
                onClick={() => setLabelMenu(current => current ? { ...current, view: "color" } : null)}
                className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs text-zinc-300 hover:bg-white/5"
              >
                <Palette className="h-3.5 w-3.5 text-zinc-500" />
                {tr.labels.color}
              </button>
              <button
                type="button"
                role="menuitem"
                onClick={() => setLabelMenu(current => current ? { ...current, view: "parent" } : null)}
                className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs text-zinc-300 hover:bg-white/5"
              >
                <Tags className="h-3.5 w-3.5 text-zinc-500" />
                <span className="min-w-0 flex-1 truncate">{tr.labels.nestUnder}</span>
                <ChevronRight className="h-3.5 w-3.5 text-zinc-600" />
              </button>
              <div className="my-1 border-t border-[var(--color-border-subtle)]" />
              <button
                type="button"
                role="menuitem"
                onClick={() => {
                  onDeleteGmailLabel(labelMenu.label);
                  setLabelMenu(null);
                }}
                className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs text-red-400 hover:bg-red-500/10"
              >
                <Trash2 className="h-3.5 w-3.5" />
                {tr.labels.deleteLabel}
              </button>
            </>
          )}
        </div>,
        document.body,
      )}
      {renameLabel && createPortal(
        <div
          className="fixed inset-0 z-[240] flex items-center justify-center bg-black/60 p-4 backdrop-blur-sm"
          onClick={() => { if (!renameBusy) setRenameLabel(null); }}
        >
          <form
            role="dialog"
            aria-modal="true"
            aria-labelledby="rename-label-title"
            className={`w-full max-w-sm p-5 ${ui.modal}`}
            onClick={event => event.stopPropagation()}
            onSubmit={async event => {
              event.preventDefault();
              const name = renameName.trim();
              if (!name || renameBusy) return;
              setRenameBusy(true);
              const success = await onRenameGmailLabel(
                renameLabel,
                nestedLabelName(labelParentName(renameLabel.name), name),
              );
              setRenameBusy(false);
              if (success) setRenameLabel(null);
            }}
            onKeyDown={event => {
              if (event.key === "Escape" && !renameBusy) setRenameLabel(null);
            }}
          >
            <h2 id="rename-label-title" className="text-sm font-semibold text-zinc-100">{tr.labels.renameTitle}</h2>
            <input
              autoFocus
              maxLength={225}
              value={renameName}
              onChange={event => setRenameName(event.target.value)}
              aria-label={tr.labels.newName}
              className={`mt-4 ${ui.input}`}
            />
            <div className="mt-5 flex justify-end gap-2">
              <button
                type="button"
                disabled={renameBusy}
                onClick={() => setRenameLabel(null)}
                className={ui.buttonSecondary}
              >
                {tr.common.cancel}
              </button>
              <button
                type="submit"
                disabled={renameBusy || !renameName.trim()}
                className={ui.buttonPrimary}
              >
                {tr.common.apply}
              </button>
            </div>
          </form>
        </div>,
        document.body,
      )}
    </>
  );
}

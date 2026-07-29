import { useRef, useState, useEffect, useLayoutEffect, useCallback, type CSSProperties } from "react";
import { createPortal } from "react-dom";
import {
  CornerUpLeft, Inbox, Send, Archive, ShieldAlert, Trash2,
  Users, Forward, Eye, RotateCcw, Minus, Plus, Maximize2,
  Settings, X, RefreshCw, Copy, ChevronDown, ChevronUp,
  Download, FileText, Image, ImageOff, File, Type, Link2, List, ListOrdered, Paperclip, Undo2, Redo2,
} from "lucide-react";
import { locales, useLocale } from "../i18n";
import type { EmailSummary, MailViewMode, MailZoom, RenderMode, AttachmentPayload, SavedDraft } from "../types";
import { tauriApi, type EmailAttachmentInfo } from "../tauriApi";
import { calculateReplyAllRecipients, calculateReplyRecipients, areValidRecipients } from "../mailRecipients";
import type { ReplySendRequest } from "../hooks/useMailActions";
import { buildReplyBody } from "../mailCompose";
import { extractInlineReplyBody, inlineReplyStorageKey, parseStoredInlineReplyDraft, type StoredInlineReplyDraft } from "../inlineReplyDraft";

type AttachmentInfo = EmailAttachmentInfo;

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function AttachmentIcon({ mimeType }: { mimeType: string }) {
  if (mimeType.startsWith("image/")) return <Image className="w-3.5 h-3.5" />;
  if (mimeType === "application/pdf" || mimeType.startsWith("text/")) return <FileText className="w-3.5 h-3.5" />;
  return <File className="w-3.5 h-3.5" />;
}
import { formatDateFull, formatRelativeTime, buildRenderableEmailHtml, hasRemoteEmailImages, normalizeComposerLinkUrl, splitSearchHighlight } from "../utils";
import { EmailHtmlView } from "./EmailHtmlView";
import { ToolbarTip } from "./ToolbarTip";

function SearchHighlightedText({ text, query }: { text: string; query: string }) {
  return <>{splitSearchHighlight(text, query).map((segment, index) => segment.match
    ? <mark key={`${index}-${segment.text}`} className="rounded-sm bg-yellow-300 px-px text-zinc-950">{segment.text}</mark>
    : <span key={`${index}-${segment.text}`}>{segment.text}</span>
  )}</>;
}

// ── Thread card — one email in the conversation stack ──────────────────────────
function ThreadCard({
  email,
  isActive,
  preloadedBody,
  isBodyLoading,
  hasLoadedBody,
  bodyError,
  defaultExpanded,
  renderMode,
  mailZoom,
  relayoutKey,
  onFitScaleChange,
  onOpenUrl,
  remoteImagesAllowed,
  onLoadRemoteImages,
  onTrustRemoteImages,
  scrollRef,
  onReply,
  onReplyAll,
  onForward,
  canReplyAll,
  replyEditorOpen,
  relativeNow,
  collapsible,
  searchQuery,
}: {
  email: EmailSummary;
  isActive: boolean;
  preloadedBody?: string;
  isBodyLoading?: boolean;
  hasLoadedBody?: boolean;
  bodyError?: string | null;
  defaultExpanded: boolean;
  renderMode: RenderMode;
  mailZoom: MailZoom;
  relayoutKey?: string;
  onFitScaleChange?: (scale: number) => void;
  onOpenUrl: (url: string) => void;
  remoteImagesAllowed: boolean;
  onLoadRemoteImages: (emailId: string) => void;
  onTrustRemoteImages: (email: EmailSummary) => void;
  scrollRef: React.RefObject<HTMLElement | null>;
  onReply: () => void;
  onReplyAll: () => void;
  onForward: () => void;
  canReplyAll: boolean;
  replyEditorOpen: boolean;
  relativeNow: number;
  collapsible: boolean;
  searchQuery: string;
}) {
  const tr = useLocale();
  const [expanded, setExpanded] = useState(defaultExpanded);
  const [detailsOpen, setDetailsOpen] = useState(false);
  const [lazyBody, setLazyBody] = useState<string | null>(null);
  const [lazyLoading, setLazyLoading] = useState(false);
  const lazyLoadRequestedRef = useRef(false);
  const detailsTriggerRef = useRef<HTMLButtonElement>(null);
  const detailsPopoverRef = useRef<HTMLDivElement>(null);
  const [detailsPosition, setDetailsPosition] = useState({ top: 0, left: 0, width: 0 });

  useEffect(() => {
    if (!replyEditorOpen) return;
    setExpanded(true);
    if (isActive || lazyBody !== null || lazyLoadRequestedRef.current) return;
    lazyLoadRequestedRef.current = true;
    setLazyLoading(true);
    void tauriApi.getEmailBody(email.id, email.account_id)
      .then(body => setLazyBody(body || ""))
      .catch(() => setLazyBody(""))
      .finally(() => setLazyLoading(false));
  }, [email.account_id, email.id, isActive, lazyBody, replyEditorOpen]);

  const senderAddressMatch = email.sender.match(/<\s*([^>]+)\s*>/);
  const senderAddress = (senderAddressMatch?.[1] ?? (email.sender.includes("@") ? email.sender : "")).trim();
  const senderName = email.sender.split("<")[0].replace(/"/g, "").trim() || senderAddress || email.sender;
  const recipientDisplay = email.recipient
    ? email.recipient.split(",").map(r => r.split("<")[0].replace(/"/g, "").trim() || r.trim()).join(", ")
    : tr.mail.me;
  const locale = tr === locales.tr ? "tr-TR" : "en-US";
  const displayAddress = (raw: string) => raw
    .split(",")
    .map(value => {
      const addressMatch = value.match(/<\s*([^>]+)\s*>/);
      const address = (addressMatch?.[1] ?? (value.includes("@") ? value : "")).trim();
      const name = value.split("<")[0].replace(/"/g, "").trim();
      if (name && address && name.toLocaleLowerCase() !== address.toLocaleLowerCase()) return `${name} · ${address}`;
      return address || name;
    })
    .filter(Boolean)
    .join(", ");

  const updateDetailsPosition = useCallback(() => {
    const trigger = detailsTriggerRef.current;
    if (!trigger) return;
    const rect = trigger.getBoundingClientRect();
    const viewportPadding = 12;
    const desiredWidth = 620;
    const width = Math.min(desiredWidth, window.innerWidth - viewportPadding * 2);
    const left = Math.max(viewportPadding, Math.min(rect.left, window.innerWidth - width - viewportPadding));
    const estimatedHeight = 250;
    const top = Math.max(viewportPadding, Math.min(rect.bottom + 6, window.innerHeight - estimatedHeight - viewportPadding));
    setDetailsPosition({ top, left, width });
  }, []);

  useLayoutEffect(() => {
    if (!detailsOpen) return;
    updateDetailsPosition();

    const handlePointerDown = (event: PointerEvent) => {
      const target = event.target as Node;
      if (detailsTriggerRef.current?.contains(target) || detailsPopoverRef.current?.contains(target)) return;
      setDetailsOpen(false);
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setDetailsOpen(false);
    };
    document.addEventListener("pointerdown", handlePointerDown);
    document.addEventListener("keydown", handleKeyDown);
    window.addEventListener("resize", updateDetailsPosition);
    window.addEventListener("scroll", updateDetailsPosition, true);
    return () => {
      document.removeEventListener("pointerdown", handlePointerDown);
      document.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("resize", updateDetailsPosition);
      window.removeEventListener("scroll", updateDetailsPosition, true);
    };
  }, [detailsOpen, updateDetailsPosition]);

  const toggle = async () => {
    if (!collapsible) return;
    if (expanded) setDetailsOpen(false);
    if (!expanded && !isActive && lazyBody === null && !lazyLoadRequestedRef.current) {
      lazyLoadRequestedRef.current = true;
      setLazyLoading(true);
      try {
        const raw = await tauriApi.getEmailBody(email.id, email.account_id);
        setLazyBody(raw || "");
      } catch {
        setLazyBody("");
      } finally {
        setLazyLoading(false);
      }
    }
    setExpanded(e => !e);
  };

  const bodySource = isActive ? preloadedBody ?? "" : lazyBody ?? "";
  const bodyHtml = buildRenderableEmailHtml(bodySource, email.snippet, renderMode, remoteImagesAllowed);
  const loading = isActive ? (isBodyLoading ?? false) : lazyLoading;
  const loaded = isActive ? (hasLoadedBody ?? false) : lazyBody !== null;
  const error = isActive ? bodyError : null;
  const showRemoteImageNotice = loaded && !remoteImagesAllowed && hasRemoteEmailImages(bodySource);

  return (
    <div className={`rounded-xl overflow-hidden border ${isActive ? "border-white/[0.10]" : "border-white/[0.06]"}`}>
      {/* Header — always visible, click to expand/collapse */}
      <div className="flex w-full items-center gap-1 px-2 py-1 transition-colors hover:bg-white/[0.02]">
        <div onClick={() => { if (collapsible) void toggle(); }} className={`flex min-w-0 flex-1 items-center gap-3 rounded-lg px-2 py-2 text-left ${collapsible ? "cursor-pointer" : ""}`}>
          <div className="w-8 h-8 rounded-full bg-[var(--app-accent)] flex items-center justify-center text-white text-xs font-bold shrink-0">
          {(senderName[0] || "?").toUpperCase()}
          </div>
          <div className="min-w-0 flex-1">
          <div className="flex items-baseline justify-between gap-2">
            <div className="flex min-w-0 items-baseline gap-1.5">
              <span className={`truncate text-sm font-medium ${isActive ? "text-zinc-100" : "text-zinc-300"}`}>
                <SearchHighlightedText text={senderName} query={searchQuery} />
              </span>
              {senderAddress && senderAddress.toLocaleLowerCase() !== senderName.toLocaleLowerCase() && (
                <span className="truncate text-[11px] font-normal text-zinc-500"><SearchHighlightedText text={senderAddress} query={searchQuery} /></span>
              )}
            </div>
            <span className="shrink-0 whitespace-nowrap text-[11px] text-zinc-500">
              {formatDateFull(email.date, locale)} <span className="text-zinc-600">({formatRelativeTime(email.date, relativeNow, locale)})</span>
            </span>
          </div>
          {expanded ? (
            <button
              ref={detailsTriggerRef}
              type="button"
              onClick={event => {
                event.stopPropagation();
                setDetailsOpen(open => !open);
              }}
              aria-label={tr.mail.messageDetails}
              aria-expanded={detailsOpen}
              className="mt-0.5 flex max-w-full min-w-0 items-center rounded text-left text-[11px] text-zinc-600 transition-colors hover:text-zinc-300"
            >
              <span className="truncate"><span className="text-zinc-700">{tr.mail.toShort}</span> <SearchHighlightedText text={recipientDisplay} query={searchQuery} />
                {email.cc && <> · <span className="text-zinc-700">{tr.mail.ccShort}</span> {email.cc}</>}
              </span>
              <ChevronDown className={`ml-0.5 h-3 w-3 shrink-0 transition-transform ${detailsOpen ? "rotate-180" : ""}`} />
            </button>
          ) : (
            <p className="text-[11px] text-zinc-600 truncate mt-0.5"><SearchHighlightedText text={email.snippet} query={searchQuery} /></p>
          )}
          </div>
        </div>
        <ToolbarTip label={tr.mail.replyTo}>
          <button
            type="button"
            onClick={() => {
              onReply();
            }}
            className="rounded-md p-2 text-zinc-500 transition-colors hover:bg-white/[0.05] hover:text-zinc-200"
            aria-label={tr.mail.replyTo}
          >
            <CornerUpLeft className="h-4 w-4" />
          </button>
        </ToolbarTip>
        {collapsible && (
          <button type="button" onClick={toggle} className="rounded-md p-2 text-zinc-500 transition-colors hover:bg-white/[0.05] hover:text-zinc-200">
            {lazyLoading
              ? <RefreshCw className="w-3.5 h-3.5 animate-spin" />
              : expanded
                ? <ChevronUp className="w-3.5 h-3.5" />
                : <ChevronDown className="w-3.5 h-3.5" />
            }
          </button>
        )}
      </div>

      {expanded && detailsOpen && createPortal(
        <div
          ref={detailsPopoverRef}
          role="dialog"
          aria-label={tr.mail.messageDetails}
          className="fixed z-[100] overflow-y-auto rounded-lg border border-white/[0.12] bg-[var(--color-surface-popover)] p-3 shadow-2xl shadow-black/60"
          style={{
            top: detailsPosition.top,
            left: detailsPosition.left,
            width: detailsPosition.width,
            maxHeight: `calc(100vh - ${detailsPosition.top + 12}px)`,
          }}
        >
            <div className="mb-2 text-xs font-semibold text-zinc-300">{tr.mail.messageDetails}</div>
            <dl className="grid grid-cols-[max-content_minmax(0,1fr)] gap-x-3 gap-y-1.5 text-xs leading-relaxed">
              <dt className="text-right text-zinc-600">{tr.mail.fromDetails}</dt>
              <dd className="min-w-0 break-words text-zinc-300">{senderName}{senderAddress ? ` · ${senderAddress}` : ""}</dd>
              <dt className="text-right text-zinc-600">{tr.mail.toDetails}</dt>
              <dd className="min-w-0 break-words text-zinc-300">{displayAddress(email.recipient) || tr.mail.me}</dd>
              {email.cc && <><dt className="text-right text-zinc-600">{tr.mail.ccDetails}</dt><dd className="min-w-0 break-words text-zinc-300">{displayAddress(email.cc)}</dd></>}
              {email.reply_to && email.reply_to !== email.sender && <><dt className="text-right text-zinc-600">{tr.mail.replyToDetails}</dt><dd className="min-w-0 break-words text-zinc-300">{displayAddress(email.reply_to)}</dd></>}
              <dt className="text-right text-zinc-600">{tr.mail.dateDetails}</dt>
              <dd className="min-w-0 break-words text-zinc-300">{formatDateFull(email.date, locale)} ({formatRelativeTime(email.date, relativeNow, locale)})</dd>
              <dt className="text-right text-zinc-600">{tr.mail.subjectDetails}</dt>
              <dd className="min-w-0 break-words text-zinc-300">{email.subject}</dd>
            </dl>
        </div>
      , document.body)}

      {/* Body — shown when expanded */}
      {expanded && (
        <div className="border-t border-white/[0.05]">
          {loading ? (
            <div className="bg-white flex min-h-[200px] items-center justify-center text-xs text-zinc-400">
              {tr.mail.loadingBody}
            </div>
          ) : error ? (
            <div className="bg-white flex min-h-[200px] items-center justify-center text-xs text-red-400">
              {error}
            </div>
          ) : loaded ? (
            <div className="bg-white overflow-hidden">
              {showRemoteImageNotice && (
                <div className="m-4 rounded-xl border border-[var(--app-accent)]/25 bg-[var(--app-accent-soft)] p-4 text-zinc-800 shadow-sm shadow-black/5">
                  <div className="flex items-start gap-3">
                    <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border border-white/70 bg-white/70 text-[var(--app-accent)] shadow-sm">
                      <ImageOff className="h-4 w-4" />
                    </div>
                    <div className="min-w-0 flex-1">
                      <div className="text-sm font-semibold">{tr.remoteImages.blockedTitle}</div>
                      <p className="mt-1 text-xs leading-relaxed text-zinc-600">{tr.remoteImages.blockedDescription}</p>
                      <div className="mt-3 flex flex-wrap gap-2">
                        <button
                          type="button"
                          onClick={() => onLoadRemoteImages(email.id)}
                          className="rounded-lg bg-[var(--app-accent)] px-3 py-2 text-xs font-semibold text-white transition-colors hover:bg-[var(--app-accent-hover)]"
                        >
                          {tr.remoteImages.load}
                        </button>
                        <button
                          type="button"
                          onClick={() => onTrustRemoteImages(email)}
                          className="rounded-lg border border-zinc-200 bg-white/75 px-3 py-2 text-xs font-medium text-zinc-700 transition-colors hover:bg-white"
                        >
                          {tr.remoteImages.trustSender.replace("{sender}", email.sender)}
                        </button>
                      </div>
                    </div>
                  </div>
                </div>
              )}
              <EmailHtmlView
                key={email.id}
                html={bodyHtml}
                zoom={mailZoom}
                relayoutKey={relayoutKey}
                onFitScaleChange={onFitScaleChange ?? (() => {})}
                onOpenUrl={onOpenUrl}
                scrollRef={scrollRef}
                searchQuery={searchQuery}
              />
            </div>
          ) : (
            <div className="bg-white flex min-h-[200px] items-center justify-center text-xs text-zinc-400">
              {tr.mail.preparingBody}
            </div>
          )}
          {!replyEditorOpen && <div className="flex flex-wrap items-center gap-2 border-t border-white/[0.05] bg-[var(--color-surface-subtle)] px-3 py-2 sm:px-4">
            <button type="button" onClick={onReply} className="flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-xs text-zinc-400 transition-colors hover:bg-white/[0.05] hover:text-zinc-200">
              <CornerUpLeft className="h-3.5 w-3.5" /> {tr.mail.replyTo}
            </button>
            {canReplyAll && (
              <button type="button" onClick={onReplyAll} className="flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-xs text-zinc-400 transition-colors hover:bg-white/[0.05] hover:text-zinc-200">
                <Users className="h-3.5 w-3.5" /> {tr.mail.replyAll}
              </button>
            )}
            <button type="button" onClick={onForward} className="flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-xs text-zinc-400 transition-colors hover:bg-white/[0.05] hover:text-zinc-200">
              <Forward className="h-3.5 w-3.5" /> {tr.mail.forward}
            </button>
          </div>}
        </div>
      )}
    </div>
  );
}

// ── EmailReader props ──────────────────────────────────────────────────────────
interface EmailReaderProps {
  className: string;
  activeMail: EmailSummary;
  activeMailBody: string;
  isBodyLoading: boolean;
  bodyError: string | null;
  hasLoadedActiveBody: boolean;
  mailViewMode: MailViewMode;
  activeTab: string;
  closeReader: () => void;

  showReply: boolean;
  setShowReply: (v: boolean) => void;
  replyTarget: EmailSummary | null;
  setReplyTarget: React.Dispatch<React.SetStateAction<EmailSummary | null>>;
  replyMode: "reply" | "reply-all";
  setReplyMode: (v: "reply" | "reply-all") => void;
  replyText: string;
  setReplyText: (v: string) => void;
  isSending: boolean;
  onSendReply: (request: ReplySendRequest) => Promise<boolean>;

  mailZoom: MailZoom;
  setMailFitScale: (scale: number) => void;
  stepMailZoom: (dir: 1 | -1) => void;
  persistMailZoom: (zoom: MailZoom) => void;
  effectiveZoomPct: number;

  readingToolsOpen: boolean;
  setReadingToolsOpen: (v: boolean | ((prev: boolean) => boolean)) => void;
  renderMode: RenderMode;
  setRenderMode: (v: RenderMode) => void;
  remoteImagesAllowedForEmail: (email: EmailSummary) => boolean;
  onLoadRemoteImages: (emailId: string) => void;
  onTrustRemoteImages: (email: EmailSummary) => void;

  verificationCode: string | null;
  verificationCopyState: "idle" | "copied";
  setVerificationCopyState: (v: "idle" | "copied") => void;

  showArchiveBtn: boolean;
  showSpamBtn: boolean;
  showRestoreBtn: boolean;
  showTrashToBinBtn: boolean;

  onArchive: (mail: EmailSummary) => void;
  onReportSpam: (mail: EmailSummary) => void;
  onTrash: (mail: EmailSummary) => void;
  onMoveToInbox: (mail: EmailSummary) => void;
  onMarkAsUnread: (mail: EmailSummary) => void;
  onForward: (mail: EmailSummary) => void;
  onOpenUrl: (url: string) => void;
  mailScrollRef: React.RefObject<HTMLDivElement | null>;
  relayoutKey: string;
  threadEmails: EmailSummary[];
  hasMoreThreadEmails: boolean;
  isLoadingOlderThread: boolean;
  threadMemoryLimitReached: boolean;
  onLoadOlderThread: () => void;
  accessToken: string | null;
  showToast: (msg: string, kind: "success" | "error" | "info") => void;
  searchQuery: string;
}

// ── Main component ─────────────────────────────────────────────────────────────
export function EmailReader({
  className, activeMail, activeMailBody,
  isBodyLoading, bodyError, hasLoadedActiveBody,
  mailViewMode, activeTab, closeReader,
  showReply, setShowReply, replyTarget, setReplyTarget, replyMode, setReplyMode, replyText, setReplyText,
  isSending, onSendReply,
  mailZoom, setMailFitScale, stepMailZoom, persistMailZoom, effectiveZoomPct,
  readingToolsOpen, setReadingToolsOpen, renderMode, setRenderMode,
  remoteImagesAllowedForEmail, onLoadRemoteImages, onTrustRemoteImages,
  verificationCode, verificationCopyState, setVerificationCopyState,
  showArchiveBtn, showSpamBtn, showRestoreBtn, showTrashToBinBtn,
  onArchive, onReportSpam, onTrash, onMoveToInbox, onMarkAsUnread, onForward,
  onOpenUrl, mailScrollRef, relayoutKey, threadEmails, hasMoreThreadEmails, isLoadingOlderThread,
  threadMemoryLimitReached, onLoadOlderThread, accessToken, showToast,
  searchQuery,
}: EmailReaderProps) {
  const tr = useLocale();
  const replyEditableRef = useRef<HTMLDivElement>(null);
  const replyFileInputRef = useRef<HTMLInputElement>(null);
  const [replyEmpty, setReplyEmpty] = useState(true);
  const [canUndo, setCanUndo] = useState(false);
  const [canRedo, setCanRedo] = useState(false);
  const [showFormatBar, setShowFormatBar] = useState(false);
  const [linkPopover, setLinkPopover] = useState(false);
  const [linkText, setLinkText] = useState("");
  const [linkUrl, setLinkUrl] = useState("");
  const savedRangeRef = useRef<Range | null>(null);
  const replyAttachmentReadersRef = useRef<Set<FileReader>>(new Set());
  const pendingReplyAttachmentBytesRef = useRef(0);
  const replyFocusTimerRef = useRef<number | null>(null);
  const copyResetTimerRef = useRef<number | null>(null);
  const autoScrolledMailRef = useRef<string | null>(null);
  const threadScrollSnapshotRef = useRef<{
    activeMailId: string;
    firstEmailKey: string;
    scrollHeight: number;
    scrollTop: number;
  } | null>(null);
  const mountedRef = useRef(true);
  const [replyAttachments, setReplyAttachments] = useState<(AttachmentPayload & { size: number })[]>([]);
  const [replyAttachError, setReplyAttachError] = useState<string | null>(null);
  const [pendingReplyAttachmentReads, setPendingReplyAttachmentReads] = useState(0);
  const [replyPortalHost, setReplyPortalHost] = useState<HTMLDivElement | null>(null);
  const [replyTargetBody, setReplyTargetBody] = useState<string | null>(null);
  const [replyDraftId, setReplyDraftId] = useState<string | null>(null);
  const [replyVerificationMessageId, setReplyVerificationMessageId] = useState<string | null>(null);
  const [replyDraftStatus, setReplyDraftStatus] = useState<"idle" | "saving" | "saved" | "error">("idle");
  const [replyDraftError, setReplyDraftError] = useState<string | null>(null);
  const [replyDraftHydrated, setReplyDraftHydrated] = useState(false);
  const replySaveTimerRef = useRef<number | null>(null);
  const replyDraftIdRef = useRef<string | null>(null);
  const replyVerificationMessageIdRef = useRef<string | null>(null);
  const replyDraftSaveQueueRef = useRef<Promise<SavedDraft | null>>(Promise.resolve(null));
  const replyTargetKeyRef = useRef<string | null>(null);
  const dismissedReplyKeyRef = useRef<string | null>(null);
  const inlineDraftLookupRef = useRef<string | null>(null);

  const [attachments, setAttachments] = useState<AttachmentInfo[]>([]);
  const [thumbnails, setThumbnails] = useState<Record<string, string>>({});
  const [downloadingId, setDownloadingId] = useState<string | null>(null);
  const [relativeNow, setRelativeNow] = useState(() => Date.now());

  useEffect(() => {
    const timer = window.setInterval(() => setRelativeNow(Date.now()), 60_000);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      if (replyFocusTimerRef.current) clearTimeout(replyFocusTimerRef.current);
      if (copyResetTimerRef.current) clearTimeout(copyResetTimerRef.current);
      if (replySaveTimerRef.current) clearTimeout(replySaveTimerRef.current);
      for (const reader of replyAttachmentReadersRef.current) reader.abort();
      replyAttachmentReadersRef.current.clear();
      pendingReplyAttachmentBytesRef.current = 0;
    };
  }, []);

  useEffect(() => {
    if (!replyTarget) {
      replyTargetKeyRef.current = null;
      replyDraftIdRef.current = null;
      replyVerificationMessageIdRef.current = null;
      setReplyTargetBody(null);
      setReplyDraftId(null);
      setReplyVerificationMessageId(null);
      setReplyDraftStatus("idle");
      setReplyDraftError(null);
      setReplyDraftHydrated(false);
      return;
    }
    const targetKey = inlineReplyStorageKey(replyTarget);
    replyTargetKeyRef.current = targetKey;
    let cancelled = false;
    let stored: StoredInlineReplyDraft | null = null;
    try {
      stored = parseStoredInlineReplyDraft(localStorage.getItem(inlineReplyStorageKey(replyTarget)));
    } catch {
      stored = null;
    }
    setReplyText(stored?.body ?? "");
    if (stored?.mode) setReplyMode(stored.mode);
    replyDraftIdRef.current = stored?.draftId ?? null;
    replyVerificationMessageIdRef.current = stored?.verificationMessageId ?? null;
    setReplyDraftId(stored?.draftId ?? null);
    setReplyVerificationMessageId(stored?.verificationMessageId ?? null);
    setReplyDraftStatus(stored?.draftId ? "saved" : "idle");
    setReplyDraftError(null);
    setReplyAttachments([]);
    setReplyDraftHydrated(!stored?.draftId);
    void tauriApi.getEmailBody(replyTarget.id, replyTarget.account_id)
      .then(body => { if (!cancelled) setReplyTargetBody(body || replyTarget.snippet); })
      .catch(() => { if (!cancelled) setReplyTargetBody(replyTarget.snippet); });
    if (stored?.draftId) {
      void tauriApi.getDraft(replyTarget.account_id, stored.draftId)
        .then(draft => {
          if (cancelled) return;
          setReplyAttachments(draft.attachments.map(attachment => ({
            ...attachment,
            size: Math.ceil(attachment.data.length * 0.75),
          })));
        })
        .catch(() => {
          if (cancelled) return;
          replyDraftIdRef.current = null;
          replyVerificationMessageIdRef.current = null;
          setReplyDraftId(null);
          setReplyVerificationMessageId(null);
        })
        .finally(() => { if (!cancelled) setReplyDraftHydrated(true); });
    }
    return () => { cancelled = true; };
  }, [replyTarget?.account_id, replyTarget?.id]);

  useEffect(() => {
    for (const reader of replyAttachmentReadersRef.current) reader.abort();
    replyAttachmentReadersRef.current.clear();
    pendingReplyAttachmentBytesRef.current = 0;
    setPendingReplyAttachmentReads(0);
    setReplyAttachments([]);
  }, [activeMail.id]);

  const firstThreadEmailKey = threadEmails[0]
    ? `${threadEmails[0].account_id}\u0000${threadEmails[0].id}`
    : "";

  // Preserve the visible message when older thread items are prepended above it.
  useLayoutEffect(() => {
    const container = mailScrollRef.current;
    if (!container) return;
    const previous = threadScrollSnapshotRef.current;
    if (
      previous &&
      previous.activeMailId === activeMail.id &&
      previous.firstEmailKey &&
      firstThreadEmailKey &&
      previous.firstEmailKey !== firstThreadEmailKey
    ) {
      const addedHeight = container.scrollHeight - previous.scrollHeight;
      if (addedHeight > 0) container.scrollTop = previous.scrollTop + addedHeight;
    }
    return () => {
      threadScrollSnapshotRef.current = {
        activeMailId: activeMail.id,
        firstEmailKey: firstThreadEmailKey,
        scrollHeight: container.scrollHeight,
        scrollTop: container.scrollTop,
      };
    };
  }, [activeMail.id, firstThreadEmailKey, mailScrollRef]);

  // Scroll to the active card once when a thread first loads. Later thread
  // updates must not override a user's manual scroll position.
  useEffect(() => {
    if (threadEmails.length <= 1 || autoScrolledMailRef.current === activeMail.id) return;
    const timer = setTimeout(() => {
      const card = document.getElementById(`tc-${activeMail.id}`);
      const container = mailScrollRef.current;
      if (!card || !container) return;
      const cardTop = (card as HTMLElement).offsetTop;
      if (cardTop > container.clientHeight * 0.5) {
        container.scrollTop = Math.max(0, cardTop - 72);
      }
      autoScrolledMailRef.current = activeMail.id;
    }, 120);
    return () => clearTimeout(timer);
  }, [threadEmails, activeMail.id, mailScrollRef]);

  useEffect(() => {
    let cancelled = false;
    const accountId = activeMail.account_id;
    const emailId = activeMail.id;
    setAttachments([]);
    setThumbnails({});
    tauriApi.getEmailAttachments(emailId, accountId)
      .then(atts => {
        if (cancelled) return;
        setAttachments(atts);
        if (!accessToken) return;
        // Fetch thumbnails for image attachments that don't have inline data
        const imageAtts = atts.filter(a => a.mime_type.startsWith("image/") && !a.data);
        if (imageAtts.length === 0) return;
        Promise.allSettled(
          imageAtts.map(a =>
            tauriApi.fetchAttachmentData(emailId, accountId, a.id)
              .then(data => ({ id: a.id, data }))
          )
        ).then(results => {
          if (cancelled) return;
          const map: Record<string, string> = {};
          for (const r of results) {
            if (r.status === "fulfilled") map[r.value.id] = r.value.data;
          }
          if (Object.keys(map).length > 0) setThumbnails(map);
        });
      })
      .catch(error => {
        if (cancelled) return;
        console.error("Attachment thumbnails could not be loaded:", error);
      });
    return () => { cancelled = true; };
  }, [activeMail.id, activeMail.account_id, accessToken]);

  const handleDownload = async (att: AttachmentInfo) => {
    if (!accessToken) return;
    setDownloadingId(att.id);
    try {
      const saved = await tauriApi.saveAndRevealAttachment(
        activeMail.id,
        activeMail.account_id,
        att.id,
      );
      showToast(
        (saved.revealed ? tr.mail.savedToDownloads : tr.mail.savedButRevealFailed)
          .replace("{name}", saved.fileName),
        saved.revealed ? "success" : "info",
      );
    } catch (e) {
      showToast(tr.mail.downloadFailed, "error");
      console.error("Download failed:", e);
    } finally {
      setDownloadingId(null);
    }
  };

  // Clear contenteditable when reply is hidden or replyText is reset by parent
  useEffect(() => {
    if (!showReply) {
      for (const reader of replyAttachmentReadersRef.current) reader.abort();
      replyAttachmentReadersRef.current.clear();
      pendingReplyAttachmentBytesRef.current = 0;
      setPendingReplyAttachmentReads(0);
      setShowFormatBar(false);
      setLinkPopover(false);
      setReplyEmpty(true);
      setReplyAttachments([]);
      setReplyAttachError(null);
      if (replyEditableRef.current) replyEditableRef.current.innerHTML = "";
    }
  }, [showReply]);

  useEffect(() => {
    if (!replyEditableRef.current || replyEditableRef.current.innerHTML === replyText) return;
    replyEditableRef.current.innerHTML = replyText;
    setReplyEmpty(!replyEditableRef.current.innerText.trim());
  }, [replyText]);

  const syncUndoRedo = () => {
    setCanUndo(document.queryCommandEnabled("undo"));
    setCanRedo(document.queryCommandEnabled("redo"));
  };

  const applyFormat = (command: string, value?: string) => {
    replyEditableRef.current?.focus();
    document.execCommand(command, false, value);
    setReplyEmpty(!(replyEditableRef.current?.innerText.trim()));
    setReplyText(replyEditableRef.current?.innerHTML ?? "");
    syncUndoRedo();
  };

  const saveSelection = () => {
    const sel = window.getSelection();
    if (sel && sel.rangeCount > 0) {
      savedRangeRef.current = sel.getRangeAt(0).cloneRange();
      setLinkText(sel.toString());
    }
  };

  const restoreSelection = () => {
    const sel = window.getSelection();
    if (sel && savedRangeRef.current) {
      sel.removeAllRanges();
      sel.addRange(savedRangeRef.current);
    }
  };

  const applyLink = () => {
    const safeUrl = normalizeComposerLinkUrl(linkUrl);
    if (!safeUrl) return;
    restoreSelection();
    replyEditableRef.current?.focus();
    if (linkText && !window.getSelection()?.toString()) {
      const link = document.createElement("a");
      link.href = safeUrl;
      link.textContent = linkText;
      const selection = window.getSelection();
      const range = selection?.rangeCount ? selection.getRangeAt(0) : null;
      if (range && replyEditableRef.current?.contains(range.commonAncestorContainer)) {
        range.deleteContents();
        range.insertNode(link);
        range.setStartAfter(link);
        range.collapse(true);
        selection?.removeAllRanges();
        selection?.addRange(range);
      } else {
        replyEditableRef.current?.append(link);
      }
    } else {
      document.execCommand("createLink", false, safeUrl);
    }
    setReplyEmpty(!(replyEditableRef.current?.innerText.trim()));
    setLinkPopover(false);
    setLinkText("");
    setLinkUrl("");
  };

  const BLOCKED_EXT = new Set(["exe","bat","cmd","com","msi","scr","pif","vbs","vbe","js","jse","jar","wsf","wsh","ps1","reg","inf","lnk"]);
  const MAX_ATT_BYTES = 20 * 1024 * 1024;
  const MAX_ATT_FILES = 100;

  const addReplyAttachmentFiles = (files: File[]) => {
    if (!files.length) return;
    setReplyAttachError(null);
    if (replyAttachments.length + replyAttachmentReadersRef.current.size + files.length > MAX_ATT_FILES) {
      setReplyAttachError(tr.compose.attachmentCountLimit);
      return;
    }
    const blocked = files.filter(f => BLOCKED_EXT.has(f.name.split(".").pop()?.toLowerCase() ?? ""));
    if (blocked.length) {
      setReplyAttachError(tr.compose.blockedFileType.replace("{files}", blocked.map(f => f.name).join(", ")));
      return;
    }
    const existingBytes = replyAttachments.reduce((s, a) => s + a.size, 0);
    const newBytes = files.reduce((s, f) => s + f.size, 0);
    if (existingBytes + pendingReplyAttachmentBytesRef.current + newBytes > MAX_ATT_BYTES) {
      setReplyAttachError(tr.compose.attachmentTooLarge);
      return;
    }
    pendingReplyAttachmentBytesRef.current += newBytes;
    setPendingReplyAttachmentReads(prev => prev + files.length);
    files.forEach(file => {
      const reader = new FileReader();
      replyAttachmentReadersRef.current.add(reader);
      let settled = false;
      const finishRead = () => {
        if (settled) return;
        settled = true;
        replyAttachmentReadersRef.current.delete(reader);
        pendingReplyAttachmentBytesRef.current = Math.max(0, pendingReplyAttachmentBytesRef.current - file.size);
        if (mountedRef.current) setPendingReplyAttachmentReads(prev => Math.max(0, prev - 1));
      };
      reader.onload = () => {
        try {
          if (!mountedRef.current) return;
          if (typeof reader.result !== "string" || !reader.result.includes(",")) {
            throw new Error("Invalid FileReader result");
          }
          const base64 = reader.result.split(",", 2)[1];
          setReplyAttachments(prev => [...prev, { filename: file.name, mimeType: file.type || "application/octet-stream", data: base64, size: file.size }]);
        } catch {
          if (mountedRef.current) setReplyAttachError(`${tr.compose.attachmentReadFailed}: ${file.name}`);
        } finally {
          finishRead();
        }
      };
      reader.onerror = () => {
        if (mountedRef.current) setReplyAttachError(`${tr.compose.attachmentReadFailed}: ${file.name}`);
        finishRead();
      };
      reader.onabort = finishRead;
      try {
        reader.readAsDataURL(file);
      } catch {
        if (mountedRef.current) setReplyAttachError(`${tr.compose.attachmentReadFailed}: ${file.name}`);
        finishRead();
      }
    });
  };

  const handleReplyFileSelect = (e: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(e.target.files ?? []);
    e.target.value = "";
    addReplyAttachmentFiles(files);
  };

  const handleReplyPaste = (e: React.ClipboardEvent<HTMLDivElement>) => {
    const imageFiles = Array.from(e.clipboardData.items)
      .filter(item => item.kind === "file" && item.type.startsWith("image/"))
      .map(item => item.getAsFile())
      .filter((file): file is File => file !== null);
    if (imageFiles.length > 0) {
      e.preventDefault();
      addReplyAttachmentFiles(imageFiles);
      return;
    }

    const plainText = e.clipboardData.getData("text/plain");
    const htmlText = e.clipboardData.getData("text/html");
    if (!plainText && !htmlText) return;
    e.preventDefault();
    const safeText = plainText || new DOMParser().parseFromString(htmlText, "text/html").body.textContent || "";
    document.execCommand("insertText", false, safeText);
  };

  const replyRecipientSet = replyTarget
    ? (replyMode === "reply-all"
        ? calculateReplyAllRecipients(replyTarget)
        : calculateReplyRecipients(replyTarget))
    : { to: [], cc: [] };

  const buildReplyDraftBody = useCallback((body: string) => {
    if (!replyTarget) return body;
    return buildReplyBody(replyTarget, body, replyTargetBody || replyTarget.snippet, tr.compose.wroteOn);
  }, [replyTarget, replyTargetBody, tr.compose.wroteOn]);

  const persistReplyDraft = useCallback((): Promise<SavedDraft | null> => {
    if (!replyTarget || replyTargetBody === null || !replyDraftHydrated) return Promise.resolve(null);
    const body = replyEditableRef.current?.innerHTML ?? replyText;
    const hasContent = Boolean(body.replace(/<[^>]*>/g, "").trim() || replyAttachments.length > 0);
    if (!hasContent && !replyDraftIdRef.current) return Promise.resolve(null);
    const storageKey = inlineReplyStorageKey(replyTarget);
    const localSnapshot: StoredInlineReplyDraft = {
      body,
      mode: replyMode,
      draftId: replyDraftIdRef.current,
      verificationMessageId: replyVerificationMessageIdRef.current,
    };
    try { localStorage.setItem(storageKey, JSON.stringify(localSnapshot)); } catch { /* Gmail save remains authoritative. */ }
    setReplyDraftStatus("saving");
    setReplyDraftError(null);
    const saveOperation = replyDraftSaveQueueRef.current
      .catch(() => null)
      .then(async () => {
        const saved = await tauriApi.saveDraft({
        accountId: replyTarget.account_id,
        draftId: replyDraftIdRef.current,
        to: replyRecipientSet.to.join(", "),
        cc: replyRecipientSet.cc.join(", "),
        bcc: "",
        subject: `Re: ${replyTarget.subject.replace(/^(Re:\s*)+/i, "")}`,
        body: buildReplyDraftBody(body),
        attachments: replyAttachments.length > 0
          ? replyAttachments.map(({ filename, mimeType, data }) => ({ filename, mimeType, data }))
          : null,
        threadId: replyTarget.thread_id || replyTarget.id,
        inReplyTo: replyTarget.message_id || null,
        references: replyTarget.references || null,
      });
        if (!mountedRef.current || replyTargetKeyRef.current !== storageKey) return saved;
        replyDraftIdRef.current = saved.id;
        replyVerificationMessageIdRef.current = saved.verificationMessageId;
        setReplyDraftId(saved.id);
        setReplyVerificationMessageId(saved.verificationMessageId);
        setReplyDraftStatus("saved");
        try {
          localStorage.setItem(storageKey, JSON.stringify({
            ...localSnapshot,
            draftId: saved.id,
            verificationMessageId: saved.verificationMessageId,
          } satisfies StoredInlineReplyDraft));
        } catch { /* The Gmail draft was still saved successfully. */ }
        return saved;
      })
      .catch(error => {
        if (mountedRef.current && replyTargetKeyRef.current === storageKey) {
          setReplyDraftStatus("error");
          setReplyDraftError(String(error).replace(/^Error:\s*/i, ""));
        }
        throw error;
      });
    replyDraftSaveQueueRef.current = saveOperation;
    return saveOperation;
  }, [buildReplyDraftBody, replyAttachments, replyDraftHydrated, replyDraftId, replyMode, replyRecipientSet.cc.join("\u0000"), replyRecipientSet.to.join("\u0000"), replyTarget, replyTargetBody, replyText, replyVerificationMessageId]);

  useEffect(() => {
    if (!showReply || !replyTarget) return;
    const storageKey = inlineReplyStorageKey(replyTarget);
    try {
      localStorage.setItem(storageKey, JSON.stringify({
        body: replyText,
        mode: replyMode,
        draftId: replyDraftId,
        verificationMessageId: replyVerificationMessageId,
      } satisfies StoredInlineReplyDraft));
    } catch { /* Autosave continues through Gmail. */ }
    if (replyTargetBody === null || !replyDraftHydrated || (!replyText.trim() && replyAttachments.length === 0 && !replyDraftId)) return;
    if (replySaveTimerRef.current) clearTimeout(replySaveTimerRef.current);
    replySaveTimerRef.current = window.setTimeout(() => {
      void persistReplyDraft().catch(() => undefined);
    }, 900);
    return () => {
      if (replySaveTimerRef.current) clearTimeout(replySaveTimerRef.current);
    };
  }, [showReply, replyTarget, replyText, replyMode, replyAttachments, replyTargetBody, replyDraftHydrated]);

  const sendInlineReply = async () => {
    if (!replyTarget || !areValidRecipients([...replyRecipientSet.to, ...replyRecipientSet.cc])) {
      setReplyDraftError(tr.messages.replySendFailed);
      return;
    }
    if (replySaveTimerRef.current) {
      clearTimeout(replySaveTimerRef.current);
      replySaveTimerRef.current = null;
    }
    let saved;
    try {
      saved = await persistReplyDraft();
    } catch {
      return;
    }
    const body = replyEditableRef.current?.innerHTML ?? replyText;
    const sent = await onSendReply({
      target: replyTarget,
      to: replyRecipientSet.to,
      cc: replyRecipientSet.cc,
      body: buildReplyDraftBody(body),
      attachments: replyAttachments.map(({ filename, mimeType, data }) => ({ filename, mimeType, data })),
      draftId: saved?.id ?? replyDraftId,
      verificationMessageId: saved?.verificationMessageId ?? replyVerificationMessageId,
    });
    if (sent) {
      localStorage.removeItem(inlineReplyStorageKey(replyTarget));
      setReplyText("");
      setReplyTarget(null);
    }
  };

  const deleteInlineReplyDraft = async () => {
    if (!replyTarget) return;
    try {
      if (replySaveTimerRef.current) {
        clearTimeout(replySaveTimerRef.current);
        replySaveTimerRef.current = null;
      }
      await replyDraftSaveQueueRef.current.catch(() => null);
      if (replyDraftIdRef.current) await tauriApi.deleteDraft(replyTarget.account_id, replyDraftIdRef.current);
      localStorage.removeItem(inlineReplyStorageKey(replyTarget));
      setReplyText("");
      setReplyTarget(null);
      setShowReply(false);
    } catch (error) {
      setReplyDraftStatus("error");
      setReplyDraftError(String(error).replace(/^Error:\s*/i, ""));
    }
  };

  const closeReaderWithDraft = () => {
    if (!replyTarget || (!replyText.trim() && replyAttachments.length === 0 && !replyDraftId)) {
      closeReader();
      return;
    }
    void persistReplyDraft().catch(() => undefined).finally(closeReader);
  };

  // All emails to render: full thread if available, otherwise just activeMail
  const allEmails = threadEmails.length > 0 ? threadEmails : [activeMail];
  const threadMessageKey = allEmails
    .map(email => `${email.id}:${email.message_id}`)
    .join("\u0000");

  useEffect(() => {
    dismissedReplyKeyRef.current = null;
    inlineDraftLookupRef.current = null;
  }, [activeMail.account_id, activeMail.thread_id]);

  useEffect(() => {
    if (showReply || replyTarget) return;
    const savedTarget = [...allEmails].reverse().find(email => {
      const key = inlineReplyStorageKey(email);
      if (dismissedReplyKeyRef.current === key) return false;
      try {
        const stored = parseStoredInlineReplyDraft(localStorage.getItem(key));
        return Boolean(stored && (stored.body.trim() || stored.draftId));
      } catch {
        return false;
      }
    });
    if (savedTarget) {
      const stored = parseStoredInlineReplyDraft(localStorage.getItem(inlineReplyStorageKey(savedTarget)));
      if (!stored) return;
      setReplyTarget(savedTarget);
      setReplyMode(stored.mode ?? "reply");
      setShowReply(true);
      return;
    }

    const threadId = activeMail.thread_id || activeMail.id;
    const lookupKey = `${activeMail.account_id}:${threadId}:${threadMessageKey}`;
    if (inlineDraftLookupRef.current === lookupKey) return;
    inlineDraftLookupRef.current = lookupKey;
    let cancelled = false;
    void (async () => {
      let pageToken: string | null = null;
      for (let pageNumber = 0; pageNumber < 10; pageNumber += 1) {
        const page = await tauriApi.listDrafts(activeMail.account_id, pageToken);
        const candidate = page.drafts.find(draft => {
          if (draft.threadId !== threadId || !draft.inReplyTo) return false;
          const replyId = draft.inReplyTo.trim().toLowerCase();
          return allEmails.some(email => email.message_id.trim().toLowerCase() === replyId);
        });
        if (candidate) {
          const target = allEmails.find(email =>
            email.message_id.trim().toLowerCase() === candidate.inReplyTo.trim().toLowerCase());
          if (!target || cancelled) return;
          const content = await tauriApi.getDraft(activeMail.account_id, candidate.id);
          if (cancelled) return;
          const stored: StoredInlineReplyDraft = {
            body: extractInlineReplyBody(content.body),
            mode: content.cc.trim() ? "reply-all" : "reply",
            draftId: content.id,
            verificationMessageId: content.rfcMessageId || candidate.rfcMessageId || null,
          };
          localStorage.setItem(inlineReplyStorageKey(target), JSON.stringify(stored));
          setReplyTarget(target);
          setReplyMode(stored.mode);
          setShowReply(true);
          return;
        }
        pageToken = page.nextPageToken;
        if (!pageToken) return;
      }
    })().catch(() => {
      if (!cancelled) inlineDraftLookupRef.current = null;
    });
    return () => { cancelled = true; };
  }, [activeMail.account_id, activeMail.id, activeMail.thread_id, replyTarget, showReply, threadMessageKey]);

  const openReply = (email: EmailSummary, mode: "reply" | "reply-all") => {
    const activate = (target: EmailSummary) => {
      dismissedReplyKeyRef.current = null;
      setReplyTarget(target);
      setReplyMode(mode);
      setShowReply(true);
      if (replyFocusTimerRef.current) clearTimeout(replyFocusTimerRef.current);
      replyFocusTimerRef.current = window.setTimeout(() => replyEditableRef.current?.focus(), 100);
    };
    const activateAndRefresh = () => {
      activate(email);
      void tauriApi.refreshEmailFromGmail(email.account_id, email.id)
        .then(refreshed => {
          setReplyTarget(current => current && current.id === email.id && current.account_id === email.account_id
            ? refreshed
            : current);
        })
        .catch(() => undefined);
    };
    if (replyTarget && (replyTarget.id !== email.id || replyTarget.account_id !== email.account_id)) {
      void persistReplyDraft().catch(() => undefined).finally(activateAndRefresh);
    } else activateAndRefresh();
  };
  const captureReplyHost = useCallback((node: HTMLDivElement | null) => setReplyPortalHost(node), []);

  return (
    <main className={className}>
      {/* Mobile Back Button */}
      <div className="md:hidden h-12 flex items-center px-4 border-b border-white/5 shrink-0">
        <button
          onClick={closeReaderWithDraft}
          className="flex items-center gap-2 text-xs text-zinc-400 hover:text-zinc-200"
        >
          <CornerUpLeft className="w-3.5 h-3.5" /> Back
        </button>
      </div>

      {/* Desktop Toolbar */}
      <div className="hidden md:flex h-12 items-center justify-between gap-2 px-3 lg:px-5 border-b border-white/5 shrink-0">
        <div className="flex min-w-0 items-center gap-1.5">
          {mailViewMode !== "split" && (
            <button
              type="button"
              onClick={closeReaderWithDraft}
              aria-label={tr.common.close}
              className="mr-1 shrink-0 rounded-md p-1.5 text-zinc-500 transition-colors hover:bg-white/5 hover:text-zinc-200"
            >
              <CornerUpLeft className="h-3.5 w-3.5" />
            </button>
          )}
          <span className="min-w-0 truncate text-xs text-zinc-500 flex items-center gap-1.5 capitalize">
            {activeTab === "inbox" && <Inbox className="w-3.5 h-3.5 shrink-0" />}
            {activeTab === "sent" && <Send className="w-3.5 h-3.5 shrink-0" />}
            {activeTab === "archive" && <Archive className="w-3.5 h-3.5 shrink-0" />}
            {activeTab === "spam" && <ShieldAlert className="w-3.5 h-3.5 shrink-0" />}
            {activeTab === "trash" && <Trash2 className="w-3.5 h-3.5 shrink-0" />}
            <span className="truncate">{activeMail.subject}</span>
          </span>
        </div>
        <div className="flex shrink-0 items-center gap-0.5" style={{ WebkitAppRegion: "no-drag" } as CSSProperties}>
          <ToolbarTip label={tr.mail.markAsUnread}>
            <button type="button" onClick={() => onMarkAsUnread(activeMail)} className="p-2 rounded-md hover:bg-white/5 text-zinc-400 hover:text-zinc-200 transition-colors">
              <Eye className="w-4 h-4" />
            </button>
          </ToolbarTip>
          {showRestoreBtn && (
            <ToolbarTip label={activeTab === "spam" ? tr.actions.notSpam : tr.actions.restoreInbox}>
              <button type="button" onClick={() => onMoveToInbox(activeMail)} className="p-2 rounded-md hover:bg-white/5 text-zinc-400 hover:text-emerald-400 transition-colors">
                <RotateCcw className="w-4 h-4" />
              </button>
            </ToolbarTip>
          )}
          {showArchiveBtn && (
            <ToolbarTip label={tr.actions.archive}>
              <button type="button" onClick={() => onArchive(activeMail)} className="p-2 rounded-md hover:bg-white/5 text-zinc-400 hover:text-amber-400 transition-colors">
                <Archive className="w-4 h-4" />
              </button>
            </ToolbarTip>
          )}
          {showSpamBtn && (
            <ToolbarTip label={tr.actions.reportSpam}>
              <button type="button" onClick={() => onReportSpam(activeMail)} className="p-2 rounded-md hover:bg-white/5 text-zinc-400 hover:text-orange-400 transition-colors">
                <ShieldAlert className="w-4 h-4" />
              </button>
            </ToolbarTip>
          )}
          {showTrashToBinBtn && (
            <ToolbarTip label={tr.actions.moveTrash}>
              <button type="button" onClick={() => onTrash(activeMail)} className="p-2 rounded-md hover:bg-white/5 text-zinc-400 hover:text-red-400 transition-colors">
                <Trash2 className="w-4 h-4" />
              </button>
            </ToolbarTip>
          )}
          <div className="mx-1 hidden h-5 w-px bg-white/5 lg:block" />
          <div className="hidden items-center rounded-md border border-white/10 bg-white/[0.03] lg:flex">
            <ToolbarTip label={tr.reading.zoomOut}>
              <button type="button" onClick={() => stepMailZoom(-1)} className="flex h-7 w-7 items-center justify-center rounded-l-md text-zinc-400 hover:bg-white/5 hover:text-zinc-100">
                <Minus className="h-3.5 w-3.5" />
              </button>
            </ToolbarTip>
            <ToolbarTip label={tr.reading.fitWidthHint}>
              <button
                type="button"
                onClick={() => persistMailZoom("fit")}
                aria-pressed={mailZoom === "fit"}
                className={`flex h-7 min-w-[3.25rem] items-center justify-center gap-1 px-1 text-[11px] font-medium tabular-nums transition-colors ${
                  mailZoom === "fit" ? "text-[var(--app-accent)]" : "text-zinc-300 hover:text-zinc-100"
                }`}
              >
                {mailZoom === "fit" && <Maximize2 className="h-3 w-3" />}
                {effectiveZoomPct}%
              </button>
            </ToolbarTip>
            <ToolbarTip label={tr.reading.zoomIn}>
              <button type="button" onClick={() => stepMailZoom(1)} className="flex h-7 w-7 items-center justify-center rounded-r-md text-zinc-400 hover:bg-white/5 hover:text-zinc-100">
                <Plus className="h-3.5 w-3.5" />
              </button>
            </ToolbarTip>
          </div>
          <ToolbarTip label={tr.reading.settings}>
            <button
              type="button"
              onClick={() => setReadingToolsOpen(open => !open)}
              className={`p-2 rounded-md transition-colors ${
                readingToolsOpen ? "bg-[var(--app-accent-soft)] text-zinc-100" : "text-zinc-400 hover:bg-white/5 hover:text-zinc-200"
              }`}
            >
              <Settings className="w-4 h-4" />
            </button>
          </ToolbarTip>
        </div>
      </div>

      {/* Reading Tools Panel */}
      <aside
        className={`absolute bottom-0 right-0 top-12 z-20 hidden w-72 border-l border-[var(--color-border-default)] bg-[var(--color-surface-sidebar)] p-4 shadow-2xl shadow-black/40 transition-transform duration-200 md:block ${
          readingToolsOpen ? "translate-x-0" : "translate-x-full"
        }`}
        aria-hidden={!readingToolsOpen}
      >
        <div className="mb-4 flex items-center justify-between">
          <h3 className="text-sm font-semibold text-zinc-200">{tr.reading.settings}</h3>
          <button type="button" onClick={() => setReadingToolsOpen(false)} className="rounded-md p-1 text-zinc-500 hover:bg-white/10 hover:text-zinc-200">
            <X className="h-4 w-4" />
          </button>
        </div>
        <div className="space-y-5">
          <div>
            <div className="mb-1 text-sm text-zinc-300">{tr.reading.zoom}</div>
            <p className="mb-2 text-[11px] leading-relaxed text-zinc-600">{tr.reading.zoomHint}</p>
            <div className="flex items-center gap-2">
              <div className="inline-flex items-center rounded-[var(--radius-md)] border border-[var(--color-border-default)] bg-[var(--color-surface-app)]">
                <button type="button" onClick={() => stepMailZoom(-1)} className="flex h-8 w-8 items-center justify-center text-zinc-400 hover:text-zinc-100">
                  <Minus className="h-3.5 w-3.5" />
                </button>
                <span className="min-w-[3rem] text-center text-xs font-medium text-zinc-200 tabular-nums">{effectiveZoomPct}%</span>
                <button type="button" onClick={() => stepMailZoom(1)} className="flex h-8 w-8 items-center justify-center text-zinc-400 hover:text-zinc-100">
                  <Plus className="h-3.5 w-3.5" />
                </button>
              </div>
              <button
                type="button"
                onClick={() => persistMailZoom("fit")}
                aria-pressed={mailZoom === "fit"}
                className={`flex items-center gap-1.5 rounded-lg border px-3 py-1.5 text-xs transition-colors ${
                  mailZoom === "fit" ? "border-[var(--app-accent)] bg-[var(--app-accent-soft)] text-zinc-100" : "border-[var(--color-border-default)] bg-[var(--color-surface-app)] text-zinc-400 hover:text-zinc-200"
                }`}
              >
                <Maximize2 className="h-3.5 w-3.5" />
                {tr.reading.fitWidth}
              </button>
            </div>
          </div>
          <div>
            <div className="mb-2 text-xs font-medium text-zinc-300">{tr.reading.renderMode}</div>
            <div className="inline-flex rounded-[var(--radius-md)] border border-[var(--color-border-default)] bg-[var(--color-surface-app)] p-1">
              {(["full", "simple"] as const).map(mode => (
                <button
                  key={mode}
                  type="button"
                  onClick={() => { setRenderMode(mode); localStorage.setItem("fursoy_render_mode", mode); }}
                  className={`px-3 py-1.5 text-xs rounded-md transition-colors ${renderMode === mode ? "bg-white/10 text-zinc-100" : "text-zinc-500 hover:text-zinc-300"}`}
                >
                  {mode === "full" ? tr.settings.fullHtml : tr.settings.simpleHtml}
                </button>
              ))}
            </div>
          </div>
        </div>
      </aside>

      {/* Scrollable Content */}
      <div ref={mailScrollRef} className="flex-1 overflow-y-scroll overscroll-contain p-6 md:p-8">
        <div className="mx-auto w-full max-w-[1040px] min-w-0">

          {/* Subject heading */}
          <h1 className="text-xl font-bold text-zinc-100 mb-5 leading-snug"><SearchHighlightedText text={activeMail.subject} query={searchQuery} /></h1>

          {/* Received email attachments */}
          {attachments.length > 0 && (
            <div className="mb-4 flex flex-wrap gap-2">
              {attachments.map(att => {
                const isImage = att.mime_type.startsWith("image/");
                const thumbData = att.data ?? thumbnails[att.id] ?? null;
                const hasThumb = isImage && thumbData;
                return (
                  <button
                    key={att.id}
                    type="button"
                    onClick={() => handleDownload(att)}
                    disabled={downloadingId === att.id}
                    className="flex flex-col rounded-lg bg-white/[0.04] border border-white/[0.08] hover:bg-white/[0.07] hover:border-white/[0.14] transition-colors text-left disabled:opacity-50 overflow-hidden"
                    style={{ maxWidth: 200 }}
                  >
                    {hasThumb && (
                      <img
                        src={`data:${att.mime_type};base64,${thumbData}`}
                        alt=""
                        className="w-full object-cover"
                        style={{ maxHeight: 160 }}
                      />
                    )}
                    <div className="flex items-center gap-2 px-3 py-2">
                      <span className="text-zinc-400 shrink-0">
                        <AttachmentIcon mimeType={att.mime_type} />
                      </span>
                      <div className="min-w-0 flex-1">
                        <div className="text-xs text-zinc-300 truncate">{att.filename}</div>
                        <div className="text-[10px] text-zinc-600">{formatBytes(att.size)}</div>
                      </div>
                      {downloadingId === att.id
                        ? <RefreshCw className="w-3.5 h-3.5 text-zinc-500 animate-spin shrink-0" />
                        : <Download className="w-3.5 h-3.5 text-zinc-600 shrink-0" />
                      }
                    </div>
                  </button>
                );
              })}
            </div>
          )}

          {/* OTP Banner */}
          {verificationCode && allEmails.length === 1 && (
            <div className="mb-5 flex items-center justify-between px-4 py-3 rounded-lg bg-blue-500/10 border border-blue-500/20">
              <div className="flex items-center gap-3">
                <div className="w-8 h-8 rounded-lg bg-blue-500/20 flex items-center justify-center">
                  <ShieldAlert className="w-4 h-4 text-blue-400" />
                </div>
                <div>
                  <div className="text-[11px] text-blue-400/70 font-medium">{tr.mail.verificationCode}</div>
                  <div className="text-lg font-bold text-blue-300 tracking-[0.3em] font-mono">{verificationCode}</div>
                </div>
              </div>
              <button
                type="button"
                onClick={() => {
                  void navigator.clipboard.writeText(verificationCode);
                  setVerificationCopyState("copied");
                  if (copyResetTimerRef.current) clearTimeout(copyResetTimerRef.current);
                  copyResetTimerRef.current = window.setTimeout(() => {
                    if (mountedRef.current) setVerificationCopyState("idle");
                  }, 2000);
                }}
                className="min-w-[7.5rem] justify-center px-4 py-2 rounded-lg bg-blue-500 hover:bg-blue-400 text-white text-xs font-semibold transition-colors flex items-center gap-2"
              >
                <Copy className="w-3.5 h-3.5" />
                {verificationCopyState === "copied" ? tr.common.copied : tr.common.copy}
              </button>
            </div>
          )}

          {/* Thread stack — all emails chronologically, latest (activeMail) expanded */}
          <div className="space-y-2">
            {threadMemoryLimitReached ? (
              <div className="py-2 text-center text-xs text-zinc-600">{tr.mail.threadMemoryLimit}</div>
            ) : hasMoreThreadEmails ? (
              <div className="flex justify-center py-2">
                <button
                  type="button"
                  onClick={onLoadOlderThread}
                  disabled={isLoadingOlderThread}
                  className="rounded-md px-3 py-1.5 text-xs text-zinc-500 transition-colors hover:bg-white/5 hover:text-zinc-300 disabled:cursor-wait disabled:opacity-60"
                >
                  {isLoadingOlderThread ? tr.mail.loadingOlderThread : tr.mail.loadOlderThread}
                </button>
              </div>
            ) : null}
            {allEmails.map((email) => {
              const isActive = email.id === activeMail.id;
              const isReplyTarget = replyTarget?.id === email.id && replyTarget.account_id === email.account_id;
              const canReplyAll = calculateReplyRecipients(email).canReplyAll;
              return (
                <div key={email.id} id={`tc-${email.id}`}>
                  <ThreadCard
                    email={email}
                    isActive={isActive}
                    preloadedBody={isActive ? activeMailBody : undefined}
                    isBodyLoading={isActive ? isBodyLoading : undefined}
                    hasLoadedBody={isActive ? hasLoadedActiveBody : undefined}
                    bodyError={isActive ? bodyError : undefined}
                    defaultExpanded={isActive}
                    renderMode={renderMode}
                    mailZoom={mailZoom}
                    relayoutKey={isActive ? relayoutKey : undefined}
                    onFitScaleChange={isActive ? setMailFitScale : undefined}
                    onOpenUrl={onOpenUrl}
                    remoteImagesAllowed={remoteImagesAllowedForEmail(email)}
                    onLoadRemoteImages={onLoadRemoteImages}
                    onTrustRemoteImages={onTrustRemoteImages}
                    scrollRef={mailScrollRef as React.RefObject<HTMLElement | null>}
                    onReply={() => openReply(email, "reply")}
                    onReplyAll={() => openReply(email, "reply-all")}
                    onForward={() => onForward(email)}
                    canReplyAll={canReplyAll}
                    replyEditorOpen={isReplyTarget && showReply}
                    relativeNow={relativeNow}
                    collapsible={allEmails.length > 1}
                    searchQuery={searchQuery}
                  />
                  {isReplyTarget && <div ref={captureReplyHost} className="mt-2" />}
                </div>
              );
            })}
          </div>

          {/* Mobile action buttons */}
          <div className="flex md:hidden items-center gap-1 mt-4">
            {showRestoreBtn && (
              <ToolbarTip label={activeTab === "spam" ? tr.actions.notSpam : tr.mail.inbox}>
                <button type="button" onClick={() => onMoveToInbox(activeMail)} className="p-2 rounded-md hover:bg-white/5 text-zinc-400">
                  <RotateCcw className="w-4 h-4" />
                </button>
              </ToolbarTip>
            )}
            {showArchiveBtn && (
              <ToolbarTip label={tr.actions.archive}>
                <button type="button" onClick={() => onArchive(activeMail)} className="p-2 rounded-md hover:bg-white/5 text-zinc-400">
                  <Archive className="w-4 h-4" />
                </button>
              </ToolbarTip>
            )}
            {showSpamBtn && (
              <ToolbarTip label={tr.actions.reportSpam}>
                <button type="button" onClick={() => onReportSpam(activeMail)} className="p-2 rounded-md hover:bg-white/5 text-zinc-400 hover:text-orange-400">
                  <ShieldAlert className="w-4 h-4" />
                </button>
              </ToolbarTip>
            )}
            {showTrashToBinBtn && (
              <ToolbarTip label={tr.actions.moveTrash}>
                <button type="button" onClick={() => onTrash(activeMail)} className="p-2 rounded-md hover:bg-white/5 text-zinc-400">
                  <Trash2 className="w-4 h-4" />
                </button>
              </ToolbarTip>
            )}
          </div>

          {/* Reply Box */}
          {showReply && replyTarget && replyPortalHost && createPortal((
            <div className="mt-4 rounded-xl border border-white/10 bg-white/[0.02] overflow-hidden">
              {/* To: header */}
              <div className="px-4 py-2.5 border-b border-white/5 flex items-center gap-2">
                {replyMode === "reply-all"
                  ? <Users className="w-3.5 h-3.5 text-zinc-500" />
                  : <CornerUpLeft className="w-3.5 h-3.5 text-zinc-500" />
                }
                <span className="text-xs text-zinc-400 truncate">
                  {replyMode === "reply-all" ? (
                    <>{tr.mail.replyAllPrefix} <span className="text-zinc-300">{[...replyRecipientSet.to, ...replyRecipientSet.cc].join(", ")}</span></>
                  ) : (
                    <>{tr.mail.replyTo} <span className="text-zinc-300">{replyRecipientSet.to.join(", ")}</span></>
                  )}
                </span>
                {replyDraftStatus !== "idle" && (
                  <span className={`ml-auto shrink-0 text-[10px] ${replyDraftStatus === "error" ? "text-red-400" : "text-zinc-600"}`}>
                    {replyDraftStatus === "saving" ? tr.compose.savingDraft : replyDraftStatus === "saved" ? tr.compose.draftSaved : tr.compose.draftSaveFailed}
                  </span>
                )}
              </div>

              {/* Editable area */}
              <div className="relative px-4 pt-4 pb-3 min-h-[120px]">
                {replyEmpty && (
                  <span className="absolute top-4 left-4 pointer-events-none text-zinc-600 text-sm select-none">
                    {tr.mail.writeReply}
                  </span>
                )}
                <div
                  ref={replyEditableRef}
                  contentEditable
                  role="textbox"
                  aria-multiline="true"
                  aria-label={tr.mail.writeReply}
                  suppressContentEditableWarning
                  onPaste={handleReplyPaste}
                  onInput={() => {
                    setReplyEmpty(!(replyEditableRef.current?.innerText.trim()));
                    setReplyText(replyEditableRef.current?.innerHTML ?? "");
                    syncUndoRedo();
                  }}
                  className="outline-none text-sm text-zinc-200 min-h-[96px] [&_a]:text-blue-400 [&_a]:underline [&_b]:font-bold [&_strong]:font-bold [&_i]:italic [&_em]:italic [&_u]:underline [&_s]:line-through [&_ol]:list-decimal [&_ol]:pl-5 [&_ul]:list-disc [&_ul]:pl-5"
                  style={{ wordBreak: "break-word" }}
                />
              </div>

              {/* Quote attribution */}
              <div className="px-4 pb-2 text-[11px] text-zinc-700 italic truncate border-t border-white/[0.03] pt-2">
                — {replyTarget.sender.split("<")[0].replace(/"/g, "").trim()}, {formatDateFull(replyTarget.date)}
              </div>

              {/* Formatting toolbar — visible when showFormatBar */}
              {showFormatBar && (
                <div className="relative px-3 py-1.5 border-t border-white/[0.06] flex items-center gap-0.5">
                  {/* Link popover */}
                  {linkPopover && (
                    <div className="absolute bottom-full left-0 mb-1 bg-[var(--color-surface-popover)] border border-[var(--color-border-default)] rounded-[var(--radius-lg)] p-3 shadow-2xl z-50 w-64">
                      <div className="flex flex-col gap-2">
                        <input
                          autoFocus
                          value={linkText}
                          onChange={e => setLinkText(e.target.value)}
                          placeholder={tr.compose.linkText}
                          className="w-full bg-white/[0.05] border border-white/10 rounded-lg px-2.5 py-1.5 text-xs text-zinc-200 outline-none focus:border-blue-500/50 placeholder:text-zinc-600"
                        />
                        <input
                          value={linkUrl}
                          onChange={e => setLinkUrl(e.target.value)}
                          placeholder="https://..."
                          className="w-full bg-white/[0.05] border border-white/10 rounded-lg px-2.5 py-1.5 text-xs text-zinc-200 outline-none focus:border-blue-500/50 placeholder:text-zinc-600"
                          onKeyDown={e => e.key === "Enter" && applyLink()}
                        />
                        <div className="flex gap-2 justify-end pt-0.5">
                          <button
                            type="button"
                            onClick={() => setLinkPopover(false)}
                            className="text-xs text-zinc-500 hover:text-zinc-300 transition-colors"
                          >{tr.mail.cancel}</button>
                          <button
                            type="button"
                            onClick={applyLink}
                            disabled={!linkUrl}
                            className="px-3 py-1 bg-blue-500 hover:bg-blue-600 disabled:opacity-40 text-white text-xs rounded-md transition-colors"
                          >{tr.common.apply}</button>
                        </div>
                      </div>
                    </div>
                  )}

                  {/* Format buttons */}
                  <button type="button" title={tr.compose.undo} aria-label={tr.compose.undo} disabled={!canUndo} onMouseDown={e => { e.preventDefault(); applyFormat("undo"); }}
                    className={`w-7 h-7 flex items-center justify-center rounded transition-colors ${canUndo ? "text-zinc-400 hover:text-zinc-200 hover:bg-white/[0.06] cursor-pointer" : "text-zinc-700 cursor-default"}`}>
                    <Undo2 className="w-3.5 h-3.5" />
                  </button>
                  <button type="button" title={tr.compose.redo} aria-label={tr.compose.redo} disabled={!canRedo} onMouseDown={e => { e.preventDefault(); applyFormat("redo"); }}
                    className={`w-7 h-7 flex items-center justify-center rounded transition-colors ${canRedo ? "text-zinc-400 hover:text-zinc-200 hover:bg-white/[0.06] cursor-pointer" : "text-zinc-700 cursor-default"}`}>
                    <Redo2 className="w-3.5 h-3.5" />
                  </button>
                  <div className="w-px h-4 bg-white/10 mx-1 shrink-0" />
                  {([
                    { cmd: "bold",          label: "B",  cls: "font-bold",      title: tr.compose.bold },
                    { cmd: "italic",        label: "I",  cls: "italic",         title: tr.compose.italic },
                    { cmd: "underline",     label: "U",  cls: "underline",      title: tr.compose.underline },
                    { cmd: "strikeThrough", label: "S",  cls: "line-through",   title: tr.compose.strikethrough },
                  ] as { cmd: string; label: string; cls: string; title: string }[]).map(({ cmd, label, cls, title }) => (
                    <button
                      key={cmd}
                      type="button"
                      title={title}
                      aria-label={title}
                      onMouseDown={e => { e.preventDefault(); applyFormat(cmd); }}
                      className="w-7 h-7 flex items-center justify-center rounded text-zinc-400 hover:text-zinc-200 hover:bg-white/[0.06] text-xs transition-colors"
                    >
                      <span className={cls}>{label}</span>
                    </button>
                  ))}

                  <div className="w-px h-4 bg-white/10 mx-1 shrink-0" />

                  <button
                    type="button"
                    title={tr.compose.insertLink}
                    aria-label={tr.compose.insertLink}
                    onMouseDown={e => {
                      e.preventDefault();
                      saveSelection();
                      setLinkUrl("");
                      setLinkPopover(v => !v);
                    }}
                    className={`w-7 h-7 flex items-center justify-center rounded transition-colors ${
                      linkPopover ? "text-blue-400 bg-blue-500/10" : "text-zinc-400 hover:text-zinc-200 hover:bg-white/[0.06]"
                    }`}
                  >
                    <Link2 className="w-3.5 h-3.5" />
                  </button>

                  <div className="w-px h-4 bg-white/10 mx-1 shrink-0" />

                  <button
                    type="button"
                    title={tr.compose.numberedList}
                    aria-label={tr.compose.numberedList}
                    onMouseDown={e => { e.preventDefault(); applyFormat("insertOrderedList"); }}
                    className="w-7 h-7 flex items-center justify-center rounded text-zinc-400 hover:text-zinc-200 hover:bg-white/[0.06] transition-colors"
                  >
                    <ListOrdered className="w-3.5 h-3.5" />
                  </button>
                  <button
                    type="button"
                    title={tr.compose.bulletList}
                    aria-label={tr.compose.bulletList}
                    onMouseDown={e => { e.preventDefault(); applyFormat("insertUnorderedList"); }}
                    className="w-7 h-7 flex items-center justify-center rounded text-zinc-400 hover:text-zinc-200 hover:bg-white/[0.06] transition-colors"
                  >
                    <List className="w-3.5 h-3.5" />
                  </button>
                </div>
              )}

              {/* Reply attachment chips */}
              {replyAttachments.length > 0 && (
                <div className="px-3 pb-1 flex flex-wrap gap-1.5">
                  {replyAttachments.map((att, idx) => (
                    <div key={idx} className="flex items-center gap-1.5 px-2 py-1 rounded-md bg-white/[0.04] border border-white/[0.07] text-zinc-400 max-w-[200px]">
                      <AttachmentIcon mimeType={att.mimeType} />
                      <span className="text-[11px] truncate min-w-0">{att.filename}</span>
                      <span className="text-[10px] text-zinc-600 shrink-0">{formatBytes(att.size)}</span>
                      <button type="button" aria-label={`${tr.compose.removeAttachment}: ${att.filename}`} onClick={() => setReplyAttachments(p => p.filter((_, i) => i !== idx))} className="shrink-0 text-zinc-600 hover:text-zinc-300 transition-colors">
                        <X className="w-3 h-3" />
                      </button>
                    </div>
                  ))}
                </div>
              )}
              {(replyAttachError || replyDraftError) && (
                <div className="mx-3 mb-1.5 flex items-center gap-2 text-xs text-red-400 bg-red-400/10 border border-red-400/20 rounded-lg px-2.5 py-1.5">
                  <span className="min-w-0">{replyAttachError || replyDraftError}</span>
                  <button type="button" aria-label={tr.common.close} onClick={() => { setReplyAttachError(null); setReplyDraftError(null); }} className="ml-auto shrink-0 text-red-400/60 hover:text-red-400"><X className="w-3 h-3" /></button>
                </div>
              )}

              {/* Bottom action bar */}
              <div className="px-3 py-2 border-t border-white/5 flex items-center justify-between">
                <div className="flex items-center gap-1">
                  {/* Paperclip */}
                  <button
                    type="button"
                    title={tr.compose.attachFile}
                    aria-label={tr.compose.attachFile}
                    onClick={() => replyFileInputRef.current?.click()}
                    className="w-7 h-7 flex items-center justify-center rounded-lg text-zinc-500 hover:text-zinc-300 hover:bg-white/[0.04] transition-colors"
                  >
                    <Paperclip className="w-3.5 h-3.5" />
                  </button>
                  <input ref={replyFileInputRef} type="file" multiple className="hidden" onChange={handleReplyFileSelect} />

                  {/* Formatting toggle */}
                  <button
                    type="button"
                    title={tr.compose.formatting}
                    aria-label={tr.compose.formatting}
                    onClick={() => { setShowFormatBar(v => !v); setLinkPopover(false); }}
                    className={`flex items-center gap-1 px-2 py-1.5 rounded-lg text-xs transition-colors ${
                      showFormatBar ? "text-blue-400 bg-blue-500/10" : "text-zinc-500 hover:text-zinc-300 hover:bg-white/[0.04]"
                    }`}
                  >
                    <Type className="w-3.5 h-3.5" />
                    <ChevronDown className={`w-3 h-3 transition-transform duration-150 ${showFormatBar ? "rotate-180" : ""}`} />
                  </button>
                </div>

                <div className="flex items-center gap-2">
                  <button
                    type="button"
                    onClick={() => { void deleteInlineReplyDraft(); }}
                    title={tr.compose.deleteDraft}
                    aria-label={tr.compose.deleteDraft}
                    className="rounded-md p-1.5 text-zinc-600 transition-colors hover:bg-red-500/10 hover:text-red-400"
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                  </button>
                  <button
                    type="button"
                    onClick={() => {
                      dismissedReplyKeyRef.current = inlineReplyStorageKey(replyTarget);
                      void persistReplyDraft().catch(() => undefined).finally(() => {
                        setShowReply(false);
                        setReplyTarget(null);
                      });
                    }}
                    className="text-xs text-zinc-500 hover:text-zinc-300 transition-colors"
                  >
                    {tr.mail.cancel}
                  </button>
                  <button
                    type="button"
                    onClick={() => { void sendInlineReply(); }}
                    disabled={(replyEmpty && replyAttachments.length === 0) || !replyDraftHydrated || replyTargetBody === null || isSending || pendingReplyAttachmentReads > 0}
                    className="px-4 py-1.5 bg-blue-500 hover:bg-blue-600 disabled:opacity-40 text-white text-xs font-medium rounded-md transition-colors flex items-center gap-2"
                  >
                    {isSending ? <RefreshCw className="w-3 h-3 animate-spin" /> : <Send className="w-3 h-3" />}
                    {isSending ? tr.compose.sending : tr.mail.sendReply}
                  </button>
                </div>
              </div>
            </div>
          ), replyPortalHost)}
        </div>
      </div>
    </main>
  );
}

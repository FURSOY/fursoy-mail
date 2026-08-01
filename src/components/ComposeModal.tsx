import { X, RefreshCw, Send, ChevronDown, AlertCircle, Paperclip, FileText, Image, File, Type, Link2, List, ListOrdered, Undo2, Redo2, Trash2, Clock3 } from "lucide-react";
import { useState, useRef, useEffect, useCallback } from "react";
import { useLocale } from "../i18n";
import { ui } from "../theme";
import type { Account, AttachmentPayload, DraftSummary } from "../types";
import { tauriApi, type ContactSuggestion } from "../tauriApi";
import { normalizeComposerLinkUrl, sanitizeComposerHtml } from "../utils";
import { ToolbarTip } from "./ToolbarTip";
import { ProfileAvatar } from "./ProfileAvatar";

// Gmail blocks these extensions (and so do we)
const BLOCKED_EXTENSIONS = new Set([
  "exe", "bat", "cmd", "com", "msi", "scr", "pif", "vbs", "vbe",
  "js", "jse", "jar", "wsf", "wsh", "ps1", "reg", "inf", "lnk",
]);

// Gmail's total attachment limit is 25 MB (MIME encoded).
// Base64 adds ~33% overhead, so we cap raw file bytes at 20 MB total.
const MAX_TOTAL_BYTES = 20 * 1024 * 1024;
const MAX_ATTACHMENT_FILES = 100;

interface ComposeModalProps {
  composeTo: string;
  setComposeTo: (v: string) => void;
  composeSubject: string;
  setComposeSubject: (v: string) => void;
  composeBody: string;
  setComposeBody: (v: string) => void;
  composeHtmlAppend: string;
  isSending: boolean;
  sendError: string | null;
  onSend: (cc: string, bcc: string, attachments: AttachmentPayload[], body: string, draftId: string | null, verificationMessageId: string | null) => Promise<boolean>;
  onClose: (saved: boolean) => void;
  onClear: () => void;
  accounts: Account[];
  composeAccountId: string | null;
  setComposeAccountId: (id: string) => void;
}

function emailColor(email: string): string {
  let h = 0;
  for (let i = 0; i < email.length; i++) h = email.charCodeAt(i) + ((h << 5) - h);
  const palette = ["#3b82f6", "#8b5cf6", "#ec4899", "#f59e0b", "#10b981", "#06b6d4", "#ef4444", "#f97316"];
  return palette[Math.abs(h) % palette.length];
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function fileIcon(mimeType: string) {
  if (mimeType.startsWith("image/")) return <Image className="w-3.5 h-3.5" />;
  if (mimeType === "application/pdf" || mimeType.startsWith("text/")) return <FileText className="w-3.5 h-3.5" />;
  return <File className="w-3.5 h-3.5" />;
}

interface AttachmentItem extends AttachmentPayload {
  size: number;
}

export function ComposeModal({
  composeTo, setComposeTo,
  composeSubject, setComposeSubject,
  composeBody, setComposeBody,
  composeHtmlAppend,
  isSending,
  sendError,
  onSend,
    onClose,
    onClear,
  accounts,
  composeAccountId,
  setComposeAccountId,
}: ComposeModalProps) {
  const tr = useLocale();
  const [fromOpen, setFromOpen] = useState(false);
  const [suggestions, setSuggestions] = useState<ContactSuggestion[]>([]);
  const [suggOpen, setSuggOpen] = useState(false);
  const [highlightIdx, setHighlightIdx] = useState(0);
  const [attachments, setAttachments] = useState<AttachmentItem[]>([]);
  const [attachError, setAttachError] = useState<string | null>(null);
  const [pendingAttachmentReads, setPendingAttachmentReads] = useState(0);
  const [showFormatBar, setShowFormatBar] = useState(false);
  const [linkPopover, setLinkPopover] = useState(false);
  const [linkText, setLinkText] = useState("");
  const [linkUrl, setLinkUrl] = useState("");
  const [bodyEmpty, setBodyEmpty] = useState(true);
  const [canUndo, setCanUndo] = useState(false);
  const [canRedo, setCanRedo] = useState(false);
  const [draftId, setDraftId] = useState<string | null>(null);
  const [drafts, setDrafts] = useState<DraftSummary[]>([]);
  const [draftsLoading, setDraftsLoading] = useState(false);
  const [hasMoreDrafts, setHasMoreDrafts] = useState(false);
  const [draftStatus, setDraftStatus] = useState<"idle" | "saving" | "saved" | "error">("idle");
  const [draftError, setDraftError] = useState<string | null>(null);
  const [confirmDiscard, setConfirmDiscard] = useState(false);
  const [confirmSubjectless, setConfirmSubjectless] = useState(false);
  const [composeCc, setComposeCc] = useState("");
  const [composeBcc, setComposeBcc] = useState("");
  const [showCc, setShowCc] = useState(false);
  const [showBcc, setShowBcc] = useState(false);
  const [draftActionPending, setDraftActionPending] = useState(false);
  const draftActionPendingRef = useRef(false);
  const modalRef = useRef<HTMLDivElement>(null);
  const fromRef = useRef<HTMLDivElement>(null);
  const toRef = useRef<HTMLInputElement>(null);
  const suggRef = useRef<HTMLDivElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const bodyEditableRef = useRef<HTMLDivElement>(null);
  const savedRangeRef = useRef<Range | null>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const focusTimerRef = useRef<number | null>(null);
  const attachmentReadersRef = useRef<Set<FileReader>>(new Set());
  const pendingAttachmentBytesRef = useRef(0);
  const mountedRef = useRef(true);
  const contactSearchRequestIdRef = useRef(0);
  const draftIdRef = useRef<string | null>(null);
  const draftVerificationMessageIdRef = useRef<string | null>(null);
  const draftSaveTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const draftSaveQueueRef = useRef<Promise<string | null>>(Promise.resolve(null));
  const draftListRequestIdRef = useRef(0);
  const draftListLoadingRef = useRef(false);
  const nextDraftPageTokenRef = useRef<string | null>(null);
  const draftCreateOutcomeUnknownRef = useRef<string | null>(null);

  const activeAccount = accounts.find(a => a.id === composeAccountId) ?? accounts[0];
  const supportsRemoteDrafts = activeAccount?.provider === "google" || activeAccount?.provider === "imap";

  const beginDraftAction = () => {
    if (draftActionPendingRef.current) return false;
    draftActionPendingRef.current = true;
    setDraftActionPending(true);
    return true;
  };

  const finishDraftAction = () => {
    draftActionPendingRef.current = false;
    setDraftActionPending(false);
  };

  const bodyText = useCallback((html: string) => {
    const doc = new DOMParser().parseFromString(html, "text/html");
    return doc.body.textContent?.trim() ?? "";
  }, []);

  const hasDraftContent = useCallback((body = composeBody, items = attachments) =>
    Boolean(composeTo.trim() || composeCc.trim() || composeBcc.trim() || composeSubject.trim() || bodyText(body) || items.length > 0),
  [attachments, bodyText, composeBcc, composeBody, composeCc, composeSubject, composeTo]);

  const updateDraftSummary = useCallback((id: string, updatedAt: number, body: string) => {
    const summary: DraftSummary = {
      id,
      messageId: "",
      rfcMessageId: "",
      threadId: "",
      inReplyTo: "",
      references: "",
      to: composeTo,
      cc: composeCc,
      bcc: composeBcc,
      subject: composeSubject,
      snippet: bodyText(body).slice(0, 120),
      updatedAt,
    };
    setDrafts(previous => [summary, ...previous.filter(item => item.id !== id)]
      .sort((left, right) => right.updatedAt - left.updatedAt));
  }, [bodyText, composeBcc, composeCc, composeSubject, composeTo]);

  const persistDraft = useCallback((body: string, items: AttachmentItem[]) => {
    if (!supportsRemoteDrafts) return draftSaveQueueRef.current;
    const existingDraftId = draftIdRef.current;
    if (!activeAccount?.id || pendingAttachmentReads > 0 || (!hasDraftContent(body, items) && !existingDraftId)) {
      return draftSaveQueueRef.current;
    }
    if (!existingDraftId && draftCreateOutcomeUnknownRef.current) {
      return Promise.reject(new Error(draftCreateOutcomeUnknownRef.current));
    }
    setDraftStatus("saving");
    setDraftError(null);
    const snapshot = {
      accountId: activeAccount.id,
      to: composeTo,
      cc: composeCc,
      bcc: composeBcc,
      subject: composeSubject,
      body: sanitizeComposerHtml(body),
      attachments: items.map(({ filename, mimeType, data }) => ({ filename, mimeType, data })),
    };
    draftSaveQueueRef.current = draftSaveQueueRef.current
      .catch(() => null)
      .then(async () => {
        const saved = await tauriApi.saveDraft({
          ...snapshot,
          draftId: draftIdRef.current,
          attachments: snapshot.attachments.length > 0 ? snapshot.attachments : null,
        });
        draftIdRef.current = saved.id;
        draftVerificationMessageIdRef.current = saved.verificationMessageId;
        draftCreateOutcomeUnknownRef.current = null;
        setDraftId(saved.id);
        setDraftStatus("saved");
        updateDraftSummary(saved.id, saved.updatedAt, snapshot.body);
        return saved.id;
      })
      .catch(error => {
        const message = String(error).replace(/^Error:\s*/i, "");
        if (!draftIdRef.current && /outcome is unknown/i.test(message)) {
          draftCreateOutcomeUnknownRef.current = message;
        }
        setDraftStatus("error");
        setDraftError(message);
        throw error;
      });
    return draftSaveQueueRef.current;
  }, [activeAccount?.id, composeBcc, composeCc, composeSubject, composeTo, hasDraftContent, pendingAttachmentReads, supportsRemoteDrafts, updateDraftSummary]);

  const loadDrafts = useCallback(async (reset: boolean) => {
    const accountId = activeAccount?.id;
    if (!supportsRemoteDrafts || !accountId || draftListLoadingRef.current) return;
    const pageToken = reset ? null : nextDraftPageTokenRef.current;
    if (!reset && !pageToken) return;
    draftListLoadingRef.current = true;
    setDraftsLoading(true);
    const requestId = ++draftListRequestIdRef.current;
    try {
      const page = await tauriApi.listDrafts(accountId, pageToken);
      if (requestId !== draftListRequestIdRef.current) return;
      setDrafts(previous => {
        if (reset) return page.drafts;
        const known = new Set(previous.map(item => item.id));
        return [...previous, ...page.drafts.filter(item => !known.has(item.id))];
      });
      nextDraftPageTokenRef.current = page.nextPageToken;
      setHasMoreDrafts(Boolean(page.nextPageToken));
    } catch (error) {
      if (requestId !== draftListRequestIdRef.current) return;
      setDraftError(String(error).replace(/^Error:\s*/i, ""));
    } finally {
      if (requestId === draftListRequestIdRef.current) {
        draftListLoadingRef.current = false;
        setDraftsLoading(false);
      }
    }
  }, [activeAccount?.id, supportsRemoteDrafts]);

  useEffect(() => {
    mountedRef.current = true;
    focusTimerRef.current = window.setTimeout(() => toRef.current?.focus(), 0);
    return () => {
      mountedRef.current = false;
      if (focusTimerRef.current) clearTimeout(focusTimerRef.current);
      if (debounceRef.current) clearTimeout(debounceRef.current);
      if (draftSaveTimerRef.current) clearTimeout(draftSaveTimerRef.current);
      for (const reader of attachmentReadersRef.current) reader.abort();
      attachmentReadersRef.current.clear();
      pendingAttachmentBytesRef.current = 0;
    };
  }, []);

  useEffect(() => {
    if (!fromOpen) return;
    const h = (e: MouseEvent) => {
      if (fromRef.current && !fromRef.current.contains(e.target as Node)) setFromOpen(false);
    };
    document.addEventListener("mousedown", h);
    return () => document.removeEventListener("mousedown", h);
  }, [fromOpen]);

  useEffect(() => {
    if (!suggOpen) return;
    const h = (e: MouseEvent) => {
      if (toRef.current && !toRef.current.contains(e.target as Node) &&
          suggRef.current && !suggRef.current.contains(e.target as Node)) {
        setSuggOpen(false);
      }
    };
    document.addEventListener("mousedown", h);
    return () => document.removeEventListener("mousedown", h);
  }, [suggOpen]);

  useEffect(() => {
    contactSearchRequestIdRef.current += 1;
    setSuggestions([]);
    setSuggOpen(false);
  }, [activeAccount?.id]);

  useEffect(() => {
    draftIdRef.current = null;
    draftVerificationMessageIdRef.current = null;
    draftCreateOutcomeUnknownRef.current = null;
    draftListRequestIdRef.current += 1;
    draftListLoadingRef.current = false;
    nextDraftPageTokenRef.current = null;
    setDraftId(null);
    setDrafts([]);
    setHasMoreDrafts(false);
    setDraftStatus("idle");
    setDraftError(null);
    void loadDrafts(true);
  }, [activeAccount?.id, loadDrafts]);

  useEffect(() => {
    if (!supportsRemoteDrafts || (!hasDraftContent() && !draftIdRef.current) || pendingAttachmentReads > 0 || draftActionPending || isSending) return;
    if (draftSaveTimerRef.current) clearTimeout(draftSaveTimerRef.current);
    draftSaveTimerRef.current = setTimeout(() => {
      const html = bodyEditableRef.current?.innerHTML ?? composeBody;
      void persistDraft(html, attachments).catch(() => undefined);
    }, 1000);
    return () => {
      if (draftSaveTimerRef.current) clearTimeout(draftSaveTimerRef.current);
    };
  }, [attachments, composeBcc, composeBody, composeCc, composeSubject, composeTo, draftActionPending, hasDraftContent, isSending, pendingAttachmentReads, persistDraft, supportsRemoteDrafts]);

  const searchContacts = useCallback((q: string) => {
    if (debounceRef.current) clearTimeout(debounceRef.current);
    const trimmed = q.trim();
    if (trimmed.length < 1) { setSuggestions([]); setSuggOpen(false); return; }
    const accountId = activeAccount?.id;
    if (!accountId) { setSuggestions([]); setSuggOpen(false); return; }
    const requestId = ++contactSearchRequestIdRef.current;
    debounceRef.current = setTimeout(async () => {
      try {
        const res = await tauriApi.searchContacts(trimmed, accountId);
        if (requestId !== contactSearchRequestIdRef.current || activeAccount?.id !== accountId) return;
        setSuggestions(res);
        setSuggOpen(res.length > 0);
        setHighlightIdx(0);
      } catch { /* ignore */ }
    }, 200);
  }, [activeAccount?.id]);

  const handleToChange = (v: string) => {
    setComposeTo(v);
    const token = v.split(",").pop()?.trim() ?? "";
    searchContacts(token);
  };

  const applySuggestion = (s: ContactSuggestion) => {
    const parts = composeTo.split(",");
    parts[parts.length - 1] = s.name ? `"${s.name}" <${s.email}>` : s.email;
    setComposeTo(parts.join(", ") + ", ");
    setSuggOpen(false);
    setSuggestions([]);
    if (focusTimerRef.current) clearTimeout(focusTimerRef.current);
    focusTimerRef.current = window.setTimeout(() => toRef.current?.focus(), 0);
  };

  const handleToKeyDown = (e: React.KeyboardEvent) => {
    if (!suggOpen || suggestions.length === 0) return;
    if (e.key === "ArrowDown") { e.preventDefault(); setHighlightIdx(i => Math.min(i + 1, suggestions.length - 1)); }
    else if (e.key === "ArrowUp") { e.preventDefault(); setHighlightIdx(i => Math.max(i - 1, 0)); }
    else if (e.key === "Enter" || e.key === "Tab") {
      if (suggestions[highlightIdx]) { e.preventDefault(); applySuggestion(suggestions[highlightIdx]); }
    } else if (e.key === "Escape") { setSuggOpen(false); }
  };

  const addAttachmentFiles = (files: File[]) => {
    if (files.length === 0) return;

    setAttachError(null);

    if (attachments.length + attachmentReadersRef.current.size + files.length > MAX_ATTACHMENT_FILES) {
      setAttachError(tr.compose.attachmentCountLimit);
      return;
    }

    // Check for blocked extensions
    const blocked = files.filter(f => {
      const ext = f.name.split(".").pop()?.toLowerCase() ?? "";
      return BLOCKED_EXTENSIONS.has(ext);
    });
    if (blocked.length > 0) {
      setAttachError(tr.compose.blockedFileType.replace("{files}", blocked.map(f => f.name).join(", ")));
      return;
    }

    // Check total size (existing + new)
    const existingBytes = attachments.reduce((s, a) => s + a.size, 0);
    const newBytes = files.reduce((s, f) => s + f.size, 0);
    if (existingBytes + pendingAttachmentBytesRef.current + newBytes > MAX_TOTAL_BYTES) {
      const remainingMB = ((MAX_TOTAL_BYTES - existingBytes - pendingAttachmentBytesRef.current) / (1024 * 1024)).toFixed(1);
      setAttachError(tr.compose.attachmentRemaining.replace("{remaining}", remainingMB));
      return;
    }

    pendingAttachmentBytesRef.current += newBytes;
    setPendingAttachmentReads(prev => prev + files.length);
    files.forEach(file => {
      const reader = new FileReader();
      attachmentReadersRef.current.add(reader);
      let settled = false;
      const finishRead = () => {
        if (settled) return;
        settled = true;
        attachmentReadersRef.current.delete(reader);
        pendingAttachmentBytesRef.current = Math.max(0, pendingAttachmentBytesRef.current - file.size);
        if (mountedRef.current) setPendingAttachmentReads(prev => Math.max(0, prev - 1));
      };
      reader.onload = () => {
        try {
          if (!mountedRef.current) return;
          if (typeof reader.result !== "string" || !reader.result.includes(",")) {
            throw new Error("Invalid FileReader result");
          }
          const base64 = reader.result.split(",", 2)[1];
          setAttachments(prev => [...prev, {
            filename: file.name,
            mimeType: file.type || "application/octet-stream",
            data: base64,
            size: file.size,
          }]);
        } catch {
          if (mountedRef.current) setAttachError(`${tr.compose.attachmentReadFailed}: ${file.name}`);
        } finally {
          finishRead();
        }
      };
      reader.onerror = () => {
        if (mountedRef.current) setAttachError(`${tr.compose.attachmentReadFailed}: ${file.name}`);
        finishRead();
      };
      reader.onabort = finishRead;
      try {
        reader.readAsDataURL(file);
      } catch {
        if (mountedRef.current) setAttachError(`${tr.compose.attachmentReadFailed}: ${file.name}`);
        finishRead();
      }
    });
  };

  const handleFileSelect = (e: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(e.target.files ?? []);
    e.target.value = "";
    addAttachmentFiles(files);
  };

  const handleBodyPaste = (e: React.ClipboardEvent<HTMLDivElement>) => {
    const imageFiles = Array.from(e.clipboardData.items)
      .filter(item => item.kind === "file" && item.type.startsWith("image/"))
      .map(item => item.getAsFile())
      .filter((file): file is File => file !== null);
    if (imageFiles.length > 0) {
      e.preventDefault();
      addAttachmentFiles(imageFiles);
      return;
    }

    const plainText = e.clipboardData.getData("text/plain");
    const htmlText = e.clipboardData.getData("text/html");
    if (!plainText && !htmlText) return;
    e.preventDefault();
    const safeText = plainText || new DOMParser().parseFromString(htmlText, "text/html").body.textContent || "";
    document.execCommand("insertText", false, safeText);
  };

  const removeAttachment = (idx: number) => {
    setAttachments(prev => prev.filter((_, i) => i !== idx));
  };

  // Sync composeHtmlAppend (forward) into contenteditable
  useEffect(() => {
    if (!composeHtmlAppend || !bodyEditableRef.current) return;
    const sep = '<br/><br/><div style="border-top:1px solid rgba(255,255,255,0.08);margin:8px 0;"></div>';
    bodyEditableRef.current.innerHTML =
      sanitizeComposerHtml(bodyEditableRef.current.innerHTML || "") +
      sep +
      sanitizeComposerHtml(composeHtmlAppend);
    setComposeBody(bodyEditableRef.current.innerHTML);
    setBodyEmpty(false);
  }, [composeHtmlAppend]);

  // Keep externally loaded draft HTML and the editor in sync without moving
  // the caret during ordinary typing.
  useEffect(() => {
    if (composeBody === "" && bodyEditableRef.current) {
      bodyEditableRef.current.innerHTML = "";
      setBodyEmpty(true);
    } else if (composeBody && bodyEditableRef.current) {
      const safeBody = sanitizeComposerHtml(composeBody);
      if (bodyEditableRef.current.innerHTML !== safeBody) {
        bodyEditableRef.current.innerHTML = safeBody;
      }
      setBodyEmpty(!(bodyEditableRef.current.innerText.trim()));
    }
  }, [composeBody]);

  const syncUndoRedo = () => {
    setCanUndo(document.queryCommandEnabled("undo"));
    setCanRedo(document.queryCommandEnabled("redo"));
  };

  const applyFormat = (command: string, value?: string) => {
    bodyEditableRef.current?.focus();
    document.execCommand(command, false, value);
    setBodyEmpty(!(bodyEditableRef.current?.innerText.trim()));
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
    bodyEditableRef.current?.focus();
    if (linkText && !window.getSelection()?.toString()) {
      const link = document.createElement("a");
      link.href = safeUrl;
      link.textContent = linkText;
      const selection = window.getSelection();
      const range = selection?.rangeCount ? selection.getRangeAt(0) : null;
      if (range && bodyEditableRef.current?.contains(range.commonAncestorContainer)) {
        range.deleteContents();
        range.insertNode(link);
        range.setStartAfter(link);
        range.collapse(true);
        selection?.removeAllRanges();
        selection?.addRange(range);
      } else {
        bodyEditableRef.current?.append(link);
      }
    } else {
      document.execCommand("createLink", false, safeUrl);
    }
    setBodyEmpty(!(bodyEditableRef.current?.innerText.trim()));
    setLinkPopover(false);
    setLinkText("");
    setLinkUrl("");
  };

  const handleClose = async () => {
    if (!supportsRemoteDrafts) {
      if (hasDraftContent()) setConfirmDiscard(true);
      else onClose(false);
      return;
    }
    if (pendingAttachmentReads > 0 || !beginDraftAction()) return;
    if (draftSaveTimerRef.current) clearTimeout(draftSaveTimerRef.current);
    try {
      if (hasDraftContent() || draftIdRef.current) {
        await persistDraft(bodyEditableRef.current?.innerHTML ?? composeBody, attachments);
        onClose(true);
      } else {
        onClose(false);
      }
    } catch {
      // Keep the composer open: closing after a failed save would lose work.
    } finally {
      finishDraftAction();
    }
  };

  const handleDiscard = async () => {
    if (!beginDraftAction()) return;
    if (draftSaveTimerRef.current) clearTimeout(draftSaveTimerRef.current);
    try {
      await draftSaveQueueRef.current.catch(() => null);
      const discardedDraftId = draftIdRef.current;
      if (discardedDraftId && activeAccount?.id) {
        await tauriApi.deleteDraft(activeAccount.id, discardedDraftId);
        setDrafts(previous => previous.filter(item => item.id !== discardedDraftId));
      }
      draftIdRef.current = null;
      draftVerificationMessageIdRef.current = null;
      draftCreateOutcomeUnknownRef.current = null;
      setDraftId(null);
      setComposeTo("");
      setComposeCc("");
      setComposeBcc("");
      setShowCc(false);
      setShowBcc(false);
      setComposeSubject("");
      setComposeBody("");
      setAttachments([]);
      setAttachError(null);
      setDraftStatus("idle");
      setDraftError(null);
      if (bodyEditableRef.current) bodyEditableRef.current.innerHTML = "";
      setBodyEmpty(true);
      onClear();
    } catch (error) {
      setDraftStatus("error");
      setDraftError(String(error).replace(/^Error:\s*/i, ""));
    } finally {
      setConfirmDiscard(false);
      finishDraftAction();
    }
  };

  const sendNow = async () => {
    if (!beginDraftAction()) return;
    if (draftSaveTimerRef.current) clearTimeout(draftSaveTimerRef.current);
    try {
      const body = bodyEditableRef.current?.innerHTML ?? composeBody;
      if (supportsRemoteDrafts) await persistDraft(body, attachments);
      await onSend(
        composeCc,
        composeBcc,
        attachments.map(({ filename, mimeType, data }) => ({ filename, mimeType, data })),
        body,
        draftIdRef.current,
        draftVerificationMessageIdRef.current,
      );
    } catch {
      // persistDraft already exposes the save error and keeps the composer open.
    } finally {
      finishDraftAction();
    }
  };

  const handleDialogKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      if (confirmSubjectless) setConfirmSubjectless(false);
      else if (confirmDiscard) setConfirmDiscard(false);
      else void handleClose();
      return;
    }
    if (event.key !== "Tab") return;
    const focusable = Array.from(modalRef.current?.querySelectorAll<HTMLElement>(
      'button:not([disabled]), input:not([disabled]), [contenteditable="true"], [tabindex]:not([tabindex="-1"])'
    ) ?? []).filter(element => element.offsetParent !== null);
    if (focusable.length === 0) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  };

  const handleSend = async () => {
    if (!canSend) return;
    if (!composeSubject.trim()) {
      setConfirmSubjectless(true);
      return;
    }
    await sendNow();
  };

  const openDraft = async (selectedId: string) => {
    if (!activeAccount?.id || selectedId === draftIdRef.current || !beginDraftAction()) return;
    if (draftSaveTimerRef.current) clearTimeout(draftSaveTimerRef.current);
    setDraftError(null);
    try {
      if (hasDraftContent() || draftIdRef.current) {
        await persistDraft(bodyEditableRef.current?.innerHTML ?? composeBody, attachments);
      }
      const selected = await tauriApi.getDraft(activeAccount.id, selectedId);
      draftIdRef.current = selected.id;
      draftVerificationMessageIdRef.current = null;
      setDraftId(selected.id);
      setComposeTo(selected.to);
      setComposeCc(selected.cc);
      setComposeBcc(selected.bcc);
      setShowCc(Boolean(selected.cc.trim()));
      setShowBcc(Boolean(selected.bcc.trim()));
      setComposeSubject(selected.subject);
      setComposeBody(sanitizeComposerHtml(selected.body));
      setAttachments(selected.attachments.map(attachment => ({
        ...attachment,
        size: Math.floor(attachment.data.length * 0.75),
      })));
      setDraftStatus("saved");
    } catch (error) {
      setDraftStatus("error");
      setDraftError(String(error).replace(/^Error:\s*/i, ""));
    } finally {
      finishDraftAction();
    }
  };

  const startNewDraft = async () => {
    if (!beginDraftAction()) return;
    if (draftSaveTimerRef.current) clearTimeout(draftSaveTimerRef.current);
    try {
      if (hasDraftContent() || draftIdRef.current) {
        await persistDraft(bodyEditableRef.current?.innerHTML ?? composeBody, attachments);
      }
      draftIdRef.current = null;
      draftVerificationMessageIdRef.current = null;
      draftCreateOutcomeUnknownRef.current = null;
      setDraftId(null);
      setComposeTo("");
      setComposeCc("");
      setComposeBcc("");
      setShowCc(false);
      setShowBcc(false);
      setComposeSubject("");
      setComposeBody("");
      setAttachments([]);
      setDraftStatus("idle");
      setDraftError(null);
    } catch {
      // The save error is already displayed by persistDraft.
    } finally {
      finishDraftAction();
    }
  };

  const changeComposeAccount = async (accountId: string) => {
    if (accountId === activeAccount?.id) {
      setFromOpen(false);
      return;
    }
    if (pendingAttachmentReads > 0 || !beginDraftAction()) return;
    if (draftSaveTimerRef.current) clearTimeout(draftSaveTimerRef.current);
    try {
      if (hasDraftContent() || draftIdRef.current) {
        await persistDraft(bodyEditableRef.current?.innerHTML ?? composeBody, attachments);
      }
      setComposeAccountId(accountId);
      setFromOpen(false);
    } catch {
      // Keep the current account selected when its draft could not be saved.
    } finally {
      finishDraftAction();
    }
  };

  const formatDraftTime = (timestamp: number) => {
    const date = new Date(timestamp);
    const today = new Date();
    if (date.toDateString() === today.toDateString()) {
      return new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit" }).format(date);
    }
    return new Intl.DateTimeFormat(undefined, { day: "2-digit", month: "short" }).format(date);
  };

  const canSend = composeTo.trim().length > 0
    && !isSending
    && !draftActionPending
    && pendingAttachmentReads === 0
    && (!bodyEmpty || attachments.length > 0);

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm p-3">
      <div
        ref={modalRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="compose-dialog-title"
        onKeyDown={handleDialogKeyDown}
        className="w-full max-w-4xl bg-[var(--color-surface-panel)] border border-[var(--color-border-default)] rounded-[var(--radius-lg)] shadow-[var(--shadow-panel)] flex overflow-hidden"
        style={{ height: "min(600px, 92vh)" }}
      >
        <div className="min-w-0 flex-1 flex flex-col">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-[var(--color-border-subtle)]">
          <div className="flex items-center gap-3 min-w-0">
            <h3 id="compose-dialog-title" className="text-[length:var(--font-size-body)] font-semibold text-[var(--color-text-primary)] truncate">{composeHtmlAppend ? tr.mail.forward : tr.compose.title}</h3>
            {draftStatus !== "idle" && (
              <span className={`text-[length:var(--font-size-caption)] ${draftStatus === "error" ? "text-[var(--color-status-danger)]" : "text-[var(--color-text-disabled)]"}`}>
                {draftStatus === "saving" ? tr.compose.savingDraft : draftStatus === "saved" ? tr.compose.draftSaved : tr.compose.draftSaveFailed}
              </span>
            )}
          </div>
          <ToolbarTip label={supportsRemoteDrafts ? tr.compose.saveAndClose : tr.common.close}>
            <button type="button" aria-label={supportsRemoteDrafts ? tr.compose.saveAndClose : tr.common.close} onClick={() => void handleClose()} disabled={draftActionPending || pendingAttachmentReads > 0} className={ui.iconButton}>
              {draftActionPending ? <RefreshCw className="w-4 h-4 animate-spin" /> : <X className="w-4 h-4" />}
            </button>
          </ToolbarTip>
        </div>

        <div className="p-4 flex flex-col gap-3 flex-1 overflow-hidden min-h-0">
          {/* From */}
          {accounts.length > 1 && (
            <div ref={fromRef} className="relative">
              <button
                type="button"
                onClick={() => setFromOpen(o => !o)}
                className="w-full flex items-center gap-2 px-3 py-2 rounded-lg bg-white/[0.03] border border-white/5 hover:border-white/10 transition-colors text-left"
              >
                <span className="text-[10px] text-zinc-600 shrink-0 w-10">{tr.compose.from}</span>
                <ProfileAvatar
                  picture={activeAccount?.picture}
                  email={activeAccount?.email ?? ""}
                  className="w-5 h-5 rounded-full object-cover shrink-0 text-[9px]"
                  fallbackClassName="text-white"
                  fallbackStyle={{ background: emailColor(activeAccount?.email ?? "") }}
                />
                <span className="flex-1 min-w-0 text-xs text-zinc-300 truncate">{activeAccount?.email}</span>
                <ChevronDown className={`w-3.5 h-3.5 text-zinc-500 shrink-0 transition-transform ${fromOpen ? "rotate-180" : ""}`} />
              </button>
              {fromOpen && (
                <div className="absolute left-0 right-0 top-full mt-1 z-20 bg-[var(--color-surface-popover)] border border-[var(--color-border-default)] rounded-[var(--radius-md)] shadow-xl overflow-hidden">
                  {accounts.map(acc => (
                    <button key={acc.id} type="button"
                      onClick={() => void changeComposeAccount(acc.id)}
                      className={`w-full flex items-center gap-2.5 px-3 py-2.5 hover:bg-white/5 transition-colors text-left ${acc.id === composeAccountId ? "bg-white/[0.04]" : ""}`}
                    >
                      <ProfileAvatar
                        picture={acc.picture}
                        email={acc.email}
                        className="w-7 h-7 rounded-full object-cover shrink-0 text-xs"
                        fallbackClassName="text-white"
                        fallbackStyle={{ background: emailColor(acc.email) }}
                      />
                      <div className="min-w-0 flex-1">
                        <div className="text-xs text-zinc-200 truncate">{acc.email.split("@")[0]}</div>
                        <div className="text-[10px] text-zinc-500 truncate">{acc.email}</div>
                      </div>
                      {acc.id === composeAccountId && <div className="ml-auto w-1.5 h-1.5 rounded-full bg-blue-500 shrink-0" />}
                    </button>
                  ))}
                </div>
              )}
            </div>
          )}

          {/* To */}
          <div className="relative">
            <div className="relative flex items-center">
              <label htmlFor="compose-to" className="absolute left-3 text-[10px] text-zinc-600 pointer-events-none">{tr.compose.toLabelShort}</label>
              <input
                id="compose-to"
                ref={toRef}
                value={composeTo}
                onChange={e => handleToChange(e.target.value)}
                onKeyDown={handleToKeyDown}
                onFocus={() => { if (suggestions.length > 0) setSuggOpen(true); }}
                placeholder="example@gmail.com"
                autoComplete="off"
                spellCheck={false}
                className={`${ui.input} pl-12 pr-20`}
              />
              <div className="absolute right-3 flex items-center gap-2 text-[length:var(--font-size-caption)]">
                <button
                  type="button"
                  aria-expanded={showCc}
                  aria-controls="compose-cc-field"
                  aria-label={showCc ? tr.compose.hideCc : tr.compose.showCc}
                  onClick={() => setShowCc(open => !open)}
                  className={showCc ? "text-[var(--app-accent)]" : "text-[var(--color-text-subtle)] hover:text-[var(--color-text-secondary)]"}
                >
                  {tr.compose.cc}
                </button>
                <button
                  type="button"
                  aria-expanded={showBcc}
                  aria-controls="compose-bcc-field"
                  aria-label={showBcc ? tr.compose.hideBcc : tr.compose.showBcc}
                  onClick={() => setShowBcc(open => !open)}
                  className={showBcc ? "text-[var(--app-accent)]" : "text-[var(--color-text-subtle)] hover:text-[var(--color-text-secondary)]"}
                >
                  {tr.compose.bcc}
                </button>
              </div>
            </div>
            {suggOpen && suggestions.length > 0 && (
              <div ref={suggRef} className="absolute left-0 right-0 top-full mt-1 z-20 bg-[var(--color-surface-popover)] border border-[var(--color-border-default)] rounded-[var(--radius-md)] shadow-xl overflow-hidden">
                {suggestions.map((s, i) => (
                  <button
                    key={s.email}
                    type="button"
                    onMouseDown={e => { e.preventDefault(); applySuggestion(s); }}
                    className={`w-full flex items-center gap-2.5 px-3 py-2.5 text-left transition-colors ${i === highlightIdx ? "bg-white/10" : "hover:bg-white/5"}`}
                    onMouseEnter={() => setHighlightIdx(i)}
                  >
                    <div
                      className="w-7 h-7 rounded-full flex items-center justify-center text-xs font-bold text-white shrink-0"
                      style={{ background: emailColor(s.email) }}
                    >
                      {(s.name || s.email)[0]?.toUpperCase()}
                    </div>
                    <div className="min-w-0 flex-1">
                      {s.name && <div className="text-xs text-zinc-200 truncate">{s.name}</div>}
                      <div className={`truncate ${s.name ? "text-[10px] text-zinc-500" : "text-xs text-zinc-300"}`}>{s.email}</div>
                    </div>
                  </button>
                ))}
              </div>
            )}
          </div>

          {showCc && (
            <div id="compose-cc-field" className="relative flex items-center">
              <label htmlFor="compose-cc" className="absolute left-3 text-[10px] text-zinc-600 pointer-events-none">{tr.compose.cc}</label>
              <input
                id="compose-cc"
                value={composeCc}
                onChange={event => setComposeCc(event.target.value)}
                placeholder="example@gmail.com"
                autoComplete="off"
                spellCheck={false}
                className={`${ui.input} pl-12`}
              />
            </div>
          )}

          {showBcc && (
            <div id="compose-bcc-field" className="relative flex items-center">
              <label htmlFor="compose-bcc" className="absolute left-3 text-[10px] text-zinc-600 pointer-events-none">{tr.compose.bcc}</label>
              <input
                id="compose-bcc"
                value={composeBcc}
                onChange={event => setComposeBcc(event.target.value)}
                placeholder="example@gmail.com"
                autoComplete="off"
                spellCheck={false}
                className={`${ui.input} pl-12`}
              />
            </div>
          )}

          {/* Subject */}
          <div className="relative flex items-center">
            <label htmlFor="compose-subject" className="absolute left-3 text-[10px] text-zinc-600 pointer-events-none">{tr.compose.subject}</label>
            <input
              id="compose-subject"
              value={composeSubject}
              onChange={e => setComposeSubject(e.target.value)}
              placeholder={tr.compose.subjectPlaceholder}
              className={`${ui.input} pl-12`}
            />
          </div>

          {/* Body — bordered container with contenteditable + bottom bar */}
          <div className="rounded-lg border border-white/[0.08] bg-white/[0.02] overflow-hidden flex flex-col flex-1 min-h-0">
            {/* Editable area — scrolls internally, never grows the modal */}
            <div className="relative px-3 pt-3 pb-2 flex-1 min-h-0 overflow-y-auto">
              {bodyEmpty && (
                <span className="absolute top-3 left-3 pointer-events-none text-zinc-600 text-sm select-none">
                  {tr.compose.body}
                </span>
              )}
              <div
                ref={bodyEditableRef}
                contentEditable
                role="textbox"
                aria-multiline="true"
                aria-label={tr.compose.body}
                suppressContentEditableWarning
                onPaste={handleBodyPaste}
                onInput={() => {
                  setBodyEmpty(!(bodyEditableRef.current?.innerText.trim()));
                  setComposeBody(sanitizeComposerHtml(bodyEditableRef.current?.innerHTML ?? ""));
                  syncUndoRedo();
                }}
                className="outline-none text-sm text-zinc-200 [&_a]:text-blue-400 [&_a]:underline [&_b]:font-bold [&_strong]:font-bold [&_i]:italic [&_em]:italic [&_u]:underline [&_s]:line-through [&_ol]:list-decimal [&_ol]:pl-5 [&_ul]:list-disc [&_ul]:pl-5"
                style={{ wordBreak: "break-word", minHeight: "100%", whiteSpace: "pre-wrap" }}
              />
            </div>

            {/* Attachment chips */}
            {attachments.length > 0 && (
              <div className="px-3 pb-1.5 flex flex-wrap gap-1.5 shrink-0">
                {attachments.map((att, idx) => (
                  <div key={idx} className="flex items-center gap-1.5 px-2 py-1 rounded-md bg-white/[0.04] border border-white/[0.07] text-zinc-400 max-w-[200px]">
                    <span className="shrink-0 text-zinc-500">{fileIcon(att.mimeType)}</span>
                    <span className="text-[11px] truncate min-w-0">{att.filename}</span>
                    <span className="text-[10px] text-zinc-600 shrink-0">{formatBytes(att.size)}</span>
                    <button type="button" aria-label={`${tr.compose.removeAttachment}: ${att.filename}`} onClick={() => removeAttachment(idx)} className="shrink-0 ml-0.5 text-zinc-600 hover:text-zinc-300 transition-colors">
                      <X className="w-3 h-3" />
                    </button>
                  </div>
                ))}
              </div>
            )}

            {/* Attachment error */}
            {attachError && (
              <div className="mx-3 mb-1.5 shrink-0 flex items-center gap-2 text-xs text-red-400 bg-red-400/10 border border-red-400/20 rounded-lg px-2.5 py-1.5">
                <AlertCircle className="w-3.5 h-3.5 shrink-0" />
                <span className="min-w-0">{attachError}</span>
                <button type="button" aria-label={tr.common.close} onClick={() => setAttachError(null)} className="ml-auto shrink-0 text-red-400/60 hover:text-red-400"><X className="w-3 h-3" /></button>
              </div>
            )}

            {/* Formatting toolbar */}
            {showFormatBar && (
              <div className="relative px-2 py-1 border-t border-white/[0.06] flex items-center gap-0.5 shrink-0">
                {linkPopover && (
                  <div className="absolute bottom-full left-0 mb-1 bg-[var(--color-surface-popover)] border border-[var(--color-border-default)] rounded-[var(--radius-lg)] p-3 shadow-2xl z-50 w-64">
                    <div className="flex flex-col gap-2">
                      <input autoFocus value={linkText} onChange={e => setLinkText(e.target.value)} placeholder={tr.compose.linkText}
                        className="w-full bg-white/[0.05] border border-white/10 rounded-lg px-2.5 py-1.5 text-xs text-zinc-200 outline-none focus:border-blue-500/50 placeholder:text-zinc-600" />
                      <input value={linkUrl} onChange={e => setLinkUrl(e.target.value)} placeholder="https://..."
                        className="w-full bg-white/[0.05] border border-white/10 rounded-lg px-2.5 py-1.5 text-xs text-zinc-200 outline-none focus:border-blue-500/50 placeholder:text-zinc-600"
                        onKeyDown={e => e.key === "Enter" && applyLink()} />
                      <div className="flex gap-2 justify-end pt-0.5">
                        <button type="button" onClick={() => setLinkPopover(false)} className="text-xs text-zinc-500 hover:text-zinc-300 transition-colors">{tr.common.cancel}</button>
                        <button type="button" onClick={applyLink} disabled={!linkUrl}
                          className="px-3 py-1 bg-blue-500 hover:bg-blue-600 disabled:opacity-40 text-white text-xs rounded-md transition-colors">{tr.common.apply}</button>
                      </div>
                    </div>
                  </div>
                )}
                <ToolbarTip label={tr.compose.undo}>
                  <button type="button" disabled={!canUndo} onMouseDown={e => { e.preventDefault(); applyFormat("undo"); }}
                    className={`w-7 h-7 flex items-center justify-center rounded transition-colors ${canUndo ? "text-zinc-400 hover:text-zinc-200 hover:bg-white/[0.06] cursor-pointer" : "text-zinc-700 cursor-default"}`}>
                    <Undo2 className="w-3.5 h-3.5" />
                  </button>
                </ToolbarTip>
                <ToolbarTip label={tr.compose.redo}>
                  <button type="button" disabled={!canRedo} onMouseDown={e => { e.preventDefault(); applyFormat("redo"); }}
                    className={`w-7 h-7 flex items-center justify-center rounded transition-colors ${canRedo ? "text-zinc-400 hover:text-zinc-200 hover:bg-white/[0.06] cursor-pointer" : "text-zinc-700 cursor-default"}`}>
                    <Redo2 className="w-3.5 h-3.5" />
                  </button>
                </ToolbarTip>
                <div className="w-px h-4 bg-white/10 mx-1 shrink-0" />
                {([
                  { cmd: "bold",          label: "B", cls: "font-bold",    title: tr.compose.bold },
                  { cmd: "italic",        label: "I", cls: "italic",       title: tr.compose.italic },
                  { cmd: "underline",     label: "U", cls: "underline",    title: tr.compose.underline },
                  { cmd: "strikeThrough", label: "S", cls: "line-through", title: tr.compose.strikethrough },
                ] as { cmd: string; label: string; cls: string; title: string }[]).map(({ cmd, label, cls, title }) => (
                  <ToolbarTip key={cmd} label={title}>
                    <button type="button" onMouseDown={e => { e.preventDefault(); applyFormat(cmd); }}
                      className="w-7 h-7 flex items-center justify-center rounded text-zinc-400 hover:text-zinc-200 hover:bg-white/[0.06] text-xs transition-colors">
                      <span className={cls}>{label}</span>
                    </button>
                  </ToolbarTip>
                ))}
                <div className="w-px h-4 bg-white/10 mx-1 shrink-0" />
                <ToolbarTip label={tr.compose.insertLink}>
                  <button type="button" onMouseDown={e => { e.preventDefault(); saveSelection(); setLinkUrl(""); setLinkPopover(v => !v); }}
                    className={`w-7 h-7 flex items-center justify-center rounded transition-colors ${linkPopover ? "text-blue-400 bg-blue-500/10" : "text-zinc-400 hover:text-zinc-200 hover:bg-white/[0.06]"}`}>
                    <Link2 className="w-3.5 h-3.5" />
                  </button>
                </ToolbarTip>
                <div className="w-px h-4 bg-white/10 mx-1 shrink-0" />
                <ToolbarTip label={tr.compose.numberedList}>
                  <button type="button" onMouseDown={e => { e.preventDefault(); applyFormat("insertOrderedList"); }}
                    className="w-7 h-7 flex items-center justify-center rounded text-zinc-400 hover:text-zinc-200 hover:bg-white/[0.06] transition-colors">
                    <ListOrdered className="w-3.5 h-3.5" />
                  </button>
                </ToolbarTip>
                <ToolbarTip label={tr.compose.bulletList}>
                  <button type="button" onMouseDown={e => { e.preventDefault(); applyFormat("insertUnorderedList"); }}
                    className="w-7 h-7 flex items-center justify-center rounded text-zinc-400 hover:text-zinc-200 hover:bg-white/[0.06] transition-colors">
                    <List className="w-3.5 h-3.5" />
                  </button>
                </ToolbarTip>
              </div>
            )}

            {/* Bottom bar — paperclip + format toggle */}
            <div className="px-2 py-1.5 border-t border-white/[0.06] flex items-center gap-1 shrink-0">
              <ToolbarTip label={tr.compose.attachFile}>
                <button type="button" onClick={() => fileInputRef.current?.click()}
                  className="w-7 h-7 flex items-center justify-center rounded-lg text-zinc-500 hover:text-zinc-300 hover:bg-white/[0.04] transition-colors">
                  <Paperclip className="w-3.5 h-3.5" />
                </button>
              </ToolbarTip>
              <input ref={fileInputRef} type="file" multiple className="hidden" onChange={handleFileSelect} />
              <ToolbarTip label={tr.compose.formatting}>
                <button type="button" onClick={() => { setShowFormatBar(v => !v); setLinkPopover(false); }}
                  className={`flex items-center gap-1 px-2 py-1.5 rounded-lg text-xs transition-colors ${showFormatBar ? "text-blue-400 bg-blue-500/10" : "text-zinc-500 hover:text-zinc-300 hover:bg-white/[0.04]"}`}>
                  <Type className="w-3.5 h-3.5" />
                  <ChevronDown className={`w-3 h-3 transition-transform duration-150 ${showFormatBar ? "rotate-180" : ""}`} />
                </button>
              </ToolbarTip>
            </div>
          </div>
        </div>

        {/* Footer */}
        <div className="px-4 py-3 border-t border-white/5 space-y-2">
          {(sendError || draftError) && (
            <div className="flex items-center gap-2 text-xs text-red-400 bg-red-400/10 border border-red-400/20 rounded-lg px-3 py-2">
              <AlertCircle className="w-3.5 h-3.5 shrink-0" />
              <span className="min-w-0 break-words">{sendError || draftError}</span>
            </div>
          )}
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              <button type="button" onClick={() => setConfirmDiscard(true)} disabled={draftActionPending || isSending} className="flex items-center gap-1.5 text-[length:var(--font-size-compact)] text-[var(--color-text-subtle)] hover:text-[var(--color-status-danger)] transition-colors disabled:opacity-50">
                <Trash2 className="w-3.5 h-3.5" />
                {tr.compose.deleteDraft}
              </button>
            </div>
            <button
              onClick={() => void handleSend()}
              disabled={!canSend}
              className={`px-5 py-1.5 text-white text-xs font-medium rounded-lg transition-all flex items-center gap-2 ${
                isSending
                  ? "bg-blue-500/60 cursor-not-allowed"
                  : canSend
                  ? "bg-blue-500 hover:bg-blue-600 active:scale-95"
                  : "bg-blue-500/30 cursor-not-allowed opacity-50"
              }`}
            >
              {isSending ? <RefreshCw className="w-3 h-3 animate-spin" /> : <Send className="w-3 h-3" />}
              {isSending ? tr.compose.sending : tr.compose.send}
            </button>
          </div>
        </div>
        </div>

          {supportsRemoteDrafts && <aside className="hidden md:flex w-72 shrink-0 flex-col border-l border-[var(--color-border-default)] bg-[var(--color-surface-app)]">
            <div className="px-4 py-3 border-b border-[var(--color-border-subtle)] flex items-center justify-between gap-2">
              <div>
                <div className="text-[length:var(--font-size-compact)] font-semibold text-[var(--color-text-secondary)]">{tr.compose.recentDrafts}</div>
                <div className="text-[length:var(--font-size-caption)] text-[var(--color-text-disabled)]">{activeAccount?.email}</div>
              </div>
              <button type="button" onClick={() => void startNewDraft()} className="text-[length:var(--font-size-caption)] text-[var(--app-accent)] hover:text-[var(--app-accent-hover)] transition-colors">
                {tr.compose.newDraft}
              </button>
            </div>

            <div
              className="flex-1 min-h-0 overflow-y-auto p-3 space-y-2"
              onScroll={event => {
                const element = event.currentTarget;
                if (hasMoreDrafts && !draftsLoading && element.scrollHeight - element.scrollTop - element.clientHeight < 160) {
                  void loadDrafts(false);
                }
              }}
            >
              {draftsLoading && drafts.length === 0 && (
                <div className="h-full flex items-center justify-center text-[var(--color-text-disabled)]">
                  <RefreshCw className="w-4 h-4 animate-spin" />
                </div>
              )}
              {!draftsLoading && drafts.length === 0 && (
                <div className="h-full flex flex-col items-center justify-center text-center px-4">
                  <Clock3 className="w-5 h-5 text-[var(--color-text-disabled)] mb-2" />
                  <p className="text-[length:var(--font-size-compact)] text-[var(--color-text-subtle)]">{tr.compose.noDrafts}</p>
                </div>
              )}
              {drafts.map(item => (
                <button
                  key={item.id}
                  type="button"
                  onClick={() => void openDraft(item.id)}
                  disabled={draftActionPending}
                  className={`w-full text-left rounded-[var(--radius-md)] border p-3 transition-colors disabled:opacity-60 ${item.id === draftId ? "border-[var(--app-accent)] bg-[var(--app-accent-soft)]" : "border-[var(--color-border-subtle)] bg-[var(--color-surface-subtle)] hover:bg-[var(--color-surface-hover)] hover:border-[var(--color-border-default)]"}`}
                >
                  <div className="flex items-start justify-between gap-2">
                    <span className="min-w-0 truncate text-[length:var(--font-size-compact)] font-semibold text-[var(--color-text-secondary)]">
                      {item.subject.trim() || tr.compose.noSubject}
                    </span>
                    <span className="shrink-0 text-[length:var(--font-size-micro)] text-[var(--color-text-disabled)]">{formatDraftTime(item.updatedAt)}</span>
                  </div>
                  <div className="mt-1 truncate text-[length:var(--font-size-caption)] text-[var(--color-text-subtle)]">
                    {item.to.trim() || tr.compose.noRecipient}
                  </div>
                  <div className="mt-1 truncate text-[length:var(--font-size-caption)] text-[var(--color-text-disabled)]">
                    {item.snippet.trim() || tr.compose.emptyDraft}
                  </div>
                </button>
              ))}
              {draftsLoading && drafts.length > 0 && (
                <div className="flex justify-center py-3 text-[var(--color-text-disabled)]">
                  <RefreshCw className="w-4 h-4 animate-spin" />
                </div>
              )}
            </div>
          </aside>}
      </div>

      {confirmDiscard && (
        <div className="fixed inset-0 z-[200] flex items-center justify-center bg-black/60 backdrop-blur-sm p-4" onClick={() => setConfirmDiscard(false)}>
          <div role="alertdialog" aria-modal="true" aria-labelledby="discard-draft-title" aria-describedby="discard-draft-description" className={`${ui.modal} w-full max-w-sm p-5`} onClick={event => event.stopPropagation()}>
            <div className="flex items-start gap-3">
              <div className="w-9 h-9 rounded-[var(--radius-md)] flex items-center justify-center shrink-0 bg-[var(--color-status-danger-soft)] text-[var(--color-status-danger)]">
                <Trash2 className="w-4 h-4" />
              </div>
              <div>
                <h4 id="discard-draft-title" className="text-[length:var(--font-size-body)] font-semibold text-[var(--color-text-primary)]">{tr.compose.deleteDraftTitle}</h4>
                <p id="discard-draft-description" className="mt-1 text-[length:var(--font-size-compact)] text-[var(--color-text-subtle)]">{tr.compose.deleteDraftConfirm}</p>
              </div>
            </div>
            <div className="mt-5 flex justify-end gap-2">
              <button type="button" onClick={() => setConfirmDiscard(false)} className={ui.buttonSecondary}>{tr.common.cancel}</button>
              <button type="button" onClick={() => void handleDiscard()} className="rounded-[var(--radius-md)] bg-[var(--color-action-danger)] px-4 py-2 text-[length:var(--font-size-compact)] font-semibold text-[var(--color-text-on-accent)] hover:bg-[var(--color-action-danger-hover)] transition-colors">{tr.compose.deleteDraft}</button>
            </div>
          </div>
        </div>
      )}

      {confirmSubjectless && (
        <div className="fixed inset-0 z-[200] flex items-center justify-center bg-black/60 backdrop-blur-sm p-4" onClick={() => setConfirmSubjectless(false)}>
          <div role="alertdialog" aria-modal="true" aria-labelledby="subjectless-title" aria-describedby="subjectless-description" className={`${ui.modal} w-full max-w-sm p-5`} onClick={event => event.stopPropagation()}>
            <div className="flex items-start gap-3">
              <div className="w-9 h-9 rounded-[var(--radius-md)] flex items-center justify-center shrink-0 bg-[var(--color-status-warning-soft)] text-[var(--color-status-warning)]">
                <AlertCircle className="w-4 h-4" />
              </div>
              <div>
                <h4 id="subjectless-title" className="text-[length:var(--font-size-body)] font-semibold text-[var(--color-text-primary)]">{tr.compose.sendWithoutSubjectTitle}</h4>
                <p id="subjectless-description" className="mt-1 text-[length:var(--font-size-compact)] text-[var(--color-text-subtle)]">{tr.compose.sendWithoutSubjectMessage}</p>
              </div>
            </div>
            <div className="mt-5 flex justify-end gap-2">
              <button type="button" onClick={() => setConfirmSubjectless(false)} className={ui.buttonSecondary}>{tr.common.cancel}</button>
              <button
                type="button"
                autoFocus
                onClick={() => { setConfirmSubjectless(false); void sendNow(); }}
                className="rounded-[var(--radius-md)] bg-[var(--app-accent)] px-4 py-2 text-[length:var(--font-size-compact)] font-semibold text-[var(--color-text-on-accent)] hover:bg-[var(--app-accent-hover)] transition-colors"
              >
                {tr.compose.sendWithoutSubject}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

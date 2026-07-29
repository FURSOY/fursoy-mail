import { useCallback, useEffect, useRef, useState, type Dispatch, type MutableRefObject, type SetStateAction } from "react";
import type { AppLocale } from "../i18n";
import type { Account, AttachmentPayload, EmailSummary } from "../types";
import { tauriApi } from "../tauriApi";
import { enqueueMailMutation, inboxUnreadDelta, runAuthenticatedMailAction, type MailMutationQueue } from "../mailActionState";
import { isAuthFailure } from "../utils";
import { areValidRecipients } from "../mailRecipients";
import { buildForwardBody } from "../mailCompose";

export interface ReplySendRequest {
  target: EmailSummary;
  to: string[];
  cc: string[];
  body: string;
  attachments: AttachmentPayload[];
  draftId: string | null;
  verificationMessageId: string | null;
}

export interface ConfirmModalState {
  message: string;
  onConfirm: () => void;
}

interface UseMailActionsOptions {
  locale: AppLocale;
  accounts: Account[];
  accountTokens: Record<string, string>;
  activeAccountId: string | null;
  activeTabRef: MutableRefObject<string>;
  recentlyReadRef: MutableRefObject<Set<string>>;
  mailMutationQueueRef: MutableRefObject<MailMutationQueue>;
  setEmails: Dispatch<SetStateAction<EmailSummary[]>>;
  setSelectedMail: Dispatch<SetStateAction<string | null>>;
  setThreadRefreshKey: Dispatch<SetStateAction<number>>;
  getTokenForEmail: (email: EmailSummary | undefined) => string;
  loadEmails: (tab?: string) => Promise<EmailSummary[]>;
  refreshUnreadCount: () => Promise<number>;
  adjustUnreadBadge: (accountId: string, delta: number) => void;
  refreshAccessToken: (accountId: string) => Promise<{ authenticated: boolean }>;
  upsertToken: (accountId: string, accessToken: string) => void;
  clearExpiredAccount: (accountId: string) => void;
  markAccountExpired: (accountId: string, showMessage?: boolean) => void;
  showToast: (message: string, type?: "error" | "success" | "info") => void;
}

function sameEmail(left: EmailSummary, right: EmailSummary) {
  return left.id === right.id && left.account_id === right.account_id;
}

function emailKey(email: EmailSummary) {
  return `${email.account_id}\u0000${email.id}`;
}

function actionFailureMessage(summary: string, error: unknown) {
  const detail = (error instanceof Error ? error.message : String(error))
    .replace(/^Error:\s*/i, "")
    .trim();
  return detail ? `${summary}: ${detail.slice(0, 180)}` : summary;
}

const SENT_VERIFICATION_DELAYS_MS = [0, 1_500, 3_000, 6_000, 10_000];

function waitForVerificationDelay(delay: number, signal: AbortSignal): Promise<boolean> {
  if (signal.aborted) return Promise.resolve(false);
  return new Promise(resolve => {
    const timer = window.setTimeout(() => {
      signal.removeEventListener("abort", cancel);
      resolve(true);
    }, delay);
    const cancel = () => {
      window.clearTimeout(timer);
      resolve(false);
    };
    signal.addEventListener("abort", cancel, { once: true });
  });
}

async function verifyUncertainSend(accountId: string, messageId: string, signal: AbortSignal) {
  for (const delay of SENT_VERIFICATION_DELAYS_MS) {
    if (delay > 0 && !await waitForVerificationDelay(delay, signal)) return false;
    if (signal.aborted) return false;
    try {
      if (await tauriApi.verifySentMessage(accountId, messageId)) return true;
    } catch {
      // A verification request may fail transiently. Keep the send locked and
      // exhaust the bounded checks; never turn this into an automatic resend.
    }
  }
  return false;
}

export function useMailActions(options: UseMailActionsOptions) {
  const {
    locale, accounts, accountTokens, activeAccountId,
    activeTabRef, recentlyReadRef, mailMutationQueueRef, setEmails, setSelectedMail,
    setThreadRefreshKey, getTokenForEmail, loadEmails, refreshUnreadCount,
    adjustUnreadBadge, refreshAccessToken, upsertToken, clearExpiredAccount,
    markAccountExpired, showToast,
  } = options;

  const [showReply, setShowReply] = useState(false);
  const [replyTarget, setReplyTarget] = useState<EmailSummary | null>(null);
  const [replyMode, setReplyMode] = useState<"reply" | "reply-all">("reply");
  const [replyText, setReplyText] = useState("");
  const [isSending, setIsSending] = useState(false);
  const [showCompose, setShowCompose] = useState(false);
  const [confirmModal, setConfirmModal] = useState<ConfirmModalState | null>(null);
  const [composeTo, setComposeTo] = useState("");
  const [composeSubject, setComposeSubject] = useState("");
  const [composeBody, setComposeBody] = useState("");
  const [composeHtmlAppend, setComposeHtmlAppend] = useState("");
  const [composeAccountId, setComposeAccountId] = useState<string | null>(null);
  const [composeSendError, setComposeSendError] = useState<string | null>(null);
  const verificationAbortRef = useRef(new AbortController());

  useEffect(() => {
    const controller = new AbortController();
    verificationAbortRef.current = controller;
    return () => controller.abort();
  }, []);

  const runAuthenticatedAction = useCallback(async (
    mail: EmailSummary,
    action: (accessToken: string) => Promise<void>,
  ) => {
    const currentToken = getTokenForEmail(mail);
    await runAuthenticatedMailAction({
      accountId: mail.account_id,
      currentToken,
      reloginRequiredMessage: locale.messages.reloginRequired,
      action,
      refreshAccessToken,
      upsertToken,
      clearExpiredAccount,
      markAccountExpired,
    });
  }, [
    clearExpiredAccount, getTokenForEmail, locale, markAccountExpired,
    refreshAccessToken, upsertToken,
  ]);

  const handleArchive = useCallback(async (mail: EmailSummary) => {
    if (!getTokenForEmail(mail)) return;
    const unreadDelta = inboxUnreadDelta(mail, "archive");
    if (unreadDelta) adjustUnreadBadge(mail.account_id, unreadDelta);
    setEmails(previous => previous.map(email => sameEmail(email, mail) ? { ...email, label: "archive" } : email));
    setSelectedMail(null);
    try {
      await runAuthenticatedAction(mail, () => tauriApi.archiveEmail(mail.account_id, mail.thread_id || mail.id));
      await loadEmails(activeTabRef.current);
      await refreshUnreadCount();
    } catch (error) {
      if (unreadDelta) adjustUnreadBadge(mail.account_id, -unreadDelta);
      console.error("Archive email failed:", error);
      showToast(actionFailureMessage(locale.messages.archiveFailed, error), "error");
      void loadEmails(activeTabRef.current);
    }
  }, [activeTabRef, adjustUnreadBadge, getTokenForEmail, loadEmails, locale, refreshUnreadCount, runAuthenticatedAction, setEmails, setSelectedMail, showToast]);

  const handleTrash = useCallback(async (mail: EmailSummary) => {
    if (!getTokenForEmail(mail)) return;
    const unreadDelta = inboxUnreadDelta(mail, "trash");
    if (unreadDelta) adjustUnreadBadge(mail.account_id, unreadDelta);
    setEmails(previous => previous.map(email => sameEmail(email, mail) ? { ...email, label: "trash" } : email));
    setSelectedMail(null);
    try {
      await runAuthenticatedAction(mail, () => tauriApi.trashEmail(mail.account_id, mail.thread_id || mail.id));
      await loadEmails(activeTabRef.current);
      await refreshUnreadCount();
    } catch (error) {
      if (unreadDelta) adjustUnreadBadge(mail.account_id, -unreadDelta);
      console.error("Trash email failed:", error);
      showToast(actionFailureMessage(locale.messages.deleteFailed, error), "error");
      void loadEmails(activeTabRef.current);
    }
  }, [activeTabRef, adjustUnreadBadge, getTokenForEmail, loadEmails, locale, refreshUnreadCount, runAuthenticatedAction, setEmails, setSelectedMail, showToast]);

  const handleReportSpam = useCallback(async (mail: EmailSummary) => {
    if (!getTokenForEmail(mail)) return;
    const unreadDelta = inboxUnreadDelta(mail, "spam");
    if (unreadDelta) adjustUnreadBadge(mail.account_id, unreadDelta);
    setEmails(previous => previous.map(email => sameEmail(email, mail) ? { ...email, label: "spam" } : email));
    setSelectedMail(null);
    try {
      await runAuthenticatedAction(mail, () => tauriApi.reportSpam(mail.account_id, mail.thread_id || mail.id));
      await loadEmails(activeTabRef.current);
      await refreshUnreadCount();
    } catch (error) {
      if (unreadDelta) adjustUnreadBadge(mail.account_id, -unreadDelta);
      console.error("Report spam failed:", error);
      showToast(actionFailureMessage(locale.messages.spamReportFailed, error), "error");
      void loadEmails(activeTabRef.current);
    }
  }, [activeTabRef, adjustUnreadBadge, getTokenForEmail, loadEmails, locale, refreshUnreadCount, runAuthenticatedAction, setEmails, setSelectedMail, showToast]);

  const handleMoveToInbox = useCallback(async (mail: EmailSummary) => {
    if (!getTokenForEmail(mail)) return;
    const unreadDelta = inboxUnreadDelta(mail, "inbox");
    if (unreadDelta) adjustUnreadBadge(mail.account_id, unreadDelta);
    setEmails(previous => previous.filter(email => !sameEmail(email, mail)));
    setSelectedMail(null);
    try {
      await runAuthenticatedAction(mail, () => tauriApi.moveToInbox(mail.account_id, mail.thread_id || mail.id));
      showToast(locale.messages.movedToInbox, "success");
      void loadEmails(activeTabRef.current);
      void refreshUnreadCount();
    } catch (error) {
      if (unreadDelta) adjustUnreadBadge(mail.account_id, -unreadDelta);
      console.error("Move email to inbox failed:", error);
      showToast(actionFailureMessage(locale.messages.moveFailed, error), "error");
      void loadEmails(activeTabRef.current);
    }
  }, [activeTabRef, adjustUnreadBadge, getTokenForEmail, loadEmails, locale, refreshUnreadCount, runAuthenticatedAction, setEmails, setSelectedMail, showToast]);

  const handleReply = useCallback(async (request: ReplySendRequest): Promise<boolean> => {
    const { target, to, cc, body, attachments, draftId, verificationMessageId } = request;
    if ((!body.trim() && attachments.length === 0) || !areValidRecipients([...to, ...cc])) {
      showToast(locale.messages.replySendFailed, "error");
      return false;
    }
    const accessToken = getTokenForEmail(target);
    if (!accessToken) return false;
    setIsSending(true);
    try {
      const outcome = draftId && verificationMessageId
        ? await tauriApi.sendDraft(target.account_id, draftId, verificationMessageId)
        : await tauriApi.sendReply({
            accountId: target.account_id,
            to: to.join(", "),
            cc: cc.join(", "),
            subject: target.subject,
            body,
            threadId: target.thread_id || target.id,
            inReplyTo: target.message_id,
            references: target.references,
            attachments: attachments.length > 0 ? attachments : null,
          });
      if (outcome.status === "outcome_unknown") {
        showToast(locale.messages.sendOutcomeUnknown, "info");
        const verified = await verifyUncertainSend(target.account_id, outcome.messageId, verificationAbortRef.current.signal);
        if (!verified) {
          showToast(locale.messages.sendOutcomeUnresolved, "error");
          return false;
        }
        showToast(locale.messages.sendOutcomeVerified, "success");
      }
      setReplyText("");
      setShowReply(false);
      setReplyTarget(null);
      setThreadRefreshKey(current => current + 1);
      return true;
    } catch {
      showToast(locale.messages.replySendFailed, "error");
      return false;
    } finally {
      setIsSending(false);
    }
  }, [getTokenForEmail, locale, setThreadRefreshKey, showToast]);

  const handleComposeSend = useCallback(async (
    cc: string,
    bcc: string,
    attachments: AttachmentPayload[],
    body: string,
    draftId: string | null,
    verificationMessageId: string | null,
  ): Promise<boolean> => {
    if (!composeTo.trim()) return false;
    const sendFromId = composeAccountId ?? activeAccountId ?? accounts[0]?.id;
    if (!sendFromId) {
      setComposeSendError(locale.messages.noSendAccount);
      return false;
    }
    setComposeSendError(null);
    setIsSending(true);
    let token = accountTokens[sendFromId];
    if (!token) {
      try {
        const refreshed = await refreshAccessToken(sendFromId);
        if (!refreshed.authenticated) throw new Error(locale.messages.reloginRequired);
        token = "active";
        upsertToken(sendFromId, token);
        clearExpiredAccount(sendFromId);
      } catch {
        setComposeSendError(locale.messages.reloginRequired);
        setIsSending(false);
        return false;
      }
    }
    try {
      const outcome = draftId && verificationMessageId
        ? await tauriApi.sendDraft(sendFromId, draftId, verificationMessageId)
        : await tauriApi.sendEmail({
            accountId: sendFromId,
            to: composeTo,
            cc,
            bcc,
            subject: composeSubject,
            body: body + composeHtmlAppend,
            attachments: attachments.length > 0 ? attachments : null,
          });
      if (outcome.status === "outcome_unknown") {
        setComposeSendError(locale.messages.sendOutcomeUnknown);
        showToast(locale.messages.sendOutcomeUnknown, "info");
        const verified = await verifyUncertainSend(sendFromId, outcome.messageId, verificationAbortRef.current.signal);
        if (!verified) {
          setComposeSendError(locale.messages.sendOutcomeUnresolved);
          showToast(locale.messages.sendOutcomeUnresolved, "error");
          return false;
        }
      }
      setShowCompose(false);
      setComposeTo("");
      setComposeSubject("");
      setComposeBody("");
      setComposeHtmlAppend("");
      setComposeSendError(null);
      showToast(
        outcome.status === "outcome_unknown"
          ? locale.messages.sendOutcomeVerified
          : locale.messages.emailSent,
        "success",
      );
      return true;
    } catch (error) {
      const raw = String(error);
      if (isAuthFailure(raw)) {
        markAccountExpired(sendFromId);
        setComposeSendError(locale.messages.reloginRequired);
      } else {
        const message = raw.replace(/^Error:\s*/i, "").replace(/Gmail send error:\s*/i, "");
        setComposeSendError(message || locale.messages.sendFailed);
      }
      return false;
    } finally {
      setIsSending(false);
    }
  }, [
    accountTokens, accounts, activeAccountId, clearExpiredAccount, composeAccountId,
    composeHtmlAppend, composeSubject, composeTo, locale, markAccountExpired,
    refreshAccessToken, showToast, upsertToken,
  ]);

  const handleMarkAsUnread = useCallback(async (mail: EmailSummary) => {
    if (!getTokenForEmail(mail)) return;
    const unreadDelta = mail.unread ? 0 : 1;
    recentlyReadRef.current.delete(emailKey(mail));
    setEmails(previous => previous.map(email => sameEmail(email, mail) ? { ...email, unread: true } : email));
    if (unreadDelta) adjustUnreadBadge(mail.account_id, unreadDelta);
    // Leave the reader before refreshing the thread. An open reader marks its
    // loaded messages as read, which can otherwise immediately undo this action.
    setSelectedMail(null);
    try {
      await enqueueMailMutation(
        mailMutationQueueRef.current,
        emailKey(mail),
        () => runAuthenticatedAction(mail, () => tauriApi.markAsUnread(mail.account_id, mail.thread_id || mail.id)),
      );
      await loadEmails(activeTabRef.current);
      await refreshUnreadCount();
    } catch (error) {
      console.error("Mark email as unread failed:", error);
      if (unreadDelta) adjustUnreadBadge(mail.account_id, -unreadDelta);
      showToast(actionFailureMessage(locale.messages.operationFailed, error), "error");
      void loadEmails(activeTabRef.current);
    }
  }, [activeTabRef, adjustUnreadBadge, getTokenForEmail, loadEmails, locale, recentlyReadRef, refreshUnreadCount, runAuthenticatedAction, setEmails, setSelectedMail, showToast]);

  const handleForward = useCallback(async (mail: EmailSummary) => {
    const exactBody = await tauriApi.getEmailBody(mail.id, mail.account_id).catch(() => mail.snippet);
    setComposeTo("");
    setComposeSubject(`Fwd: ${mail.subject.replace(/^(Fwd:\s*)+/i, "")}`);
    setComposeBody("");
    setComposeHtmlAppend(buildForwardBody(mail, exactBody, {
      forwardedMessage: locale.compose.forwardedMessage,
      sender: locale.compose.senderLabel,
      subject: locale.compose.subject,
      date: locale.compose.dateLabel,
    }));
    setComposeAccountId(mail.account_id ?? activeAccountId ?? accounts[0]?.id ?? null);
    setShowCompose(true);
  }, [accounts, activeAccountId, locale]);

  return {
    showReply, setShowReply, replyTarget, setReplyTarget, replyMode, setReplyMode, replyText, setReplyText,
    isSending, showCompose, setShowCompose, confirmModal, setConfirmModal,
    composeTo, setComposeTo, composeSubject, setComposeSubject, composeBody, setComposeBody,
    composeHtmlAppend, setComposeHtmlAppend, composeAccountId, setComposeAccountId,
    composeSendError, setComposeSendError,
    handleArchive, handleTrash, handleReportSpam, handleMoveToInbox,
    handleReply, handleComposeSend, handleMarkAsUnread, handleForward,
  };
}

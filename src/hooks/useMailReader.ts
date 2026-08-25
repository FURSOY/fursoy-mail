import { useEffect, useRef, useState, type Dispatch, type MutableRefObject, type RefObject, type SetStateAction } from "react";
import type { AppLocale } from "../i18n";
import type { EmailSummary } from "../types";
import { tauriApi } from "../tauriApi";
import { enqueueMailMutation, type MailMutationQueue } from "../mailActionState";
import { addBoundedSetValue, MAX_RECENTLY_READ_EMAILS } from "../boundedSet";

const THREAD_PAGE_SIZE = 20;
const MAX_THREAD_EMAILS = 200;

interface UseMailReaderOptions {
  selectedMail: string | null;
  activeMail: EmailSummary | undefined;
  activeMailKey: string | null;
  locale: AppLocale;
  mailScrollRef: RefObject<HTMLDivElement | null>;
  recentlyReadRef: MutableRefObject<Set<string>>;
  mailMutationQueueRef: MutableRefObject<MailMutationQueue>;
  setEmails: Dispatch<SetStateAction<EmailSummary[]>>;
  setSearchResults: Dispatch<SetStateAction<EmailSummary[] | null>>;
  setReadingToolsOpen: Dispatch<SetStateAction<boolean>>;
  getTokenForEmail: (email: EmailSummary | undefined) => string;
  adjustUnreadBadge: (accountId: string, delta: number) => void;
}

function sameEmail(left: EmailSummary, right: EmailSummary) {
  return left.id === right.id && left.account_id === right.account_id;
}

function emailKey(email: EmailSummary) {
  return `${email.account_id}\u0000${email.id}`;
}

export function useMailReader(options: UseMailReaderOptions) {
  const {
    selectedMail, activeMail, activeMailKey, locale, mailScrollRef, recentlyReadRef, mailMutationQueueRef,
    setEmails, setSearchResults, setReadingToolsOpen, getTokenForEmail, adjustUnreadBadge,
  } = options;
  const [selectedMailBody, setSelectedMailBody] = useState("");
  const [selectedMailBodyId, setSelectedMailBodyId] = useState<string | null>(null);
  const [isBodyLoading, setIsBodyLoading] = useState(false);
  const [bodyError, setBodyError] = useState<string | null>(null);
  const [threadEmails, setThreadEmails] = useState<EmailSummary[]>([]);
  const [threadRefreshKey, setThreadRefreshKey] = useState(0);
  const [hasMoreThreadEmails, setHasMoreThreadEmails] = useState(false);
  const [isLoadingOlderThread, setIsLoadingOlderThread] = useState(false);
  const [threadMemoryLimitReached, setThreadMemoryLimitReached] = useState(false);
  const threadOffsetRef = useRef(0);
  const threadRequestIdRef = useRef(0);

  useEffect(() => {
    let cancelled = false;
    setSelectedMailBody("");
    setSelectedMailBodyId(null);
    setBodyError(null);
    setIsBodyLoading(false);
    setReadingToolsOpen(false);
    if (mailScrollRef.current) mailScrollRef.current.scrollTop = 0;
    if (!selectedMail || !activeMail) return;
    setIsBodyLoading(true);
    tauriApi.getEmailBody(activeMail.id, activeMail.account_id)
      .then(body => {
        if (cancelled) return;
        setSelectedMailBody(body || "");
        setSelectedMailBodyId(selectedMail);
      })
      .catch(error => {
        if (cancelled) return;
        console.error("Failed to load email body:", error);
        setBodyError(locale.mail.bodyLoadFailed);
      })
      .finally(() => { if (!cancelled) setIsBodyLoading(false); });
    return () => { cancelled = true; };
  }, [selectedMail, activeMailKey]);

  useEffect(() => {
    if (!activeMail || !activeMailKey) return;
    const token = getTokenForEmail(activeMail);
    if (!token) return;
    let cancelled = false;
    void tauriApi.getEmailBody(activeMail.id, activeMail.account_id)
      .then(body => {
        if (cancelled || selectedMail !== activeMailKey) return;
        setSelectedMailBody(body || "");
        setSelectedMailBodyId(activeMailKey);
      })
      .catch(() => {
        // The locally cached message remains usable when this read fails.
      });
    return () => { cancelled = true; };
  }, [activeMailKey]);

  const selectedMailThreadId = activeMail?.thread_id;

  const markThreadEmailsAsRead = (loadedEmails: EmailSummary[]) => {
    for (const email of loadedEmails) {
      if (!email.unread || recentlyReadRef.current.has(emailKey(email))) continue;
      addBoundedSetValue(recentlyReadRef.current, emailKey(email), MAX_RECENTLY_READ_EMAILS);
      setEmails(previous => previous.map(item => sameEmail(item, email) ? { ...item, unread: false } : item));
      setSearchResults(previous => previous?.map(item => sameEmail(item, email) ? { ...item, unread: false } : item) ?? null);
      adjustUnreadBadge(email.account_id, -1);
      const token = getTokenForEmail(email);
      if (!token) continue;
      enqueueMailMutation(
        mailMutationQueueRef.current,
        emailKey(email),
        () => tauriApi.markAsRead(email.account_id, email.id),
      ).then(() => {
        recentlyReadRef.current.delete(emailKey(email));
      }).catch(error => {
        console.error("Failed to mark thread email as read:", error);
        if (!recentlyReadRef.current.has(emailKey(email))) return;
        recentlyReadRef.current.delete(emailKey(email));
        setEmails(previous => previous.map(item => sameEmail(item, email) ? { ...item, unread: true } : item));
        setSearchResults(previous => previous?.map(item => sameEmail(item, email) ? { ...item, unread: true } : item) ?? null);
        setThreadEmails(previous => previous.map(item => sameEmail(item, email) ? { ...item, unread: true } : item));
        adjustUnreadBadge(email.account_id, 1);
      });
    }
  };

  useEffect(() => {
    if (!selectedMail || !selectedMailThreadId || !activeMail) {
      setThreadEmails([]);
      setHasMoreThreadEmails(false);
      setThreadMemoryLimitReached(false);
      return;
    }
    let cancelled = false;
    const requestId = ++threadRequestIdRef.current;
    setIsLoadingOlderThread(false);
    setThreadMemoryLimitReached(false);
    tauriApi.getThreadEmails(selectedMailThreadId, activeMail.account_id, THREAD_PAGE_SIZE + 1, 0)
      .then(all => {
        if (cancelled || requestId !== threadRequestIdRef.current) return;
        const page = all.slice(0, THREAD_PAGE_SIZE).reverse();
        threadOffsetRef.current = page.length;
        const withActive = page.some(email => sameEmail(email, activeMail))
          ? page
          : [...page, activeMail].sort((left, right) => left.date - right.date);
        setThreadEmails(withActive);
        setHasMoreThreadEmails(all.length > THREAD_PAGE_SIZE);
        markThreadEmailsAsRead(withActive);
      })
      .catch(() => { if (!cancelled) setThreadEmails([]); });
    return () => { cancelled = true; };
  }, [selectedMail, selectedMailThreadId, threadRefreshKey]);

  const loadOlderThreadEmails = async () => {
    if (!selectedMailThreadId || !activeMail || isLoadingOlderThread || !hasMoreThreadEmails || threadMemoryLimitReached) return;
    const requestId = threadRequestIdRef.current;
    setIsLoadingOlderThread(true);
    try {
      const all = await tauriApi.getThreadEmails(
        selectedMailThreadId,
        activeMail.account_id,
        THREAD_PAGE_SIZE + 1,
        threadOffsetRef.current,
      );
      if (requestId !== threadRequestIdRef.current) return;
      const page = all.slice(0, THREAD_PAGE_SIZE).reverse();
      threadOffsetRef.current += page.length;
      const seen = new Set(threadEmails.map(emailKey));
      const available = Math.max(0, MAX_THREAD_EMAILS - threadEmails.length);
      const candidates = page.filter(email => !seen.has(emailKey(email)));
      const accepted = available > 0 ? candidates.slice(-available) : [];
      setThreadEmails(previous => [...accepted, ...previous].sort((left, right) => left.date - right.date));
      markThreadEmailsAsRead(accepted);
      const reachedLimit = threadEmails.length + accepted.length >= MAX_THREAD_EMAILS;
      setThreadMemoryLimitReached(reachedLimit);
      setHasMoreThreadEmails(!reachedLimit && all.length > THREAD_PAGE_SIZE);
    } finally {
      if (requestId === threadRequestIdRef.current) setIsLoadingOlderThread(false);
    }
  };

  return {
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
  };
}

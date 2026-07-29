import type { EmailSummary } from "./types";
import { escapeHtml, formatDateFull, sanitizeComposerHtml } from "./utils";

export function buildReplyBody(
  target: EmailSummary,
  authoredBody: string,
  targetBody: string,
  wroteOnTemplate: string,
): string {
  const heading = wroteOnTemplate
    .replace("{date}", escapeHtml(formatDateFull(target.date)))
    .replace("{sender}", `<b>${escapeHtml(target.sender)}</b>`);
  return `${sanitizeComposerHtml(authoredBody)}<br/><br/><blockquote><div>${heading}</div>${sanitizeComposerHtml(targetBody || target.snippet)}</blockquote>`;
}

export function buildForwardBody(
  target: EmailSummary,
  targetBody: string,
  labels: { forwardedMessage: string; sender: string; subject: string; date: string },
): string {
  const header = `<br/><br/><div><b>---------- ${escapeHtml(labels.forwardedMessage)} ----------</b><br/>${escapeHtml(labels.sender)}: ${escapeHtml(target.sender)}<br/>${escapeHtml(labels.subject)}: ${escapeHtml(target.subject)}<br/>${escapeHtml(labels.date)}: ${escapeHtml(formatDateFull(target.date))}<br/><br/></div>`;
  return header + sanitizeComposerHtml(targetBody || target.snippet);
}

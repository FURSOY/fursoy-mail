import type { OtpMode, RenderMode, MailZoom, MailViewMode, AppControls } from "./types";
import { type ThemePresetName, themePresets } from "./theme";
import type { AppLanguage } from "./i18n";

export const LARGE_BODY_RENDER_LIMIT = 4_000_000;
export const MAX_INLINE_DATA_URI = 4_000_000;
export const FIXED_LAYOUT_MIN_WIDTH = 460;
export const IMAGE_PROXY_BASE = "http://mailimg.localhost/?url=";
export const MAX_MAIL_LIST_CACHE_ENTRIES = 20;
export const MAIL_PAGE_SIZE = 100;
export const STARTUP_NETWORK_DELAY_MS = 5000;
export const STARTUP_UPDATE_DELAY_MS = 9000;
export const MAIL_TABS = new Set(["inbox", "starred", "all", "sent", "archive", "spam", "trash"]);
export const ZOOM_STEPS = [0.5, 0.6, 0.7, 0.8, 0.9, 1, 1.25, 1.5, 1.75, 2];
export const MIN_ZOOM = ZOOM_STEPS[0];
export const MAX_ZOOM = ZOOM_STEPS[ZOOM_STEPS.length - 1];

/**
 * Whether a tab shows a list of mail. The fixed system tabs are only part of
 * it: a custom IMAP folder and a label each have their own list too, and code
 * that checks the fixed set alone silently stops working the moment one of
 * those is open.
 */
export function isMailListTab(tab: string): boolean {
  return MAIL_TABS.has(tab) || tab.startsWith("gmail:") || tab.startsWith("custom:");
}

export function isNoUpdateError(error: unknown): boolean {
  const message = (error instanceof Error ? error.message : String(error)).toLowerCase();
  return /no update|not available|up to date|guncel|güncel|204/.test(message);
}

export function isAuthFailure(error: unknown): boolean {
  const message = (error instanceof Error ? error.message : String(error)).toLowerCase();
  return /401|unauthorized|invalid_grant|invalid credentials|unauthenticated|autherror|expected oauth 2 access token|no refresh token|no session found|mail_account_auth_failed|mail_oauth_token_failed|mail_oauth_refresh_revoked|mail_oauth_refresh_token_missing|session expired|oturum yenilenemedi|oturum bilgisi bulunamad/.test(message);
}

/**
 * Whether the stored credential itself is gone, as opposed to a refresh that
 * could not be completed right now. Only the first is worth sending the user
 * back to the sign-in screen: a token endpoint that was unreachable, throttled,
 * or briefly broken leaves a perfectly good session behind, and treating that
 * as an expiry is what makes an app ask for a password it never needed.
 */
export function isSessionRevoked(error: unknown): boolean {
  const message = (error instanceof Error ? error.message : String(error)).toLowerCase();
  return /invalid_grant|mail_oauth_refresh_revoked|mail_oauth_refresh_token_missing|no refresh token|no session found|oturum bilgisi bulunamad/.test(message);
}

export function byteLength(text: string): number {
  return new Blob([text]).size;
}

export function decodeBasicHtmlEntities(html: string): string {
  const codeToChar = (code: number) => {
    if (!Number.isFinite(code) || code < 1 || code > 0x10ffff) return " ";
    try {
      return String.fromCodePoint(code);
    } catch {
      return " ";
    }
  };
  return html
    .replace(/&nbsp;/gi, " ")
    .replace(/&amp;#(\d+);/gi, (_, n) => codeToChar(Number(n)))
    .replace(/&amp;#x([0-9a-f]+);/gi, (_, h) => codeToChar(parseInt(h, 16)))
    .replace(/&#(\d+);/g, (_, n) => codeToChar(Number(n)))
    .replace(/&#x([0-9a-f]+);/gi, (_, h) => codeToChar(parseInt(h, 16)))
    .replace(/&amp;/gi, "&")
    .replace(/&lt;/gi, "<")
    .replace(/&gt;/gi, ">")
    .replace(/&quot;/gi, '"')
    .replace(/&#39;/g, "'");
}

export function stripHtml(html: string): string {
  const decoded = decodeBasicHtmlEntities(html);
  return decoded
    .replace(/<script[\s\S]*?<\/script>/gi, " ")
    .replace(/<style[\s\S]*?<\/style>/gi, " ")
    .replace(/<[^>]+>/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

export function escapeHtml(text: string): string {
  return decodeBasicHtmlEntities(text)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

const HTML_CONTENT_RE = /<(?:!doctype|html|head|body|style|table|tbody|thead|tr|td|th|div|p|br|span|a|img|ul|ol|li|blockquote|h[1-6]|font|section|article)\b/i;

export function isHtmlEmailBody(value: string): boolean {
  return HTML_CONTENT_RE.test(value);
}

export function htmlToReadablePlainText(html: string): string {
  const withLinkTargets = html
    .replace(/<script\b[\s\S]*?<\/script>/gi, "")
    .replace(/<style\b[\s\S]*?<\/style>/gi, "")
    .replace(/<img\b[^>]*>/gi, "")
    .replace(/<a\b[^>]*href\s*=\s*(["'])(https?:\/\/[^"']+)\1[^>]*>([\s\S]*?)<\/a>/gi, (_match, _quote, url, label) => {
      const text = stripHtml(label);
      return text && text !== url ? `${text} (${url})` : url;
    })
    .replace(/<br\s*\/?>/gi, "\n")
    .replace(/<li\b[^>]*>/gi, "\n• ")
    .replace(/<\/(?:p|div|li|tr|table|blockquote|section|article|h[1-6])\s*>/gi, "\n")
    .replace(/<[^>]+>/g, " ");

  return decodeBasicHtmlEntities(withLinkTargets)
    .replace(/\r\n?/g, "\n")
    .split("\n")
    .map(line => line.replace(/[\t ]+/g, " ").trim())
    .join("\n")
    .replace(/\n[\t ]+\n/g, "\n\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

export function linkifyPlainText(text: string): string {
  const pattern = /\[([^\]\r\n]+)\]\((https?:\/\/[^\s)]+)\)|(https?:\/\/[^\s<>"'\])]+)|([A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,})/gi;
  let result = "";
  let cursor = 0;
  for (const match of text.matchAll(pattern)) {
    const index = match.index ?? 0;
    result += escapeHtml(text.slice(cursor, index));
    const markdownLabel = match[1];
    const url = match[2] || match[3];
    const email = match[4];
    const href = email ? `mailto:${email}` : url;
    const label = markdownLabel || email || url;
    result += `<a href="${escapeHtml(href)}">${escapeHtml(label)}</a>`;
    cursor = index + match[0].length;
  }
  return result + escapeHtml(text.slice(cursor));
}

export function renderPlainTextEmail(text: string): string {
  const normalized = decodeBasicHtmlEntities(text || "")
    .replace(/\r\n?/g, "\n")
    .replace(/\n{4,}/g, "\n\n\n")
    .trim();
  return `<div class="plain-text">${linkifyPlainText(normalized)}</div>`;
}

export function isSimpleDocumentHtml(html: string): boolean {
  if (/<(?:style|link|img|picture|svg|video|audio|table|form|input|button|canvas)\b/i.test(html)) {
    return false;
  }
  if (/\b(?:display|position|float|grid|flex|width|height|font-family|font-size|background-image)\s*:/i.test(html)) {
    return false;
  }
  const tags = Array.from(html.matchAll(/<\/?([a-z][a-z0-9-]*)\b/gi), match => match[1].toLowerCase());
  const documentTags = new Set([
    "a", "b", "blockquote", "br", "code", "del", "div", "em", "i", "li",
    "ol", "p", "pre", "s", "span", "strong", "u", "ul",
  ]);
  return tags.length > 0 && tags.every(tag => documentTags.has(tag));
}

export function renderSimpleDocumentHtml(html: string): string {
  const safe = sanitizeComposerHtml(html)
    .replace(
      /\[<a\b[^>]*>(https?:\/\/[^\]<]+)\]\((https?:\/\/[^)<]+)\)<\/a>/gi,
      (_match, label, url) => `<a href="${escapeHtml(url)}">${escapeHtml(label)}</a>`,
    )
    .replace(
      /\[<a\b[^>]*>(https?:\/\/[^<]+)<\/a>\]\((https?:\/\/[^)<]+)\)/gi,
      (_match, label, url) => `<a href="${escapeHtml(url)}">${escapeHtml(label)}</a>`,
    );
  return `<div class="simple-document">${safe}</div>`;
}

const COMPOSER_ALLOWED_TAGS = new Set([
  "A", "B", "BLOCKQUOTE", "BR", "DIV", "EM", "I", "LI",
  "OL", "P", "S", "SPAN", "STRONG", "U", "UL",
]);
const COMPOSER_DROP_CONTENT_TAGS = new Set([
  "BASE", "EMBED", "FORM", "IFRAME", "LINK", "META", "OBJECT",
  "SCRIPT", "STYLE", "SVG", "TEMPLATE",
]);

export function normalizeComposerLinkUrl(rawUrl: string): string | null {
  const trimmed = rawUrl.trim();
  if (!trimmed || /[\u0000-\u001f\u007f]/.test(trimmed)) return null;
  const candidate = /^[a-z][a-z0-9+.-]*:/i.test(trimmed)
    ? trimmed
    : `https://${trimmed}`;
  try {
    const parsed = new URL(candidate);
    return /^(https?:|mailto:|tel:)$/i.test(parsed.protocol) ? parsed.href : null;
  } catch {
    return null;
  }
}

/**
 * Produces the small, inert HTML subset accepted by the privileged compose
 * editor. Received email HTML must never be copied into the main WebView DOM
 * without passing through this boundary.
 */
export function sanitizeComposerHtml(html: string): string {
  if (!html) return "";
  if (typeof DOMParser === "undefined") {
    return escapeHtml(stripHtml(html)).replace(/\n/g, "<br/>");
  }

  const doc = new DOMParser().parseFromString(html, "text/html");
  for (const element of Array.from(doc.body.querySelectorAll("*"))) {
    if (COMPOSER_DROP_CONTENT_TAGS.has(element.tagName)) {
      element.remove();
      continue;
    }
    if (!COMPOSER_ALLOWED_TAGS.has(element.tagName)) {
      element.replaceWith(...Array.from(element.childNodes));
      continue;
    }

    const href = element.tagName === "A"
      ? normalizeComposerLinkUrl(element.getAttribute("href") ?? "")
      : null;
    for (const attribute of Array.from(element.attributes)) {
      element.removeAttribute(attribute.name);
    }
    if (element.tagName === "A" && href) {
      element.setAttribute("href", href);
    } else if (element.tagName === "A") {
      element.replaceWith(...Array.from(element.childNodes));
    }
  }
  return doc.body.innerHTML;
}

export function sanitizeEmailHtml(html: string, fallback: string): string {
  const source = (html || "").trim();
  if (!source) {
    return `<div class="plain-text">${escapeHtml(fallback || "").replace(/\n/g, "<br/>")}</div>`;
  }

  const styles = (source.match(/<style\b[^>]*>[\s\S]*?<\/style>/gi) || []).join("\n");
  const cleaned = source
    .replace(/<!doctype[\s\S]*?>/gi, "")
    .replace(/<html\b[^>]*>/gi, "")
    .replace(/<\/html>/gi, "")
    .replace(/<head\b[\s\S]*?<\/head>/gi, "")
    .replace(/<meta\b[^>]*>/gi, "")
    .replace(/<base\b[^>]*>/gi, "")
    .replace(/<script\b[^<]*(?:(?!<\/script>)<[^<]*)*<\/script>/gi, "")
    .replace(/<iframe\b[\s\S]*?<\/iframe>/gi, "")
    .replace(/\son[a-z]+\s*=\s*(?:"[^"]*"|'[^']*'|[^\s>]+)/gi, "")
    .replace(/\s(href|src|action)\s*=\s*(["'])\s*javascript:[\s\S]*?\2/gi, "")
    .replace(/\s(href|src|action)\s*=\s*javascript:[^\s>]*/gi, "");

  const bodyMatch = cleaned.match(/<body\b[^>]*>([\s\S]*?)<\/body>/i);
  return `${styles}${bodyMatch ? bodyMatch[1] : cleaned}`;
}

const BLOCKED_REMOTE_IMAGE_DATA_URI = `data:image/svg+xml,${encodeURIComponent(
  '<svg xmlns="http://www.w3.org/2000/svg" width="500" height="280" viewBox="0 0 500 280"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop stop-color="#e4e4e7"/><stop offset="1" stop-color="#d4d4d8"/></linearGradient></defs><rect width="500" height="280" rx="18" fill="url(#g)"/><rect x="32" y="32" width="436" height="216" rx="14" fill="#fafafa" opacity=".8"/><path d="M178 178l48-52 34 36 25-27 57 61H158l20-18z" fill="#a1a1aa"/><circle cx="210" cy="102" r="20" fill="#c4c4cc"/><path d="M250 98h104M250 122h72" stroke="#a1a1aa" stroke-width="10" stroke-linecap="round"/></svg>'
)}`;

function proxyRemoteImageUrl(url: string): string {
  return `${IMAGE_PROXY_BASE}${encodeURIComponent(url)}`;
}

export function hasRemoteEmailImages(html: string): boolean {
  return /<(?:img\b[^>]*?\ssrc|[^>]+\sbackground)\s*=\s*(["']?)\s*https?:\/\//i.test(html)
    || /url\(\s*(["']?)\s*https?:\/\//i.test(html);
}

export function proxifyEmailImages(html: string): string {
  return html
    .replace(
      /(<img\b[^>]*?\ssrc\s*=\s*|\sbackground\s*=\s*)(?:(["'])(https?:\/\/[^"']+)\2|(https?:\/\/[^\s>]+))/gi,
      (_match, prefix, quote, quotedUrl, unquotedUrl) => {
        const url = quotedUrl ?? unquotedUrl;
        const delimiter = quote ?? '"';
        return `${prefix}${delimiter}${proxyRemoteImageUrl(url)}${delimiter}`;
      }
    )
    .replace(
      /url\(\s*(["']?)(https?:\/\/[^'"\s)]+)\1\s*\)/gi,
      (_match, _quote, url) => `url("${proxyRemoteImageUrl(url)}")`
    )
    .replace(/\ssrcset\s*=\s*(?:"[^"]*"|'[^']*'|[^\s>]+)/gi, "")
    .replace(/\sloading\s*=\s*["']lazy["']/gi, "");
}

function blockRemoteEmailImages(html: string): string {
  return html
    .replace(
      /(<img\b[^>]*?\ssrc\s*=\s*|\sbackground\s*=\s*)(?:(["'])(https?:\/\/[^"']+)\2|(https?:\/\/[^\s>]+))/gi,
      (_match, prefix, quote) => `${prefix}${quote ?? '"'}${BLOCKED_REMOTE_IMAGE_DATA_URI}${quote ?? '"'}`
    )
    .replace(/url\(\s*(["']?)(https?:\/\/[^'"\s)]+)\1\s*\)/gi, "none")
    .replace(/\ssrcset\s*=\s*(?:"[^"]*"|'[^']*'|[^\s>]+)/gi, "")
    .replace(/\sloading\s*=\s*["']lazy["']/gi, "");
}

export function buildRenderableEmailHtml(
  html: string,
  fallback: string,
  mode: RenderMode,
  loadRemoteImages = true,
): string {
  const source = (html || fallback || "").trim();
  const sourceIsHtml = isHtmlEmailBody(source);

  if (!sourceIsHtml) {
    return renderPlainTextEmail(source);
  }

  if (isSimpleDocumentHtml(source)) {
    return renderSimpleDocumentHtml(source);
  }

  if (mode === "simple" || byteLength(source) > LARGE_BODY_RENDER_LIMIT) {
    return renderPlainTextEmail(htmlToReadablePlainText(source) || fallback);
  }

  const sanitized = sanitizeEmailHtml(html, fallback)
    .replace(/@import\s+(?:url\()?[^;]+;?/gi, "")
    .replace(
    new RegExp(`\\s(src|href)\\s*=\\s*(["'])data:([^"']{${MAX_INLINE_DATA_URI},})\\2`, "gi"),
    ""
  );
  const withReaderGutter = `<div class="full-html">${sanitized}</div>`;
  return loadRemoteImages ? proxifyEmailImages(withReaderGutter) : blockRemoteEmailImages(withReaderGutter);
}

export function normalizeOtpPlaintext(text: string): string {
  // Remove zero-width and invisible unicode characters
  let s = text.replace(/[​-‍﻿⁠­]/g, "");
  s = s.replace(/\s+/g, " ").trim();
  // Join digits split by spaces: "1 2 3 4 5 6" → "123456"
  s = s.replace(/\b(?:\d[\s ]){3,11}\d\b/g, (m) => m.replace(/[\s ]+/g, ""));
  // Join hyphenated digit groups: "123-456" → "123456" (only for 3-4 digit groups totaling 4-8)
  s = s.replace(/\b(\d{3,4})[\s-](\d{3,4})\b/g, (m, g1, g2) => {
    if (g1.length + g2.length >= 4 && g1.length + g2.length <= 8) return g1 + g2;
    return m;
  });
  return s;
}

// OTP email signals — English only
const OTP_SIGNAL_EN_RE =
  /\b(?:verif(?:y|ication|ied)|confirm(?:ation)?|otp|one[\s-]?time|2fa|mfa|authentication\s*code|security\s*code|login\s*code|sign[\s-]?in\s*code|access\s*code)\b/i;

// Additional OTP signals for Turkish emails
const OTP_SIGNAL_TR_RE =
  /\b(?:doğrula(?:ma|yın|n|mak|r)?|dogrula(?:ma|yin|n|mak|r)?|onayla(?:yın|n|mak)?|onay\s*kodu?|tek[\s-]?kullan|güvenlik\s*kodu?|guvenlik\s*kodu?|sms\s*kodu?|hesap\s*doğrulama|oturum\s*aç|giriş\s*kodu?|giris\s*kodu?)\b/i;

function isOtpSignal(text: string, lang: AppLanguage): boolean {
  if (OTP_SIGNAL_EN_RE.test(text)) return true;
  if (lang === "tr" && OTP_SIGNAL_TR_RE.test(text)) return true;
  return false;
}

// Context words that suggest a number is NOT an OTP
const FALSE_POS_RE =
  /\b(?:order\s*#?|sipariş|fatura|invoice|ticket\s*#?|case\s*#?|ref(?:erence)?\s*#?|tracking|takip\s*no|po\s+box|sokak|cadde|mahalle|bulvar|version\s+v?\d|\biso\b|\bvat\b|\bkdv\b|numaral[ıi]|numarali|telefon|destek\s*merkezi)\b/i;

const METRIC_SUFFIX_RE = /^\d+(?:\.\d+)?[kmb]$/i;

function isValidCode(code: string): boolean {
  if (METRIC_SUFFIX_RE.test(code)) return false;
  if (/^[A-Za-z]+$/.test(code)) return false; // all letters = promo slug, not OTP
  if (/^\d+$/.test(code)) {
    if (/^(?:19|20)\d{2}$/.test(code)) return false; // year
    if (/^(?:27001|27701|22301|9001|42001|14001|45001|50001|31000)$/.test(code)) return false; // ISO standards
  }
  return true;
}

function falsePositiveNearby(text: string, idx: number, len: number): boolean {
  const before = text.slice(Math.max(0, idx - 80), idx);
  const after = text.slice(idx + len, Math.min(text.length, idx + len + 80));
  if (/[$€₺\xA3\xA5]\s*$/.test(before.trimEnd())) return true; // currency before
  if (/^\s*%/.test(after)) return true; // percentage after
  return FALSE_POS_RE.test(before + " " + after);
}

// Tier 1: Service-prefixed codes — "G-123456", "FB-654321"
function matchPrefixed(text: string): string | null {
  const re = /\b[A-Z]{1,3}-(\d{4,8})\b/g;
  let m;
  while ((m = re.exec(text)) !== null) {
    const ctx = text.slice(Math.max(0, m.index - 100), m.index + m[0].length + 100);
    if (/\bis\s+your\b|\bverif|\bdoğrulama|\bkodunuz\b|\bonay\b/i.test(ctx)) return m[1];
  }
  return null;
}

// Tier 2: Bracket codes — "[123456]" or "(654321)" — most reliable in subject lines
function matchBracket(text: string): string | null {
  const re = /[\[(](\d{4,8})[\])]/g;
  let m;
  while ((m = re.exec(text)) !== null) {
    if (isValidCode(m[1])) return m[1];
  }
  return null;
}

// Tier 3: keyword immediately before code — "code: 123456", "doğrulama kodunuz: 123456", "OTP: 123456"
const KW_BEFORE_CODE_RE =
  /(?:(?:verification|security|login|confirmation|access|one[\s-]?time|sms|güvenlik|guvenlik|doğrulama|dogrulama|onay)[\s-])?(?:code|kod(?:unuz|unu|u|lar)?|kodu|otp|pin|şifre(?:niz|nizi)?|sifre(?:niz|nizi)?|passcode|parola(?:nız|nızı)?)\s*(?:is\s+)?[:\-=→>]{1,2}\s*([A-Z0-9]{4,10})\b/gi;

function matchKeywordBefore(text: string): string | null {
  KW_BEFORE_CODE_RE.lastIndex = 0;
  let m;
  while ((m = KW_BEFORE_CODE_RE.exec(text)) !== null) {
    const code = m[1];
    if (isValidCode(code) && !falsePositiveNearby(text, m.index, m[0].length)) return code;
  }
  return null;
}

// Tier 4: code-first sentences — "123456 is your WhatsApp code", "654321 kodunuz"
const CODE_FIRST_RE = /\b([A-Z0-9]{4,8})\s+(?:is\s+(?:your|the)\b|kodunuz\b|şifreniz\b|sifreniz\b)/gi;

function matchCodeFirst(text: string): string | null {
  CODE_FIRST_RE.lastIndex = 0;
  let m;
  while ((m = CODE_FIRST_RE.exec(text)) !== null) {
    const code = m[1];
    if (!isValidCode(code) || falsePositiveNearby(text, m.index, m[0].length)) continue;
    // Turkish possessive forms already imply OTP context
    if (/kodunuz|şifreniz|sifreniz/i.test(m[0])) return code;
    // For "is your X", require an OTP word somewhere nearby
    const ctx = text.slice(Math.max(0, m.index - 30), m.index + m[0].length + 80);
    if (/\b(?:code|kod|otp|pin|verif|doğrulama|auth|login|password|şifre|sifre|confirm|onay)\b/i.test(ctx))
      return code;
  }
  return null;
}

// Tier 5: imperative patterns — "enter 123456", "use code 654321", "girin: 123456"
// Also handles Turkish "bu kodu kullanın: 123456" and "bu kodu girin: 123456" (Google emails)
const ENTER_RE = /\b(?:enter|use|input|type|gir(?:in?)?|kullanın?|giriniz)\s*:?\s+(?:the\s+)?(?:code\s+)?([A-Z0-9]{4,10})\b/gi;
const KODU_KULLAN_RE = /\b(?:bu\s+)?(?:kod(?:unuz|u|unu)?)\s+(?:kullanın?|gir(?:in?)?|giriniz)\s*[:\-]?\s*([A-Z0-9]{4,10})\b/gi;

function matchEnter(text: string): string | null {
  ENTER_RE.lastIndex = 0;
  let m;
  while ((m = ENTER_RE.exec(text)) !== null) {
    const code = m[1];
    if (isValidCode(code) && !falsePositiveNearby(text, m.index, m[0].length)) return code;
  }
  KODU_KULLAN_RE.lastIndex = 0;
  while ((m = KODU_KULLAN_RE.exec(text)) !== null) {
    const code = m[1];
    if (isValidCode(code) && !falsePositiveNearby(text, m.index, m[0].length)) return code;
  }
  return null;
}

// Tier 6: most prominent 6-digit number in a confirmed OTP email (last resort)
function matchFallback(subject: string, snippet: string, body: string, mode: OtpMode): string | null {
  type Candidate = { code: string; priority: number };
  const seen = new Set<string>();
  const candidates: Candidate[] = [];
  const SIX = /\b(\d{6})\b/g;

  const addFrom = (text: string, priority: number) => {
    SIX.lastIndex = 0;
    let m;
    while ((m = SIX.exec(text)) !== null) {
      const code = m[1];
      if (seen.has(code) || !isValidCode(code) || falsePositiveNearby(text, m.index, 6)) continue;
      seen.add(code);
      candidates.push({ code, priority });
    }
  };

  addFrom(subject, 3);
  addFrom(snippet, 2);
  addFrom(body.slice(0, 800), 1); // OTP codes appear near the top of the email body

  if (candidates.length === 0) return null;
  candidates.sort((a, b) => b.priority - a.priority);
  // Strict mode only accepts codes found in the subject line
  if (mode === "strict" && candidates[0].priority < 3) return null;
  return candidates[0].code;
}

export function extractVerificationCode(
  email: { subject: string; snippet: string; body_html: string },
  mode: OtpMode = "balanced",
  lang: AppLanguage = "en"
): string | null {
  if (mode === "off") return null;

  const subject = normalizeOtpPlaintext(email.subject || "");
  const body = normalizeOtpPlaintext(stripHtml(email.body_html || ""));
  const snippet = normalizeOtpPlaintext(email.snippet || "");
  const full = `${subject} ${snippet} ${body}`;

  const isOtpEmail = isOtpSignal(full, lang);

  if (!isOtpEmail) {
    // Doesn't look like an OTP email: only check subject for very obvious matches
    if (mode === "strict") return null;
    return (
      matchPrefixed(subject) ??
      matchBracket(subject) ??
      matchKeywordBefore(subject) ??
      matchCodeFirst(subject) ??
      null
    );
  }

  // Bracket in subject is the most reliable signal — check it first
  const bracket = matchBracket(subject);
  if (bracket) return bracket;

  // Tiered search: each tier tries subject → snippet → body
  const tiers = [matchPrefixed, matchKeywordBefore, matchCodeFirst, matchEnter];
  for (const fn of tiers) {
    const result = fn(subject) ?? fn(snippet) ?? fn(body);
    if (result) return result;
  }

  // Last resort: most prominent 6-digit number in a confirmed OTP email
  return matchFallback(subject, snippet, body, mode);
}

export function resolveEmailUrl(url: string | null | undefined): string | null {
  if (!url || url.startsWith("#")) return null;
  try {
    const resolved = new URL(url, "https://mail.google.com/").href;
    return /^(https?:|mailto:|tel:)/i.test(resolved) ? resolved : null;
  } catch {
    return null;
  }
}

export interface MailtoDraft {
  to: string;
  subject: string;
  body: string;
}

export function parseMailtoUrl(url: string): MailtoDraft | null {
  if (!/^mailto:/i.test(url)) return null;
  try {
    const parsed = new URL(url);
    const to = decodeURIComponent(parsed.pathname).trim();
    return {
      to,
      subject: parsed.searchParams.get("subject") ?? "",
      body: parsed.searchParams.get("body") ?? "",
    };
  } catch {
    return null;
  }
}

export function findEmailUrl(eventTarget: EventTarget | null): string | null {
  if (!eventTarget || typeof (eventTarget as unknown as Record<string, unknown>).closest !== "function") return null;
  const node = eventTarget as Element;
  const link = node.closest("a[href], area[href]") as HTMLAnchorElement | HTMLAreaElement | null;
  if (link) return resolveEmailUrl(link.getAttribute("href") || link.href);

  const button = node.closest("button, input[type='button'], input[type='submit'], [role='button']") as HTMLElement | null;
  const form = button?.closest("form") as HTMLFormElement | null;
  return resolveEmailUrl(
    button?.getAttribute("formaction") ||
    button?.getAttribute("data-href") ||
    button?.getAttribute("data-url") ||
    form?.getAttribute("action")
  );
}

export function searchHighlightTerms(query: string): string[] {
  const seen = new Set<string>();
  return query
    .trim()
    .split(/\s+/u)
    .map(term => term.trim())
    .filter(term => {
      if (!term) return false;
      const key = term.toLocaleLowerCase();
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    })
    .sort((left, right) => right.length - left.length)
    .slice(0, 20);
}

export function splitSearchHighlight(text: string, query: string): Array<{ text: string; match: boolean }> {
  const terms = searchHighlightTerms(query);
  if (!text || terms.length === 0) return [{ text, match: false }];
  const pattern = new RegExp(terms.map(term => term.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join("|"), "giu");
  const segments: Array<{ text: string; match: boolean }> = [];
  let cursor = 0;
  for (const match of text.matchAll(pattern)) {
    const index = match.index ?? 0;
    if (index > cursor) segments.push({ text: text.slice(cursor, index), match: false });
    segments.push({ text: match[0], match: true });
    cursor = index + match[0].length;
  }
  if (cursor < text.length) segments.push({ text: text.slice(cursor), match: false });
  return segments.length > 0 ? segments : [{ text, match: false }];
}

export function buildEmailSrcDoc(html: string): string {
  return `<!DOCTYPE html><html><head><meta charset="utf-8"/>
    <meta http-equiv="Content-Security-Policy" content="default-src 'none'; base-uri 'none'; form-action 'none'; frame-src 'none'; object-src 'none'; script-src 'none'; connect-src 'none'; media-src 'none'; img-src data: http://mailimg.localhost; style-src 'unsafe-inline'; font-src data:"/>
    <style>
      html, body { margin: 0; padding: 0; background: #fff; overflow: hidden; }
      * { box-sizing: border-box; }
      .mail-root {
        display: block; width: 100%; min-width: 0; padding: 0;
        font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
        font-size: 15px; line-height: 1.6; color: #1a1a1a;
      }
      .mail-root > .plain-text {
        width: 100%; padding: 24px clamp(20px, 3.5vw, 48px);
        white-space: pre-wrap; overflow-wrap: anywhere; word-break: normal;
        font-size: 15px; line-height: 1.65;
      }
      .mail-root > .simple-document {
        width: 100%; padding: 24px clamp(20px, 3.5vw, 48px);
        overflow-wrap: anywhere; font-size: 15px; line-height: 1.65;
      }
      .mail-root > .simple-document p { margin: 0 0 1em; }
      .mail-root > .simple-document p:last-child { margin-bottom: 0; }
      .mail-root > .simple-document blockquote { margin: 1em 0; padding-left: 14px; border-left: 3px solid #d4d4d8; color: #52525b; }
      .mail-root > .simple-document ul, .mail-root > .simple-document ol { margin: 0 0 1em; padding-left: 1.5em; }
      .mail-root > .full-html {
        width: 100%; padding: 16px 18px;
        overflow-wrap: anywhere; word-break: normal;
      }
      .mail-root > .full-html pre {
        max-width: 100%; white-space: pre-wrap !important;
        overflow-wrap: anywhere !important; word-break: normal;
      }
      .mail-root > .full-html code,
      .mail-root > .full-html a {
        overflow-wrap: anywhere; word-break: normal;
      }
      img, video { height: auto; }
      a { color: #2563eb; }
      mark.mail-search-highlight {
        background: #fde047; color: #18181b; border-radius: 2px;
        padding: 0 1px; box-decoration-break: clone;
      }
      ::selection { background: rgba(59, 130, 246, 0.25); }
    </style></head>
    <body><div class="mail-root">${html}</div></body></html>`;
}

export function readMailZoom(): MailZoom {
  const saved = localStorage.getItem("fursoy_mail_zoom");
  if (!saved || saved === "fit") return "fit";
  const value = parseFloat(saved);
  return Number.isFinite(value) ? Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, value)) : "fit";
}

export function getAutoMailViewMode(width: number): MailViewMode {
  if (width < 900) return "inbox-first";
  return "split";
}

export function readThemePreset(): ThemePresetName {
  const saved = localStorage.getItem("fursoy_theme_preset");
  return saved && saved in themePresets ? (saved as ThemePresetName) : "blue";
}

export function minutesFromTime(value: string): number {
  const [hours, minutes] = value.split(":").map(part => parseInt(part, 10));
  if (!Number.isFinite(hours) || !Number.isFinite(minutes)) return 0;
  return Math.max(0, Math.min(23, hours)) * 60 + Math.max(0, Math.min(59, minutes));
}

export function isInQuietHours(controls: AppControls): boolean {
  if (!controls.quietHoursEnabled) return false;
  const now = new Date();
  const current = now.getHours() * 60 + now.getMinutes();
  const start = minutesFromTime(controls.quietHoursStart);
  const end = minutesFromTime(controls.quietHoursEnd);
  if (start === end) return true;
  if (start < end) return current >= start && current < end;
  return current >= start || current < end;
}

export function formatDate(timestamp: number): string {
  const date = new Date(timestamp);
  const now = new Date();
  const isToday = date.toDateString() === now.toDateString();
  if (isToday) {
    return date.toLocaleTimeString("tr-TR", { hour: "2-digit", minute: "2-digit" });
  }
  return date.toLocaleDateString("tr-TR", { month: "short", day: "numeric" });
}

export function formatDateFull(timestamp: number, locale = "tr-TR"): string {
  const date = new Date(timestamp);
  return date.toLocaleDateString(locale, {
    month: "long",
    day: "numeric",
    year: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function formatRelativeTime(timestamp: number, now = Date.now(), locale = "tr-TR"): string {
  const delta = timestamp - now;
  const absoluteDelta = Math.abs(delta);
  const units: Array<[Intl.RelativeTimeFormatUnit, number]> = [
    ["year", 365 * 24 * 60 * 60 * 1_000],
    ["month", 30 * 24 * 60 * 60 * 1_000],
    ["day", 24 * 60 * 60 * 1_000],
    ["hour", 60 * 60 * 1_000],
    ["minute", 60 * 1_000],
  ];
  const [unit, unitMilliseconds] = units.find(([, milliseconds]) => absoluteDelta >= milliseconds) ?? ["second", 1_000];
  const value = Math.round(delta / unitMilliseconds);
  return new Intl.RelativeTimeFormat(locale, { numeric: "always" }).format(value, unit);
}

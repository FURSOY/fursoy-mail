import { describe, expect, it } from "vitest";
import {
  buildRenderableEmailHtml,
  buildEmailSrcDoc,
  extractVerificationCode,
  formatRelativeTime,
  isAuthFailure,
  isNoUpdateError,
  minutesFromTime,
  normalizeComposerLinkUrl,
  parseMailtoUrl,
  resolveEmailUrl,
  sanitizeComposerHtml,
  sanitizeEmailHtml,
  searchHighlightTerms,
  splitSearchHighlight,
} from "../utils";

describe("error classification", () => {
  it("recognizes transient authentication failures", () => {
    expect(isAuthFailure(new Error("401 Unauthorized"))).toBe(true);
    expect(isAuthFailure("invalid_grant")).toBe(true);
  });

  it("does not treat an insufficient-scope response as an expired token", () => {
    expect(isAuthFailure("403 Request had insufficient authentication scopes")).toBe(false);
  });

  it("recognizes updater responses that mean there is no newer version", () => {
    expect(isNoUpdateError("204 No update available")).toBe(true);
    expect(isNoUpdateError("connection timed out")).toBe(false);
  });
});

describe("verification code extraction", () => {
  it("extracts an English verification code", () => {
    expect(extractVerificationCode({
      subject: "Your verification code",
      snippet: "Use code 123456 to sign in",
      body_html: "<p>Use code <strong>123456</strong> to sign in.</p>",
    })).toBe("123456");
  });

  it("extracts a Turkish verification code", () => {
    expect(extractVerificationCode({
      subject: "Hesap doğrulama",
      snippet: "Doğrulama kodunuz: 654321",
      body_html: "",
    }, "balanced", "tr")).toBe("654321");
  });

  it("rejects ordinary order and invoice numbers", () => {
    expect(extractVerificationCode({
      subject: "Invoice 123456",
      snippet: "Your order number is 123456",
      body_html: "<p>Thank you for your order.</p>",
    })).toBeNull();
    expect(extractVerificationCode({
      subject: "[123456]",
      snippet: "Verification code",
      body_html: "",
    }, "off")).toBeNull();
  });
});

describe("email HTML safety", () => {
  it("wraps pathological technical lines before fit-to-width measures HTML mail", () => {
    const sourceDocument = buildEmailSrcDoc('<div class="full-html"><pre>very-long-technical-line</pre></div>');

    expect(sourceDocument).toContain("white-space: pre-wrap !important");
    expect(sourceDocument).toContain("overflow-wrap: anywhere !important");
    expect(sourceDocument).toContain(".mail-root > .full-html a");
  });

  it("keeps received HTML inert at the privileged composer boundary", () => {
    const sanitized = sanitizeComposerHtml(
      '<img src=x onerror="alert(1)"><script>alert(2)</script><b>Safe</b>',
    );

    expect(sanitized).toContain("Safe");
    expect(sanitized).not.toMatch(/onerror|script|<img/i);
  });

  it("removes executable content and event handlers", () => {
    const sanitized = sanitizeEmailHtml(
      '<html><head><style>.mail{color:red}</style></head><body><a href="javascript:alert(1)" onclick="alert(2)">Open</a><script>alert(3)</script><iframe>hidden</iframe></body></html>',
      "",
    );

    expect(sanitized).toContain("<style>.mail{color:red}</style>");
    expect(sanitized).toContain("Open");
    expect(sanitized).not.toMatch(/javascript:|onclick|<script|<iframe/i);
  });

  it("proxies remote images when loading is allowed", () => {
    const rendered = buildRenderableEmailHtml(
      '<img src="https://images.example.test/banner.png">',
      "",
      "full",
      true,
    );

    expect(rendered).toContain("http://mailimg.localhost/?url=");
    expect(rendered).not.toContain('src="https://images.example.test');
  });

  it("renders native plain-text mail with paragraphs and safe links in normal mode", () => {
    const rendered = buildRenderableEmailHtml(
      "Hello team,\r\n\r\nRead [the guide](https://example.test/guide).\r\nNext paragraph.",
      "",
      "full",
    );

    expect(rendered).toContain('class="plain-text"');
    expect(rendered).toContain("Hello team,\n\nRead ");
    expect(rendered).toContain('<a href="https://example.test/guide">the guide</a>');
  });

  it("keeps useful block structure while simplifying HTML mail", () => {
    const rendered = buildRenderableEmailHtml(
      '<style>p{color:red}</style><p>First paragraph</p><p>Second <a href="https://example.test">link</a></p><img src="https://tracker.test/pixel">',
      "",
      "simple",
    );

    expect(rendered).toContain("First paragraph\nSecond link (");
    expect(rendered).toContain('<a href="https://example.test">https://example.test</a>');
    expect(rendered).not.toMatch(/<style|<img/i);
  });

  it("renders plain-looking document HTML identically in normal and simplified modes", () => {
    const source = '<div dir="ltr"><p>Hello team,</p><p>A normal paragraph with <b>emphasis</b>.</p></div>';
    const normal = buildRenderableEmailHtml(source, "", "full");
    const simplified = buildRenderableEmailHtml(source, "", "simple");

    expect(normal).toBe(simplified);
    expect(normal).toContain('class="simple-document"');
  });
});

describe("small input helpers", () => {
  it("splits visible text into search-highlight segments", () => {
    expect(searchHighlightTerms(" fin  FIN weekly ")).toEqual(["weekly", "fin"]);
    expect(splitSearchHighlight("Fin weekly finance", "fin")).toEqual([
      { text: "Fin", match: true },
      { text: " weekly ", match: false },
      { text: "fin", match: true },
      { text: "ance", match: false },
    ]);
    expect(splitSearchHighlight("Fin", "")).toEqual([{ text: "Fin", match: false }]);
    expect(splitSearchHighlight("Open https://myaccount.google.com/notifications", "myaccount")).toEqual([
      { text: "Open https://", match: false },
      { text: "myaccount", match: true },
      { text: ".google.com/notifications", match: false },
    ]);
  });

  it("formats relative message times in the selected language", () => {
    const now = new Date("2026-07-28T15:30:00Z").getTime();
    expect(formatRelativeTime(now - 60 * 60 * 1_000, now, "tr-TR")).toBe("1 saat önce");
    expect(formatRelativeTime(now - 2 * 24 * 60 * 60 * 1_000, now, "en-US")).toBe("2 days ago");
  });

  it("only permits supported email link protocols", () => {
    expect(resolveEmailUrl("/mail/u/0/#inbox")).toBe("https://mail.google.com/mail/u/0/#inbox");
    expect(resolveEmailUrl("mailto:user@example.test")).toBe("mailto:user@example.test");
    expect(resolveEmailUrl("javascript:alert(1)")).toBeNull();
  });

  it("normalizes composer links and rejects executable schemes", () => {
    expect(normalizeComposerLinkUrl("example.test/path")).toBe("https://example.test/path");
    expect(normalizeComposerLinkUrl("mailto:user@example.test")).toBe("mailto:user@example.test");
    expect(normalizeComposerLinkUrl("javascript:alert(1)")).toBeNull();
    expect(normalizeComposerLinkUrl("https://example.test/\nBcc:test@example.test")).toBeNull();
  });

  it("parses mailto links into a compose draft", () => {
    expect(parseMailtoUrl("mailto:user@example.test?subject=Hello%20there&body=First%20line%0ASecond%20line")).toEqual({
      to: "user@example.test",
      subject: "Hello there",
      body: "First line\nSecond line",
    });
    expect(parseMailtoUrl("https://example.test")).toBeNull();
  });

  it("parses and clamps quiet-hour times", () => {
    expect(minutesFromTime("22:30")).toBe(1_350);
    expect(minutesFromTime("29:90")).toBe(1_439);
    expect(minutesFromTime("invalid")).toBe(0);
  });
});

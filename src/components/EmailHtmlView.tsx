import { useRef, useCallback, useEffect, useState } from "react";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { useLocale } from "../i18n";
import type { MailZoom } from "../types";
import { FIXED_LAYOUT_MIN_WIDTH, buildEmailSrcDoc, findEmailUrl, resolveEmailUrl, searchHighlightTerms } from "../utils";

export function EmailHtmlView({
  html,
  zoom,
  relayoutKey,
  onFitScaleChange,
  onOpenUrl,
  scrollRef,
  searchQuery = "",
}: {
  html: string;
  zoom: MailZoom;
  relayoutKey?: string | number;
  onFitScaleChange?: (scale: number) => void;
  onOpenUrl: (url: string) => void;
  scrollRef?: React.RefObject<HTMLElement | null>;
  searchQuery?: string;
}) {
  const tr = useLocale();
  const hostRef = useRef<HTMLDivElement>(null);
  const stageRef = useRef<HTMLDivElement>(null);
  const frameRef = useRef<HTMLIFrameElement>(null);
  const [documentVersion, setDocumentVersion] = useState(0);

  const applyScale = useCallback(() => {
    const host = hostRef.current;
    const stage = stageRef.current;
    const frame = frameRef.current;
    if (!host || !stage || !frame) return;
    const doc = frame.contentDocument;
    const root = doc?.querySelector(".mail-root") as HTMLElement | null;
    if (!doc || !root) return;

    const available = Math.max(1, host.clientWidth);

    if (zoom === "fit") {
      frame.style.height = "auto";
      frame.style.transform = "none";
      // Measure the mail's intrinsic width first. Fixed-width newsletters that
      // already fit should keep their natural canvas and be centered, just as
      // fixed 100% zoom does. Fluid messages still receive the full pane width.
      frame.style.width = "0px";
      const intrinsicWidth = Math.max(root.scrollWidth, 1);
      let layoutWidth: number;
      if (intrinsicWidth >= FIXED_LAYOUT_MIN_WIDTH) {
        layoutWidth = intrinsicWidth;
      } else {
        frame.style.width = `${available}px`;
        layoutWidth = Math.max(root.scrollWidth, available);
      }

      frame.style.width = `${layoutWidth}px`;
      if (layoutWidth > available + 1) {
        const fitScale = available / layoutWidth;
        const layoutHeight = Math.max(root.scrollHeight, doc.documentElement.scrollHeight, 1);
        frame.style.height = `${layoutHeight}px`;
        frame.style.transform = `scale(${fitScale})`;
        stage.style.width = `${Math.floor(layoutWidth * fitScale)}px`;
        stage.style.height = `${Math.floor(layoutHeight * fitScale)}px`;
        onFitScaleChange?.(fitScale);
      } else {
        const layoutHeight = Math.max(root.scrollHeight, doc.documentElement.scrollHeight, 1);
        frame.style.height = `${layoutHeight}px`;
        frame.style.transform = "none";
        stage.style.width = `${layoutWidth}px`;
        stage.style.height = `${layoutHeight}px`;
        onFitScaleChange?.(1);
      }
      return;
    }

    frame.style.height = "auto";
    frame.style.width = "0px";
    const minContentWidth = Math.max(root.scrollWidth, 1);

    let layoutWidth: number;
    if (minContentWidth >= FIXED_LAYOUT_MIN_WIDTH) {
      layoutWidth = minContentWidth;
    } else {
      const target = Math.max(80, Math.round(available / zoom));
      frame.style.width = `${target}px`;
      layoutWidth = Math.max(root.scrollWidth, target);
    }

    frame.style.width = `${layoutWidth}px`;
    const layoutHeight = Math.max(root.scrollHeight, doc.documentElement.scrollHeight, 1);
    frame.style.height = `${layoutHeight}px`;
    frame.style.transform = `scale(${zoom})`;
    stage.style.width = `${Math.floor(layoutWidth * zoom)}px`;
    stage.style.height = `${Math.floor(layoutHeight * zoom)}px`;
  }, [zoom, onFitScaleChange]);

  const applyScaleRef = useRef(applyScale);
  applyScaleRef.current = applyScale;

  useEffect(() => {
    const frame = frameRef.current;
    if (!frame) return;
    let innerCleanup: (() => void) | null = null;

    const handleLoad = () => {
      innerCleanup?.();
      innerCleanup = null;
      const doc = frame.contentDocument;
      if (!doc) return;

      if (!doc.querySelector(".mail-root")) {
        const navUrl = (() => {
          try { return frame.contentWindow?.location.href ?? null; } catch { return null; }
        })();
        frame.srcdoc = buildEmailSrcDoc(html);
        if (navUrl && navUrl !== "about:blank" && /^https?:/i.test(navUrl)) {
          onOpenUrl(navUrl);
        }
        return;
      }

      let active = true;
      const remeasure = () => { if (active) applyScaleRef.current(); };
      setDocumentVersion(version => version + 1);
      remeasure();

      const images = Array.from(doc.images);
      images.forEach((img) => img.addEventListener("load", remeasure));

      doc.fonts?.ready.then(remeasure).catch(() => {});

      const handleWheel = (e: WheelEvent) => {
        const outer = scrollRef?.current;
        if (!outer) return;
        let dy = e.deltaY;
        const dx = e.deltaX;
        if (e.deltaMode === 1) { dy *= 40; }
        else if (e.deltaMode === 2) { dy *= outer.clientHeight; }
        const maxScroll = outer.scrollHeight - outer.clientHeight;
        outer.scrollTop = Math.max(0, Math.min(maxScroll, outer.scrollTop + dy));
        if (dx !== 0) outer.scrollLeft += dx;
      };
      doc.addEventListener("wheel", handleWheel, { passive: true });

      const handleClick = (event: Event) => {
        const url = findEmailUrl(event.target);
        const isInteractive = url !== null ||
          !!(event.target as Element | null)?.closest?.("a, area, button, [role='button']");
        if (isInteractive) event.preventDefault();
        if (!url) return;
        event.stopPropagation();
        onOpenUrl(url);
      };
      const handleSubmit = (event: Event) => {
        const node = event.target as Element | null;
        const form = node?.closest?.("form") as HTMLFormElement | null;
        const url = resolveEmailUrl(form?.getAttribute("action"));
        if (!url) return;
        event.preventDefault();
        event.stopPropagation();
        onOpenUrl(url);
      };
      const handleCopy = (event: ClipboardEvent) => {
        const selectedText = doc.getSelection()?.toString() ?? "";
        if (!selectedText) return;

        // Email HTML often carries its own background and text styles. Copy the
        // selection as standard plain text so it works with Windows clipboard
        // history and does not leak those styles into the compose editor.
        event.preventDefault();
        event.clipboardData?.setData("text/plain", selectedText);
        void writeText(selectedText).catch(error => {
          console.error("[MAIL] clipboard write failed:", error);
        });
      };
      doc.addEventListener("click", handleClick, true);
      doc.addEventListener("submit", handleSubmit, true);
      doc.addEventListener("copy", handleCopy);
      const handleContextMenu = (e: Event) => e.preventDefault();
      doc.addEventListener("contextmenu", handleContextMenu, true);

      innerCleanup = () => {
        active = false;
        images.forEach((img) => img.removeEventListener("load", remeasure));
        doc.removeEventListener("wheel", handleWheel);
        doc.removeEventListener("click", handleClick, true);
        doc.removeEventListener("submit", handleSubmit, true);
        doc.removeEventListener("copy", handleCopy);
        doc.removeEventListener("contextmenu", handleContextMenu, true);
      };
    };

    frame.addEventListener("load", handleLoad);
    frame.srcdoc = buildEmailSrcDoc(html);

    return () => {
      frame.removeEventListener("load", handleLoad);
      innerCleanup?.();
    };
  }, [html, onOpenUrl, scrollRef]);

  useEffect(() => {
    const doc = frameRef.current?.contentDocument;
    if (!doc?.querySelector(".mail-root")) return;
    let cancelled = false;
    let timer = 0;

    for (const mark of Array.from(doc.querySelectorAll("mark.mail-search-highlight"))) {
      mark.replaceWith(doc.createTextNode(mark.textContent ?? ""));
    }

    const terms = searchHighlightTerms(searchQuery);
    if (terms.length === 0) {
      applyScaleRef.current();
      return;
    }

    const pattern = new RegExp(
      terms.map(term => term.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join("|"),
      "giu"
    );
    const walker = doc.createTreeWalker(doc.body, 4); // NodeFilter.SHOW_TEXT
    let highlightCount = 0;
    const processChunk = () => {
      if (cancelled) return;
      const matchingNodes: Text[] = [];
      let visited = 0;
      while (visited < 250 && highlightCount < 500 && walker.nextNode()) {
        visited += 1;
        const node = walker.currentNode as Text;
        const parent = node.parentElement;
        if (!node.data || parent?.closest("style, script, noscript, textarea, mark.mail-search-highlight")) continue;
        pattern.lastIndex = 0;
        if (pattern.test(node.data)) matchingNodes.push(node);
      }
      for (const node of matchingNodes) {
        if (!node.isConnected) continue;
        const fragment = doc.createDocumentFragment();
        pattern.lastIndex = 0;
        let cursor = 0;
        for (const match of node.data.matchAll(pattern)) {
          if (highlightCount >= 500) break;
          const index = match.index ?? 0;
          if (index > cursor) fragment.append(doc.createTextNode(node.data.slice(cursor, index)));
          const mark = doc.createElement("mark");
          mark.className = "mail-search-highlight";
          mark.textContent = match[0];
          fragment.append(mark);
          cursor = index + match[0].length;
          highlightCount += 1;
        }
        if (cursor < node.data.length) fragment.append(doc.createTextNode(node.data.slice(cursor)));
        node.replaceWith(fragment);
      }
      if (visited === 250 && highlightCount < 500) {
        timer = window.setTimeout(processChunk, 0);
      } else {
        applyScaleRef.current();
      }
    };
    timer = window.setTimeout(processChunk, 0);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [documentVersion, searchQuery]);

  useEffect(() => {
    let raf = 0;
    let timer = 0;
    const schedule = () => {
      cancelAnimationFrame(raf);
      clearTimeout(timer);
      raf = requestAnimationFrame(() => {
        applyScale();
        timer = window.setTimeout(() => applyScale(), 260);
      });
    };
    schedule();
    const host = hostRef.current;
    if (!host) return () => { cancelAnimationFrame(raf); clearTimeout(timer); };
    const resizeObserver = new ResizeObserver(schedule);
    resizeObserver.observe(host);
    return () => {
      cancelAnimationFrame(raf);
      clearTimeout(timer);
      resizeObserver.disconnect();
    };
  }, [applyScale, relayoutKey]);

  return (
    <div ref={hostRef} className="relative w-full min-w-0 overflow-x-auto overflow-y-hidden overscroll-contain bg-white select-text">
      <div ref={stageRef} className="relative mx-auto">
        <iframe
          ref={frameRef}
          title={tr.common.emailContent}
          sandbox="allow-same-origin allow-popups"
          className="absolute left-0 top-0 block border-0 bg-white"
          style={{ transformOrigin: "top left", width: 0, height: 0 }}
        />
      </div>
    </div>
  );
}

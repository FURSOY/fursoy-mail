import { IMAGE_PROXY_BASE } from "./utils";

export function proxiedProfileImageUrl(value: string | null | undefined, attempt = 0): string | null {
  if (!value) return null;
  try {
    const url = new URL(value);
    if (url.protocol !== "https:" && url.protocol !== "http:") return null;
    return `${IMAGE_PROXY_BASE}${encodeURIComponent(url.href)}&attempt=${attempt}`;
  } catch {
    return null;
  }
}

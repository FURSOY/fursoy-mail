import type { ThreadGroup } from "./types";

export type MailListCache = Partial<Record<string, ThreadGroup[]>>;

export function readMailListCache(cache: MailListCache, key: string): ThreadGroup[] | undefined {
  const value = cache[key];
  if (value === undefined) return undefined;
  delete cache[key];
  cache[key] = value;
  return value;
}

export function writeMailListCache(
  cache: MailListCache,
  key: string,
  value: ThreadGroup[],
  maxEntries: number,
): void {
  delete cache[key];
  cache[key] = value;
  while (Object.keys(cache).length > maxEntries) {
    const oldest = Object.keys(cache)[0];
    if (!oldest) break;
    delete cache[oldest];
  }
}

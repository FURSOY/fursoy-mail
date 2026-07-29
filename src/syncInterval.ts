export const MIN_SYNC_INTERVAL_SECONDS = 2;
export const DEFAULT_SYNC_INTERVAL_SECONDS = 30;

// Browsers store setTimeout delays in a signed 32-bit integer. Keeping this
// guard separate from the user-facing value prevents very large intervals
// from overflowing into an immediate, repeating sync loop.
const MAX_TIMER_DELAY_MS = 2_147_483_647;

export function normalizeSyncIntervalSeconds(
  value: string | number | null,
  fallback = DEFAULT_SYNC_INTERVAL_SECONDS,
): number {
  const parsed = typeof value === "number" ? value : Number.parseInt(value ?? "", 10);
  if (!Number.isFinite(parsed)) return fallback;
  return Math.max(MIN_SYNC_INTERVAL_SECONDS, Math.floor(parsed));
}

export function syncIntervalDelayMs(seconds: number): number {
  return Math.min(normalizeSyncIntervalSeconds(seconds) * 1_000, MAX_TIMER_DELAY_MS);
}

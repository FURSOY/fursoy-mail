import { describe, expect, it } from "vitest";
import {
  DEFAULT_SYNC_INTERVAL_SECONDS,
  normalizeSyncIntervalSeconds,
  syncIntervalDelayMs,
} from "../syncInterval";

describe("sync interval", () => {
  it("accepts two seconds and does not impose the old 300-second maximum", () => {
    expect(normalizeSyncIntervalSeconds("2")).toBe(2);
    expect(normalizeSyncIntervalSeconds("3600")).toBe(3600);
  });

  it("normalizes low, fractional, and invalid values safely", () => {
    expect(normalizeSyncIntervalSeconds("1")).toBe(2);
    expect(normalizeSyncIntervalSeconds(2.9)).toBe(2);
    expect(normalizeSyncIntervalSeconds("invalid")).toBe(DEFAULT_SYNC_INTERVAL_SECONDS);
  });

  it("prevents large intervals from overflowing the browser timer", () => {
    expect(syncIntervalDelayMs(2)).toBe(2_000);
    expect(syncIntervalDelayMs(Number.MAX_SAFE_INTEGER)).toBe(2_147_483_647);
  });
});

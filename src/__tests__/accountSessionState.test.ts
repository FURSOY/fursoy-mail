import { describe, expect, it } from "vitest";
import { allAccountsExpired } from "../accountSessionState";

describe("allAccountsExpired", () => {
  it("reports the expired state only while every account is expired", () => {
    const accounts = ["a@example.test", "b@example.test"];

    expect(allAccountsExpired(accounts, new Set())).toBe(false);
    expect(allAccountsExpired(accounts, new Set(["a@example.test"]))).toBe(false);
    expect(allAccountsExpired(accounts, new Set(accounts))).toBe(true);
  });

  it("clears once a single account authenticates again", () => {
    const accounts = ["a@example.test", "b@example.test"];
    const expired = new Set(accounts);
    expect(allAccountsExpired(accounts, expired)).toBe(true);

    expired.delete("a@example.test");
    expect(allAccountsExpired(accounts, expired)).toBe(false);
  });

  it("does not report an expired session when there are no accounts", () => {
    expect(allAccountsExpired([], new Set())).toBe(false);
  });

  it("ignores stale entries for accounts that were removed", () => {
    expect(allAccountsExpired(["a@example.test"], new Set(["removed@example.test"]))).toBe(false);
  });
});

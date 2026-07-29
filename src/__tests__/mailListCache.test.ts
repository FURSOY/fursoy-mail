import { describe, expect, it } from "vitest";
import { readMailListCache, writeMailListCache, type MailListCache } from "../mailListCache";
import type { ThreadGroup } from "../types";

const groups = (id: string) => [{ latestEmail: { id } }] as ThreadGroup[];

describe("mail list cache", () => {
  it("keeps recently read account mailboxes hot", () => {
    const cache: MailListCache = {};
    writeMailListCache(cache, "all\0inbox", groups("all"), 2);
    writeMailListCache(cache, "account-a\0inbox", groups("a"), 2);

    expect(readMailListCache(cache, "all\0inbox")?.[0].latestEmail.id).toBe("all");
    writeMailListCache(cache, "account-b\0inbox", groups("b"), 2);

    expect(cache["all\0inbox"]).toBeDefined();
    expect(cache["account-a\0inbox"]).toBeUndefined();
    expect(cache["account-b\0inbox"]).toBeDefined();
  });
});

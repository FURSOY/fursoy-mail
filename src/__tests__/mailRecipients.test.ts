import { describe, expect, it } from "vitest";
import type { EmailSummary } from "../types";
import { calculateReplyAllRecipients, calculateReplyRecipients, parseMailboxList } from "../mailRecipients";

const mail = (overrides: Partial<EmailSummary> = {}): EmailSummary => ({
  id: "gmail-id",
  thread_id: "thread-id",
  sender: "Alice <alice@example.test>",
  recipient: "Me <me@example.test>",
  cc: "",
  reply_to: "",
  message_id: "<message@example.test>",
  references: "<root@example.test>",
  subject: "Subject",
  snippet: "Snippet",
  date: 1,
  unread: false,
  label: "inbox",
  account_id: "me@example.test",
  ...overrides,
});

describe("reply recipient calculation", () => {
  it("parses quoted names containing commas", () => {
    expect(parseMailboxList('"Doe, Jane" <jane@example.test>, Bob <bob@example.test>'))
      .toEqual(["jane@example.test", "bob@example.test"]);
  });

  it("prefers Reply-To and removes the signed-in account and duplicates", () => {
    const recipients = calculateReplyAllRecipients(mail({
      reply_to: "Support <help@example.test>",
      recipient: "me@example.test, help@example.test, team@example.test",
      cc: "team@example.test, audit@example.test",
    }));
    expect(recipients.to).toEqual(["help@example.test", "team@example.test"]);
    expect(recipients.cc).toEqual(["audit@example.test"]);
  });

  it("replies to the first non-self recipient for a sent message", () => {
    const recipients = calculateReplyRecipients(mail({
      sender: "Me <me@example.test>",
      recipient: "Alice <alice@example.test>, Bob <bob@example.test>",
      label: "sent",
    }));
    expect(recipients.to).toEqual(["alice@example.test"]);
    expect(recipients.canReplyAll).toBe(true);
  });

  it("does not offer Reply All when only one eligible recipient remains", () => {
    expect(calculateReplyRecipients(mail({
      recipient: "Me <me@example.test>",
      cc: "me@example.test",
    })).canReplyAll).toBe(false);
  });
});

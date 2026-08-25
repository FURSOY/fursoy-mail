/**
 * The session-expired banner claims that every account lost its session, so it
 * must be recomputed whenever an account expires *or* authenticates again.
 * Deriving it in one place keeps the banner from outliving the condition it
 * reports.
 */
export function allAccountsExpired(
  accountIds: readonly string[],
  expiredAccountIds: ReadonlySet<string>,
): boolean {
  return accountIds.length > 0 && accountIds.every(id => expiredAccountIds.has(id));
}

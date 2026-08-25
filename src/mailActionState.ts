import type { EmailSummary } from "./types";
import { isAuthFailure, isSessionRevoked } from "./utils";

export type MailMutationQueue = Map<string, Promise<void>>;

export function enqueueMailMutation(
  queue: MailMutationQueue,
  key: string,
  mutation: () => Promise<void>,
): Promise<void> {
  const previous = queue.get(key) ?? Promise.resolve();
  const current = previous.catch(() => undefined).then(mutation);
  queue.set(key, current);
  const cleanup = () => {
    if (queue.get(key) === current) queue.delete(key);
  };
  void current.then(cleanup, cleanup);
  return current;
}

interface AuthenticatedMailActionOptions {
  accountId: string;
  currentToken: string;
  reloginRequiredMessage: string;
  action: (accessToken: string) => Promise<void>;
  refreshAccessToken: (accountId: string) => Promise<{ authenticated: boolean }>;
  upsertToken: (accountId: string, accessToken: string) => void;
  clearExpiredAccount: (accountId: string) => void;
  markAccountExpired: (accountId: string) => void;
}

export async function runAuthenticatedMailAction(options: AuthenticatedMailActionOptions): Promise<void> {
  const {
    accountId, currentToken, reloginRequiredMessage, action, refreshAccessToken,
    upsertToken, clearExpiredAccount, markAccountExpired,
  } = options;
  if (!currentToken) throw new Error(reloginRequiredMessage);

  try {
    await action(currentToken);
  } catch (error) {
    if (!isAuthFailure(error)) throw error;
    let refreshed: { authenticated: boolean };
    try {
      refreshed = await refreshAccessToken(accountId);
    } catch (refreshError) {
      // A refresh that could not be completed says nothing about the stored
      // credential, so the account keeps its session and the next action tries
      // again. Only a credential the provider rejected costs a sign-in.
      if (isSessionRevoked(refreshError)) markAccountExpired(accountId);
      throw refreshError;
    }
    if (!refreshed.authenticated) {
      markAccountExpired(accountId);
      throw new Error(reloginRequiredMessage);
    }
    upsertToken(accountId, "active");
    clearExpiredAccount(accountId);
    await action("active");
  }
}

export function inboxUnreadDelta(mail: EmailSummary, destinationLabel: string): number {
  if (!mail.unread || mail.label === destinationLabel) return 0;
  if (mail.label === "inbox") return -1;
  if (destinationLabel === "inbox") return 1;
  return 0;
}

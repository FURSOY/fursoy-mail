import { useCallback, useEffect, useRef, useState } from "react";
import { allAccountsExpired } from "../accountSessionState";
import type { Account } from "../types";
import { tauriApi, type DiscoveredMailProvider, type ImapAccountInput } from "../tauriApi";

export function useAccounts() {
  const [accounts, setAccounts] = useState<Account[]>([]);
  const [accountsLoaded, setAccountsLoaded] = useState(false);
  const [isConnecting, setIsConnecting] = useState(false);
  const [accountTokens, setAccountTokens] = useState<Record<string, string>>({});
  const [activeAccountId, setActiveAccountId] = useState<string | null>(null);
  const [tokenExpired, setTokenExpired] = useState(false);
  const [expiredAccountIds, setExpiredAccountIds] = useState<Set<string>>(() => new Set());

  const accountsRef = useRef<Account[]>([]);
  const accountTokensRef = useRef<Record<string, string>>({});
  const activeAccountIdRef = useRef<string | null>(null);
  const expiredAccountsRef = useRef<Set<string>>(new Set());
  const tokenExpiredRef = useRef(false);

  useEffect(() => { accountsRef.current = accounts; }, [accounts]);
  useEffect(() => { accountTokensRef.current = accountTokens; }, [accountTokens]);
  useEffect(() => { activeAccountIdRef.current = activeAccountId; }, [activeAccountId]);
  useEffect(() => { tokenExpiredRef.current = tokenExpired; }, [tokenExpired]);

  const replaceAccounts = useCallback((next: Account[]) => {
    accountsRef.current = next;
    setAccounts(next);
  }, []);

  const selectAccount = useCallback((accountId: string | null) => {
    activeAccountIdRef.current = accountId;
    setActiveAccountId(accountId);
  }, []);

  const replaceTokens = useCallback((tokens: Record<string, string>) => {
    accountTokensRef.current = tokens;
    setAccountTokens(tokens);
  }, []);

  const upsertToken = useCallback((accountId: string, accessToken: string) => {
    const next = { ...accountTokensRef.current, [accountId]: accessToken };
    accountTokensRef.current = next;
    setAccountTokens(next);
  }, []);

  const removeToken = useCallback((accountId: string) => {
    const next = { ...accountTokensRef.current };
    delete next[accountId];
    accountTokensRef.current = next;
    setAccountTokens(next);
  }, []);

  const syncExpiredBanner = useCallback(() => {
    const expired = allAccountsExpired(
      accountsRef.current.map(account => account.id),
      expiredAccountsRef.current,
    );
    if (tokenExpiredRef.current !== expired) {
      tokenExpiredRef.current = expired;
      setTokenExpired(expired);
    }
    return expired;
  }, []);

  const clearExpiredAccount = useCallback((accountId: string) => {
    expiredAccountsRef.current.delete(accountId);
    setExpiredAccountIds(new Set(expiredAccountsRef.current));
    syncExpiredBanner();
  }, [syncExpiredBanner]);

  const expireAccount = useCallback((accountId: string) => {
    if (expiredAccountsRef.current.has(accountId)) {
      return { newlyExpired: false, allExpired: false };
    }
    removeToken(accountId);
    expiredAccountsRef.current.add(accountId);
    setExpiredAccountIds(new Set(expiredAccountsRef.current));
    return { newlyExpired: true, allExpired: syncExpiredBanner() };
  }, [removeToken, syncExpiredBanner]);

  const setSessionExpired = useCallback((expired: boolean) => {
    tokenExpiredRef.current = expired;
    setTokenExpired(expired);
  }, []);

  const loadAccounts = useCallback(async () => {
    const loaded = await tauriApi.getAccounts();
    replaceAccounts(loaded);
    setAccountsLoaded(true);
    return loaded;
  }, [replaceAccounts]);

  const initializeAccounts = useCallback(async () => {
    const loaded = await loadAccounts();
    if (loaded.length === 0) return loaded;

    selectAccount(loaded[0].id);
    const tokens: Record<string, string> = {};
    for (const account of loaded) {
      try {
        const auth = await tauriApi.getAccountAuth(account.id);
        const stillValid = auth?.expires_at == null
          || auth.expires_at > Math.floor(Date.now() / 1000) + 30;
        if (auth?.authenticated && (account.provider === "imap" || stillValid)) tokens[account.id] = "active";
      } catch {
        console.error("Failed to load account authentication state.");
      }
    }
    replaceTokens(tokens);
    return loaded;
  }, [loadAccounts, replaceTokens, selectAccount]);

  const disconnectAccount = useCallback(async (accountId: string) => {
    await tauriApi.removeAccount(accountId);
    removeToken(accountId);
    clearExpiredAccount(accountId);
    const updated = await loadAccounts();
    if (activeAccountIdRef.current === accountId) {
      selectAccount(updated[0]?.id ?? null);
    }
    return updated;
  }, [clearExpiredAccount, loadAccounts, removeToken, selectAccount]);

  const addImapAccount = useCallback(async (input: ImapAccountInput) => {
    const account = await tauriApi.addMailAccount(input);
    const updated = await loadAccounts();
    upsertToken(account.id, "active");
    clearExpiredAccount(account.id);
    if (updated.length === 1) selectAccount(account.id);
    return { account, accounts: updated };
  }, [clearExpiredAccount, loadAccounts, selectAccount, upsertToken]);

  const addOAuthMailAccount = useCallback(async (email: string, provider: DiscoveredMailProvider) => {
    const auth = await tauriApi.startMailOAuth(email, provider);
    const updated = await loadAccounts();
    upsertToken(auth.email, "active");
    clearExpiredAccount(auth.email);
    if (updated.length === 1) selectAccount(auth.email);
    return { auth, accounts: updated };
  }, [clearExpiredAccount, loadAccounts, selectAccount, upsertToken]);

  const reorderAndReloadAccounts = useCallback(async (orderedIds: string[]) => {
    await tauriApi.reorderAccounts(orderedIds);
    return loadAccounts();
  }, [loadAccounts]);

  return {
    accounts,
    accountsLoaded,
    isConnecting,
    accountTokens,
    activeAccountId,
    tokenExpired,
    expiredAccountIds,
    accountsRef,
    accountTokensRef,
    activeAccountIdRef,
    expiredAccountsRef,
    tokenExpiredRef,
    setAccountsLoaded,
    reloadAccounts: loadAccounts,
    setIsConnecting,
    replaceAccounts,
    selectAccount,
    replaceTokens,
    upsertToken,
    removeToken,
    clearExpiredAccount,
    expireAccount,
    setSessionExpired,
    loadAccounts,
    initializeAccounts,
    addImapAccount,
    addOAuthMailAccount,
    disconnectAccount,
    reorderAndReloadAccounts,
    refreshAccessToken: tauriApi.refreshAccessToken,
  };
}

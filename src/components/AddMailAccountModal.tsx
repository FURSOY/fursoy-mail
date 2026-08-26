import { useEffect, useMemo, useRef, useState } from "react";
import { ArrowLeft, LockKeyhole, Mail, Server, X } from "lucide-react";
import { useLocale } from "../i18n";
import { typography, ui } from "../theme";
import {
  tauriApi,
  type DiscoveredMailProvider,
  type ImapAccountInput,
  type MailSecurity,
} from "../tauriApi";

interface AddMailAccountModalProps {
  open: boolean;
  onClose: () => void;
  onAdd: (input: ImapAccountInput) => Promise<void>;
  onOAuth: (email: string, provider: DiscoveredMailProvider) => Promise<void>;
}

const initialForm: ImapAccountInput = {
  email: "",
  username: "",
  password: "",
  imapHost: "",
  imapPort: 993,
  imapSecurity: "tls",
  smtpHost: "",
  smtpPort: 587,
  smtpSecurity: "starttls",
};

function suggestedServers(email: string, provider: DiscoveredMailProvider = "manual"): Partial<ImapAccountInput> {
  const domain = email.trim().toLowerCase().split("@")[1] ?? "";
  if (provider === "google") {
    return { imapHost: "imap.gmail.com", imapPort: 993, imapSecurity: "tls", smtpHost: "smtp.gmail.com", smtpPort: 465, smtpSecurity: "tls" };
  }
  if (provider === "microsoft") {
    const personal = ["outlook.com", "hotmail.com", "live.com", "msn.com"].includes(domain)
      || domain.startsWith("outlook.")
      || domain.startsWith("hotmail.");
    return { imapHost: "outlook.office365.com", imapPort: 993, imapSecurity: "tls", smtpHost: personal ? "smtp-mail.outlook.com" : "smtp.office365.com", smtpPort: 587, smtpSecurity: "starttls" };
  }
  if (provider === "yahoo") {
    return { imapHost: "imap.mail.yahoo.com", imapPort: 993, imapSecurity: "tls", smtpHost: "smtp.mail.yahoo.com", smtpPort: 465, smtpSecurity: "tls" };
  }
  if (provider === "icloud") {
    return { imapHost: "imap.mail.me.com", imapPort: 993, imapSecurity: "tls", smtpHost: "smtp.mail.me.com", smtpPort: 587, smtpSecurity: "starttls" };
  }
  return domain
    ? { imapHost: `imap.${domain}`, imapPort: 993, imapSecurity: "tls", smtpHost: `smtp.${domain}`, smtpPort: 587, smtpSecurity: "starttls" }
    : {};
}

export function AddMailAccountModal({ open, onClose, onAdd, onOAuth }: AddMailAccountModalProps) {
  const tr = useLocale();
  const dialogRef = useRef<HTMLDivElement>(null);
  const [step, setStep] = useState<"email" | "manual">("email");
  const [email, setEmail] = useState("");
  const [form, setForm] = useState(initialForm);
  const [provider, setProvider] = useState<DiscoveredMailProvider>("manual");
  const [submitting, setSubmitting] = useState(false);
  const [oauthPending, setOAuthPending] = useState(false);
  const [error, setError] = useState("");
  const attemptRef = useRef(0);
  const pendingOAuthRef = useRef<{ email: string; provider: DiscoveredMailProvider } | null>(null);
  const canSubmitManual = useMemo(() => Boolean(
    form.email.trim() && form.username.trim() && form.password && form.imapHost.trim() && form.smtpHost.trim()
  ), [form]);

  useEffect(() => {
    if (!open) return;
    setStep("email");
    setEmail("");
    setForm(initialForm);
    setProvider("manual");
    setError("");
    setSubmitting(false);
    setOAuthPending(false);
  }, [open]);

  if (!open) return null;

  const localizedError = (cause: unknown) => {
    const raw = String(cause).replace(/^Error:\s*/i, "").trim();
    const key = raw.match(/(mail_(?:account|oauth)_[a-z_]+)/)?.[1] ?? "unknown";
    const errors = tr.mailAccount.errors as Record<string, string>;
    const known = errors[key];
    if (known) return known;
    // A reason with no wording of its own still has to reach the user: the
    // generic sentence alone says nothing about what to change.
    return raw ? `${errors.unknown} (${raw.slice(0, 160)})` : errors.unknown;
  };
  const requestClose = () => {
    if (oauthPending) void tauriApi.cancelMailOAuth();
    if (!submitting || oauthPending) onClose();
  };
  const openManual = (mail = email, detected: DiscoveredMailProvider = "manual") => {
    const normalizedEmail = mail.trim();
    setProvider(detected);
    setForm({
      ...initialForm,
      email: normalizedEmail,
      username: normalizedEmail,
      ...suggestedServers(normalizedEmail, detected),
    });
    setError("");
    setStep("manual");
  };
  /// The sign-in happens in the browser, where this app cannot see whether the
  /// user finished, closed the tab, or wandered off. So the wait is owned by an
  /// attempt number: a retry cancels the one still waiting and takes over, and
  /// the abandoned attempt is not allowed to write over the new one's state.
  const runOAuth = async (targetEmail: string, targetProvider: DiscoveredMailProvider) => {
    const attempt = ++attemptRef.current;
    pendingOAuthRef.current = { email: targetEmail, provider: targetProvider };
    setSubmitting(true);
    setOAuthPending(true);
    setError("");
    try {
      await onOAuth(targetEmail, targetProvider);
      if (attemptRef.current !== attempt) return;
      onClose();
    } catch (cause) {
      if (attemptRef.current !== attempt) return;
      if (!/oauth_cancelled/i.test(String(cause))) setError(localizedError(cause));
    } finally {
      if (attemptRef.current === attempt) {
        setOAuthPending(false);
        setSubmitting(false);
      }
    }
  };

  const retryOAuth = async () => {
    const target = pendingOAuthRef.current;
    if (!target) return;
    await tauriApi.cancelMailOAuth().catch(() => {});
    void runOAuth(target.email, target.provider);
  };

  const discover = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!email.trim() || submitting) return;
    setSubmitting(true);
    setError("");
    try {
      const discovery = await tauriApi.discoverMailProvider(email);
      setEmail(discovery.email);
      if (discovery.authType === "oauth") {
        await runOAuth(discovery.email, discovery.provider);
        return;
      }
      openManual(discovery.email, discovery.provider);
    } catch (cause) {
      setError(localizedError(cause));
    } finally {
      setSubmitting(false);
    }
  };
  const update = <K extends keyof ImapAccountInput>(key: K, value: ImapAccountInput[K]) => {
    setForm(previous => ({ ...previous, [key]: value }));
  };
  const submitManual = async (event: React.FormEvent) => {
    event.preventDefault();
    if (!canSubmitManual || submitting) return;
    setSubmitting(true);
    setError("");
    try {
      await onAdd(form);
      onClose();
    } catch (cause) {
      setError(localizedError(cause));
    } finally {
      setSubmitting(false);
    }
  };
  const startOAuthFromManual = async (oauthProvider: "google" | "microsoft") => {
    if (!form.email.trim() || submitting) return;
    await runOAuth(form.email.trim(), oauthProvider);
  };

  return (
    <div className="fixed inset-0 z-[260] flex items-center justify-center bg-black/60 p-4 backdrop-blur-sm" onClick={requestClose}>
      <div ref={dialogRef} role="dialog" aria-modal="true" aria-labelledby="add-mail-account-title" className={`w-full ${step === "email" ? "max-w-md" : "max-w-xl"} ${ui.modal} max-h-[90vh] overflow-y-auto p-6`} onClick={event => event.stopPropagation()} onKeyDown={event => { if (event.key === "Escape") requestClose(); }}>
        <div className="mb-5 flex items-start justify-between gap-4">
          <div className="flex items-center gap-3">
            {step === "manual" ? (
              <button type="button" onClick={() => setStep("email")} disabled={submitting} aria-label={tr.common.back} className={ui.iconButton}><ArrowLeft className="h-4 w-4" /></button>
            ) : (
              <div className="flex h-10 w-10 items-center justify-center rounded-[var(--radius-md)] bg-[var(--app-accent-soft)] text-[var(--app-accent)]"><Mail className="h-5 w-5" /></div>
            )}
            <div><h2 id="add-mail-account-title" className={typography.pageTitle}>{step === "email" ? tr.mailAccount.title : tr.mailAccount.manualTitle}</h2><p className={typography.bodyMuted}>{step === "email" ? tr.mailAccount.emailFirstDescription : tr.mailAccount.manualDescription}</p></div>
          </div>
          <button type="button" onClick={requestClose} aria-label={tr.common.close} className="rounded-md p-1.5 text-zinc-500 hover:bg-white/5 hover:text-zinc-200"><X className="h-4 w-4" /></button>
        </div>

        {step === "email" ? (
          <form onSubmit={discover} className="space-y-4">
            <Field label={tr.mailAccount.email}><input autoFocus type="email" value={email} onChange={event => setEmail(event.target.value)} className={ui.input} autoComplete="email" placeholder={tr.mailAccount.emailPlaceholder} /></Field>
            <div className="flex items-start gap-2 rounded-lg border border-white/5 bg-white/[0.025] px-3 py-2.5 text-[11px] text-zinc-500"><LockKeyhole className="mt-0.5 h-3.5 w-3.5 shrink-0" /><span>{tr.mailAccount.oauthPrivacy}</span></div>
            {error && <div role="alert" className="rounded-lg border border-red-400/20 bg-red-400/10 px-3 py-2 text-xs text-red-300">{error}</div>}
            <button type="submit" disabled={!email.trim() || submitting} className={`${ui.buttonPrimary} w-full`}>{oauthPending ? tr.mailAccount.waitingForBrowser : submitting ? tr.mailAccount.detecting : tr.mailAccount.continue}</button>
            {oauthPending && (
              <div className="flex items-center justify-between gap-3 rounded-lg border border-white/5 bg-white/[0.025] px-3 py-2.5 text-[11px] text-zinc-500">
                <span>{tr.mailAccount.oauthWaitingHint}</span>
                <button type="button" onClick={() => void retryOAuth()} className="shrink-0 font-medium text-[var(--app-accent)] hover:underline">{tr.mailAccount.oauthRetry}</button>
              </div>
            )}
            <button type="button" onClick={() => openManual()} disabled={submitting} className="w-full text-center text-xs text-zinc-500 transition-colors hover:text-zinc-300">{tr.mailAccount.manualLink}</button>
          </form>
        ) : (
          <form onSubmit={submitManual} className="space-y-5">
            {provider === "manual" && (
              <div className="rounded-xl border border-white/5 bg-white/[0.02] p-3">
                <p className="mb-2 text-[11px] text-zinc-500">{tr.mailAccount.workAccountHint}</p>
                <div className="grid gap-2 sm:grid-cols-2">
                  <button type="button" disabled={submitting || !form.email.trim()} onClick={() => void startOAuthFromManual("google")} className={ui.buttonSecondary}>{tr.mailAccount.continueGoogle}</button>
                  <button type="button" disabled={submitting || !form.email.trim()} onClick={() => void startOAuthFromManual("microsoft")} className={ui.buttonSecondary}>{tr.mailAccount.continueMicrosoft}</button>
                </div>
              </div>
            )}
            <div className="grid gap-3 sm:grid-cols-2">
              <Field label={tr.mailAccount.email}><input autoFocus type="email" value={form.email} onChange={event => update("email", event.target.value)} className={ui.input} autoComplete="email" /></Field>
              <Field label={tr.mailAccount.username}><input value={form.username} onChange={event => update("username", event.target.value)} className={ui.input} autoComplete="username" /></Field>
            </div>
            <Field label={provider === "yahoo" || provider === "icloud" ? tr.mailAccount.appPassword : tr.mailAccount.password}><input type="password" value={form.password} onChange={event => update("password", event.target.value)} className={ui.input} autoComplete="current-password" /><p className="mt-1 text-[11px] text-zinc-600">{provider === "yahoo" || provider === "icloud" ? tr.mailAccount.appPasswordHint : tr.mailAccount.passwordHint}</p></Field>
            <ServerFields title={tr.mailAccount.incoming} hostLabel={tr.mailAccount.host} host={form.imapHost} onHost={value => update("imapHost", value)} port={form.imapPort} onPort={value => update("imapPort", value)} security={form.imapSecurity} onSecurity={value => update("imapSecurity", value)} tr={tr.mailAccount} />
            <ServerFields title={tr.mailAccount.outgoing} hostLabel={tr.mailAccount.host} host={form.smtpHost} onHost={value => update("smtpHost", value)} port={form.smtpPort} onPort={value => update("smtpPort", value)} security={form.smtpSecurity} onSecurity={value => update("smtpSecurity", value)} tr={tr.mailAccount} />
            {error && <div role="alert" className="rounded-lg border border-red-400/20 bg-red-400/10 px-3 py-2 text-xs text-red-300">{error}</div>}
            <div className="flex items-center justify-end gap-3 pt-1"><button type="button" onClick={requestClose} disabled={submitting} className={ui.buttonSecondary}>{tr.common.cancel}</button><button type="submit" disabled={!canSubmitManual || submitting} className={ui.buttonPrimary}>{submitting ? tr.mailAccount.testing : tr.mailAccount.add}</button></div>
          </form>
        )}
      </div>
    </div>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return <label className="block"><span className="mb-1.5 block text-xs font-medium text-zinc-400">{label}</span>{children}</label>;
}

function ServerFields({ title, hostLabel, host, onHost, port, onPort, security, onSecurity, tr }: { title: string; hostLabel: string; host: string; onHost: (value: string) => void; port: number; onPort: (value: number) => void; security: MailSecurity; onSecurity: (value: MailSecurity) => void; tr: { port: string; security: string; tls: string; starttls: string } }) {
  return <fieldset className="rounded-xl border border-white/5 bg-white/[0.02] p-4"><legend className="flex items-center gap-2 px-1 text-xs font-semibold text-zinc-300"><Server className="h-3.5 w-3.5" />{title}</legend><div className="mt-2 grid gap-3 sm:grid-cols-[1fr_88px_140px]"><Field label={hostLabel}><input value={host} onChange={event => onHost(event.target.value)} className={ui.input} /></Field><Field label={tr.port}><input type="number" min={1} max={65535} value={port} onChange={event => onPort(Number(event.target.value))} className={ui.input} /></Field><Field label={tr.security}><select value={security} onChange={event => onSecurity(event.target.value as MailSecurity)} className={ui.input}><option value="tls">{tr.tls}</option><option value="starttls">{tr.starttls}</option></select></Field></div></fieldset>;
}

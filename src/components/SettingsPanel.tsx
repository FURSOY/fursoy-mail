import { useCallback, useEffect, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { RefreshCw, DownloadCloud, Menu, LogOut, Plus, GripVertical, ExternalLink, ShieldCheck, Mail, ArrowUp, ArrowDown } from "lucide-react";
import { useLocale, type AppLanguage } from "../i18n";
import { themePresets, typography, ui, type ThemePresetName } from "../theme";
import { tauriApi } from "../tauriApi";
import type { Account, AppControls, DensityMode, NotificationMode, OtpMode, RemoteImageMode, RenderMode } from "../types";
import { MIN_SYNC_INTERVAL_SECONDS, normalizeSyncIntervalSeconds } from "../syncInterval";
import { ToolbarTip } from "./ToolbarTip";
import { ProfileAvatar } from "./ProfileAvatar";

const PRIVACY_POLICY_URL = "https://fursoy.com/privacy/";

interface SettingsPanelProps {
  isVisible: boolean;
  usesOverlaySidebar: boolean;
  onMenuOpen: () => void;

  themePreset: ThemePresetName;
  setThemePreset: (v: ThemePresetName) => void;
  densityMode: DensityMode;
  setDensityMode: (v: DensityMode) => void;

  syncIntervalValue: number;
  setSyncIntervalValue: (v: number) => void;

  launchAtStartup: boolean;
  startupSettingLoading: boolean;
  onLaunchAtStartupChange: (checked: boolean) => void;

  appControls: AppControls;
  onUpdateAppControls: (patch: Partial<AppControls>) => void;

  notifDuration: number;
  setNotifDuration: (v: number) => void;
  notifInfinite: boolean;
  setNotifInfinite: (v: boolean) => void;

  lazyBodyLoading: boolean;
  setLazyBodyLoading: (v: boolean) => void;
  renderMode: RenderMode;
  setRenderMode: (v: RenderMode) => void;
  remoteImageMode: RemoteImageMode;
  setRemoteImageMode: (v: RemoteImageMode) => void;
  otpMode: OtpMode;
  setOtpMode: (v: OtpMode) => void;
  appLanguage: AppLanguage;
  setAppLanguage: (v: AppLanguage) => void;
  pauseOnFullscreen: boolean;
  setPauseOnFullscreen: (v: boolean) => void;

  onResetLocalMailbox: () => void;
  isResettingLocalMailbox: boolean;
  onShowToast: (message: string, type?: "error" | "success" | "info") => void;

  currentVersion: string;
  isCheckingUpdate: boolean;
  updateAvailable: { version: string; date: string; body: string } | null;
  updateProgress: { downloaded: number; total: number } | null;
  updateError: string | null;
  updateStatus: string;
  onCheckForUpdates: (showUI: boolean) => void;
  onInstallUpdate: () => void;
  // multi-account
  accounts: Account[];
  onAddAccount: () => void;
  onLogoutAccount: (accountId: string) => void;
  onReorderAccounts: (orderedIds: string[]) => void;
}

export function SettingsPanel({
  isVisible, usesOverlaySidebar, onMenuOpen,
  themePreset, setThemePreset, densityMode, setDensityMode,
  syncIntervalValue, setSyncIntervalValue,
  launchAtStartup, startupSettingLoading, onLaunchAtStartupChange,
  appControls, onUpdateAppControls,
  notifDuration, setNotifDuration, notifInfinite, setNotifInfinite,
  lazyBodyLoading, setLazyBodyLoading, renderMode, setRenderMode, remoteImageMode, setRemoteImageMode,
  otpMode, setOtpMode, appLanguage, setAppLanguage, pauseOnFullscreen, setPauseOnFullscreen,
  onResetLocalMailbox,
  isResettingLocalMailbox,
  onShowToast,
  currentVersion, isCheckingUpdate, updateAvailable, updateProgress, updateError, updateStatus,
  onCheckForUpdates, onInstallUpdate,
  accounts, onAddAccount, onLogoutAccount, onReorderAccounts,
}: SettingsPanelProps) {
  const tr = useLocale();
  const [dragIndex, setDragIndex] = useState<number | null>(null);
  const [dragOverIndex, setDragOverIndex] = useState<number | null>(null);
  const [defaultMailLoading, setDefaultMailLoading] = useState(false);
  const dragStateRef = useRef({ from: null as number | null, over: null as number | null });

  const openDefaultMailSettings = useCallback(async () => {
    setDefaultMailLoading(true);
    try {
      await tauriApi.openDefaultMailSettings();
    } catch (error) {
      console.error("Failed to open default mail settings:", error);
      onShowToast(tr.defaultMail.failed, "error");
    } finally {
      setDefaultMailLoading(false);
    }
  }, [onShowToast, tr.defaultMail.failed]);

  const startDrag = useCallback((index: number) => {
    dragStateRef.current = { from: index, over: null };
    setDragIndex(index);
    setDragOverIndex(null);
  }, []);

  const moveAccount = useCallback((from: number, to: number) => {
    if (to < 0 || to >= accounts.length || from === to) return;
    const reordered = [...accounts];
    const [moved] = reordered.splice(from, 1);
    reordered.splice(to, 0, moved);
    onReorderAccounts(reordered.map(account => account.id));
  }, [accounts, onReorderAccounts]);

  useEffect(() => {
    if (dragIndex === null) return;
    const handleMove = (e: PointerEvent) => {
      for (const el of document.elementsFromPoint(e.clientX, e.clientY)) {
        const idx = (el as HTMLElement).dataset?.accountIdx;
        if (idx !== undefined) {
          const n = parseInt(idx);
          dragStateRef.current.over = n;
          setDragOverIndex(n);
          break;
        }
      }
    };
    const handleUp = () => {
      const { from, over } = dragStateRef.current;
      if (from !== null && over !== null && from !== over) {
        moveAccount(from, over);
      }
      dragStateRef.current = { from: null, over: null };
      setDragIndex(null);
      setDragOverIndex(null);
    };
    window.addEventListener("pointermove", handleMove);
    window.addEventListener("pointerup", handleUp);
    return () => {
      window.removeEventListener("pointermove", handleMove);
      window.removeEventListener("pointerup", handleUp);
    };
  }, [dragIndex, moveAccount]);

  return (
    <section
      className="flex-1 overflow-y-scroll overscroll-contain bg-[var(--color-surface-content)] p-8"
      style={isVisible ? { contain: "paint" } : { display: "none" }}
    >
      <div className="max-w-2xl mx-auto">
        <h2 className={`${typography.pageTitle} mb-6 flex items-center gap-2`}>
          {usesOverlaySidebar && (
            <ToolbarTip label={tr.settings.openMenu}>
              <button
                type="button"
                onClick={onMenuOpen}
                className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md text-zinc-500 hover:bg-white/10 hover:text-zinc-200"
              >
                <Menu className="h-4 w-4" />
              </button>
            </ToolbarTip>
          )}
          {tr.nav.settings}
        </h2>

        <div className="space-y-8">
          {/* Accounts */}
          <div className="bg-white/[0.02] border border-white/5 rounded-xl p-5">
            <h3 className="text-sm font-semibold text-zinc-200 mb-1">{tr.accounts.title}</h3>
            <p className="text-xs text-zinc-500 mb-4">{tr.accounts.description}</p>
            <div className="space-y-1.5">
              {accounts.map((acc, i) => (
                <div
                  key={acc.id}
                  data-account-idx={i}
                  className={`flex items-center gap-3 p-3 rounded-lg border transition-all select-none ${
                    dragOverIndex === i && dragIndex !== i
                      ? "border-[var(--app-accent)] bg-[var(--app-accent-soft)]"
                      : dragIndex === i
                      ? "border-white/10 bg-white/[0.05] opacity-50"
                      : "border-white/5 bg-white/[0.02]"
                  }`}
                >
                  <button
                    type="button"
                    onPointerDown={(event) => { event.preventDefault(); startDrag(i); }}
                    aria-label={`${acc.email}: ${tr.accounts.description}`}
                    className={`shrink-0 rounded p-1 text-zinc-600 hover:bg-white/5 hover:text-zinc-300 ${dragIndex === i ? "cursor-grabbing" : "cursor-grab"}`}
                  >
                    <GripVertical className="w-4 h-4" />
                  </button>
                  <ProfileAvatar picture={acc.picture} email={acc.email} alt={acc.email} className="w-8 h-8 rounded-full object-cover shrink-0 text-sm" />
                  <div className="flex-1 min-w-0">
                    <div className="text-sm font-medium text-zinc-200 truncate">{acc.email.split("@")[0]}</div>
                    <div className="text-xs text-zinc-500 truncate">{acc.email}</div>
                    {i === 0 && (
                      <div className="text-[10px] text-[var(--app-accent)] font-medium mt-0.5">{tr.accounts.primary}</div>
                    )}
                  </div>
                  <div className="flex shrink-0 items-center gap-0.5">
                    <button
                      type="button"
                      onClick={() => moveAccount(i, i - 1)}
                      disabled={i === 0}
                      aria-label={`${tr.accounts.moveUp}: ${acc.email}`}
                      className="rounded p-1 text-zinc-500 hover:bg-white/5 hover:text-zinc-200 disabled:opacity-25"
                    >
                      <ArrowUp className="h-3.5 w-3.5" />
                    </button>
                    <button
                      type="button"
                      onClick={() => moveAccount(i, i + 1)}
                      disabled={i === accounts.length - 1}
                      aria-label={`${tr.accounts.moveDown}: ${acc.email}`}
                      className="rounded p-1 text-zinc-500 hover:bg-white/5 hover:text-zinc-200 disabled:opacity-25"
                    >
                      <ArrowDown className="h-3.5 w-3.5" />
                    </button>
                  </div>
                  <button
                    type="button"
                    onClick={() => onLogoutAccount(acc.id)}
                    className="flex items-center gap-1.5 px-2.5 py-1 text-xs text-zinc-500 hover:text-red-400 hover:bg-red-400/10 rounded-md transition-colors shrink-0"
                  >
                    <LogOut className="w-3.5 h-3.5" />
                    {tr.accounts.signOut}
                  </button>
                </div>
              ))}
              <button
                onClick={onAddAccount}
                className="w-full flex items-center gap-2.5 px-3 py-2.5 rounded-lg border border-dashed border-white/10 text-zinc-500 hover:text-zinc-300 hover:border-white/20 transition-colors"
              >
                <Plus className="w-4 h-4" />
                <span className="text-sm">{tr.accounts.add}</span>
              </button>
            </div>
          </div>

          {/* Appearance */}
          <div className="bg-white/[0.02] border border-white/5 rounded-xl p-5">
            <h3 className="text-sm font-semibold text-zinc-200 mb-1">{tr.appearance.title}</h3>
            <p className="text-xs text-zinc-500 mb-4">{tr.appearance.description}</p>

            <div className="space-y-5">
              <div>
                <div className="text-xs font-medium text-zinc-300 mb-2">{tr.appearance.accentColor}</div>
                <div className="flex flex-wrap gap-2">
                  {(Object.keys(themePresets) as ThemePresetName[]).map((name) => {
                    const preset = themePresets[name];
                    const active = themePreset === name;
                    return (
                      <button
                        key={name}
                        type="button"
                        onClick={() => {
                          setThemePreset(name);
                          localStorage.setItem("fursoy_theme_preset", name);
                        }}
                        className={`flex items-center gap-2 rounded-lg border px-3 py-2 text-xs transition-all ${
                          active
                            ? "border-[var(--app-accent)] bg-[var(--app-accent-soft)] text-zinc-100"
                            : "border-[var(--color-border-default)] bg-[var(--color-surface-app)] text-zinc-400 hover:bg-white/5 hover:text-zinc-200"
                        }`}
                      >
                        <span className="h-3.5 w-3.5 rounded-full" style={{ backgroundColor: preset.accent }} />
                        {tr.appearance.colors[name]}
                      </button>
                    );
                  })}
                </div>
              </div>

              <div>
                <div className="text-xs font-medium text-zinc-300 mb-2">{tr.appearance.density}</div>
                <div className="inline-flex rounded-[var(--radius-md)] border border-[var(--color-border-default)] bg-[var(--color-surface-app)] p-1">
                  {(["comfortable", "compact"] as DensityMode[]).map((mode) => (
                    <button
                      key={mode}
                      type="button"
                      onClick={() => {
                        setDensityMode(mode);
                        localStorage.setItem("fursoy_density_mode", mode);
                      }}
                      className={`px-3 py-1.5 text-xs rounded-md transition-colors ${
                        densityMode === mode ? "bg-white/10 text-zinc-100" : "text-zinc-500 hover:text-zinc-300"
                      }`}
                    >
                      {mode === "comfortable" ? tr.appearance.comfortable : tr.appearance.compact}
                    </button>
                  ))}
                </div>
              </div>
            </div>
          </div>

          {/* General */}
          <div className={`${ui.card} p-5`}>
            <h3 className={`${typography.sectionTitle} mb-1`}>{tr.settings.generalTitle}</h3>
            <p className={`${typography.bodyMuted} mb-5`}>{tr.settings.generalDescription}</p>
            <div className="grid gap-5 sm:grid-cols-2">
              <div>
                <div className="mb-2 text-xs font-medium text-[var(--color-text-secondary)]">{tr.language.label}</div>
                <div className="inline-flex rounded-[var(--radius-md)] border border-[var(--color-border-default)] bg-[var(--color-surface-app)] p-1">
                  {(["en", "tr"] as AppLanguage[]).map((lang) => (
                    <button key={lang} type="button" onClick={() => setAppLanguage(lang)} className={`rounded-[var(--radius-sm)] px-3 py-1.5 text-xs transition-colors ${appLanguage === lang ? "bg-[var(--color-surface-hover-strong)] text-[var(--color-text-primary)]" : "text-[var(--color-text-subtle)] hover:text-[var(--color-text-secondary)]"}`}>
                      {tr.language[lang]}
                    </button>
                  ))}
                </div>
              </div>
              <div>
                <div className="mb-2 text-xs font-medium text-[var(--color-text-secondary)]">{tr.startup.title}</div>
                <label className={`flex items-center gap-2 ${startupSettingLoading ? "opacity-60" : "cursor-pointer"}`}>
                  <input type="checkbox" checked={launchAtStartup} disabled={startupSettingLoading} onChange={(e) => onLaunchAtStartupChange(e.target.checked)} className="h-4 w-4 rounded border-[var(--color-border-strong)] bg-[var(--color-surface-app)] text-[var(--app-accent)] focus:ring-0 focus:ring-offset-0" />
                  <span className="text-sm text-[var(--color-text-secondary)]">{tr.startup.launchAtStartup}</span>
                </label>
              </div>
              <div className="sm:col-span-2 rounded-[var(--radius-md)] border border-[var(--color-border-default)] bg-[var(--color-surface-app)] p-3">
                <div className="flex flex-wrap items-center gap-3">
                  <div className="rounded-[var(--radius-sm)] bg-[var(--app-accent-soft)] p-2 text-[var(--app-accent)]">
                    <Mail className="h-4 w-4" />
                  </div>
                  <div className="min-w-0 flex-1">
                    <div className="text-xs font-medium text-[var(--color-text-secondary)]">{tr.defaultMail.title}</div>
                    <p className="mt-1 text-[10px] leading-relaxed text-[var(--color-text-subtle)]">{tr.defaultMail.description}</p>
                  </div>
                  <button type="button" onClick={() => void openDefaultMailSettings()} disabled={defaultMailLoading} className={ui.buttonSecondary}>
                    {defaultMailLoading ? tr.defaultMail.opening : tr.defaultMail.action}
                  </button>
                </div>
              </div>
            </div>
          </div>

          {/* Notifications and OTP */}
          <div className={`${ui.card} p-5`}>
            <h3 className={`${typography.sectionTitle} mb-1`}>{tr.notifications.title}</h3>
            <p className={`${typography.bodyMuted} mb-5`}>{tr.notifications.description}</p>
            <div className="space-y-6">
              <div>
                <div className="mb-2 text-xs font-medium text-[var(--color-text-secondary)]">{tr.notifications.deliveryTitle}</div>
                <div className="grid gap-2 sm:grid-cols-3">
                  {(["all", "otpOnly", "off"] as NotificationMode[]).map((mode) => {
                    const label = mode === "all" ? tr.notifications.all : mode === "otpOnly" ? tr.notifications.otpOnly : tr.notifications.off;
                    const description = mode === "all" ? tr.notifications.allDescription : mode === "otpOnly" ? tr.notifications.otpOnlyDescription : tr.notifications.offDescription;
                    const active = appControls.notificationMode === mode;
                    return (
                      <label key={mode} className={`cursor-pointer rounded-[var(--radius-md)] border p-3 transition-colors ${active ? "border-[var(--app-accent)] bg-[var(--app-accent-soft)]" : "border-[var(--color-border-default)] bg-[var(--color-surface-app)] hover:bg-[var(--color-surface-hover)]"}`}>
                        <div className="flex items-start gap-2">
                          <input
                            type="radio"
                            name="notification-mode"
                            checked={active}
                            onChange={() => {
                              onUpdateAppControls({ notificationMode: mode });
                              if (mode === "otpOnly" && otpMode === "off") {
                                setOtpMode("balanced");
                                localStorage.setItem("fursoy_otp_mode", "balanced");
                              }
                            }}
                            className="mt-0.5 h-3.5 w-3.5 border-[var(--color-border-strong)] bg-[var(--color-surface-app)] text-[var(--app-accent)] focus:ring-0 focus:ring-offset-0"
                          />
                          <div>
                            <div className="text-xs font-medium text-[var(--color-text-secondary)]">{label}</div>
                            <p className="mt-1 text-[10px] leading-relaxed text-[var(--color-text-subtle)]">{description}</p>
                          </div>
                        </div>
                      </label>
                    );
                  })}
                </div>
              </div>

              <div className="grid gap-5 border-t border-[var(--color-border-subtle)] pt-5 sm:grid-cols-2">
                <div>
                  <div className="mb-1 text-xs font-medium text-[var(--color-text-secondary)]">{tr.settings.otpDetection}</div>
                  <div className="inline-flex rounded-[var(--radius-md)] border border-[var(--color-border-default)] bg-[var(--color-surface-app)] p-1">
                    {(["off", "balanced", "strict"] as OtpMode[]).map((mode) => (
                      <button key={mode} type="button" onClick={() => { setOtpMode(mode); localStorage.setItem("fursoy_otp_mode", mode); }} className={`rounded-[var(--radius-sm)] px-3 py-1.5 text-xs transition-colors ${otpMode === mode ? "bg-[var(--color-surface-hover-strong)] text-[var(--color-text-primary)]" : "text-[var(--color-text-subtle)] hover:text-[var(--color-text-secondary)]"}`}>
                        {mode === "off" ? tr.settings.otpOff : mode === "balanced" ? tr.settings.otpBalanced : tr.settings.otpStrict}
                      </button>
                    ))}
                  </div>
                </div>
                <div>
                  <div className="mb-1 text-xs font-medium text-[var(--color-text-secondary)]">{tr.settings.notificationDurationTitle}</div>
                  <div className="flex flex-wrap items-center gap-3">
                    <label className="flex cursor-pointer items-center gap-2">
                      <input type="checkbox" checked={notifInfinite} onChange={(e) => { setNotifInfinite(e.target.checked); localStorage.setItem("fursoy_notif_infinite", e.target.checked.toString()); }} className="h-4 w-4 rounded border-[var(--color-border-strong)] bg-[var(--color-surface-app)] text-[var(--app-accent)] focus:ring-0 focus:ring-offset-0" />
                      <span className="text-xs text-[var(--color-text-secondary)]">{tr.settings.keepOnScreen}</span>
                    </label>
                    <div className={`flex items-center gap-2 ${notifInfinite ? "pointer-events-none opacity-40" : ""}`}>
                      <input type="number" min="1" max="60" value={notifDuration} disabled={notifInfinite} onChange={(e) => { const val = parseInt(e.target.value, 10) || 1; setNotifDuration(val); localStorage.setItem("fursoy_notif_duration", val.toString()); }} className="w-20 rounded-[var(--radius-md)] border border-[var(--color-border-default)] bg-[var(--color-surface-app)] px-2 py-1.5 text-sm text-[var(--color-text-secondary)] outline-none focus:border-[var(--app-accent)]/50" />
                      <span className="text-xs text-[var(--color-text-subtle)]">{tr.common.seconds}</span>
                    </div>
                  </div>
                </div>
              </div>

              <div className="rounded-[var(--radius-md)] border border-[var(--color-border-subtle)] bg-[var(--color-surface-app)] p-3">
                <label className="flex cursor-pointer items-center gap-2">
                  <input type="checkbox" checked={appControls.quietHoursEnabled} onChange={(e) => onUpdateAppControls({ quietHoursEnabled: e.target.checked })} className="h-4 w-4 rounded border-[var(--color-border-strong)] bg-[var(--color-surface-app)] text-[var(--app-accent)] focus:ring-0 focus:ring-offset-0" />
                  <span className="text-sm text-[var(--color-text-secondary)]">{tr.notifications.quietHours}</span>
                </label>
                <p className="mt-1 text-[10px] text-[var(--color-text-subtle)]">{tr.notifications.quietHoursHint}</p>
                <div className={`mt-3 grid grid-cols-2 gap-3 ${appControls.quietHoursEnabled ? "" : "opacity-40"}`}>
                  {(["start", "end"] as const).map((edge) => (
                    <label key={edge} className="space-y-1">
                      <span className="text-[10px] text-[var(--color-text-subtle)]">{tr.notifications[edge]}</span>
                      <input type="time" value={edge === "start" ? appControls.quietHoursStart : appControls.quietHoursEnd} disabled={!appControls.quietHoursEnabled} onChange={(e) => onUpdateAppControls(edge === "start" ? { quietHoursStart: e.target.value } : { quietHoursEnd: e.target.value })} className="w-full rounded-[var(--radius-md)] border border-[var(--color-border-default)] bg-[var(--color-surface-control)] px-2 py-1.5 text-xs text-[var(--color-text-secondary)] outline-none focus:border-[var(--app-accent)]/50" />
                    </label>
                  ))}
                </div>
              </div>
            </div>
          </div>

          {/* Sync and performance */}
          <div className={`${ui.card} p-5`}>
            <h3 className={`${typography.sectionTitle} mb-1`}>{tr.settings.syncPerformanceTitle}</h3>
            <p className={`${typography.bodyMuted} mb-5`}>{tr.settings.syncPerformanceDescription}</p>
            <div className="space-y-5">
              <div>
                <div className="mb-2 text-xs font-medium text-[var(--color-text-secondary)]">{tr.settings.syncFrequencyTitle}</div>
                <div className="flex items-center gap-3">
                  <input type="number" min={MIN_SYNC_INTERVAL_SECONDS} value={syncIntervalValue} onChange={(e) => { const val = normalizeSyncIntervalSeconds(e.target.value, MIN_SYNC_INTERVAL_SECONDS); setSyncIntervalValue(val); localStorage.setItem("fursoy_sync_interval", val.toString()); }} className="w-24 rounded-[var(--radius-md)] border border-[var(--color-border-default)] bg-[var(--color-surface-app)] px-3 py-1.5 text-sm text-[var(--color-text-secondary)] outline-none focus:border-[var(--app-accent)]/50" />
                  <span className="text-sm text-[var(--color-text-subtle)]">{tr.common.seconds}</span>
                </div>
              </div>
              <label className="flex cursor-pointer items-center gap-2">
                <input type="checkbox" checked={appControls.mailSyncPaused} onChange={(e) => onUpdateAppControls({ mailSyncPaused: e.target.checked })} className="h-4 w-4 rounded border-[var(--color-border-strong)] bg-[var(--color-surface-app)] text-[var(--app-accent)] focus:ring-0 focus:ring-offset-0" />
                <span className="text-sm text-[var(--color-text-secondary)]">{tr.notifications.pauseMailSync}</span>
              </label>
              <label className="flex cursor-pointer items-center gap-2">
                <input type="checkbox" checked={pauseOnFullscreen} onChange={(e) => { setPauseOnFullscreen(e.target.checked); localStorage.setItem("fursoy_pause_on_fullscreen", e.target.checked.toString()); }} className="h-4 w-4 rounded border-[var(--color-border-strong)] bg-[var(--color-surface-app)] text-[var(--app-accent)] focus:ring-0 focus:ring-offset-0" />
                <span className="text-sm text-[var(--color-text-secondary)]">{tr.settings.pauseInFullscreen}</span>
              </label>
              <label className="flex cursor-pointer items-center gap-2">
                <input type="checkbox" checked={lazyBodyLoading} onChange={(e) => { setLazyBodyLoading(e.target.checked); localStorage.setItem("fursoy_lazy_body_loading", e.target.checked.toString()); }} className="h-4 w-4 rounded border-[var(--color-border-strong)] bg-[var(--color-surface-app)] text-[var(--app-accent)] focus:ring-0 focus:ring-offset-0" />
                <span className="text-sm text-[var(--color-text-secondary)]">{tr.settings.lazyEmailContent}</span>
              </label>
            </div>
          </div>

          {/* Mail content and local data */}
          <div className={`${ui.card} p-5`}>
            <h3 className={`${typography.sectionTitle} mb-1`}>{tr.settings.mailContentTitle}</h3>
            <p className={`${typography.bodyMuted} mb-5`}>{tr.settings.mailContentDescription}</p>
            <div className="space-y-6">
              <div>
                <div className="mb-2 text-xs font-medium text-[var(--color-text-secondary)]">{tr.remoteImages.title}</div>
                <div className="grid gap-2 sm:grid-cols-3">
                  {(["always", "trusted", "ask"] as RemoteImageMode[]).map((mode) => (
                    <label key={mode} className={`flex cursor-pointer items-center gap-2 rounded-[var(--radius-md)] border px-3 py-2 text-xs transition-colors ${remoteImageMode === mode ? "border-[var(--app-accent)] bg-[var(--app-accent-soft)]" : "border-[var(--color-border-default)] bg-[var(--color-surface-app)] hover:bg-[var(--color-surface-hover)]"}`}>
                      <input type="radio" name="remote-image-mode" checked={remoteImageMode === mode} onChange={() => { setRemoteImageMode(mode); localStorage.setItem("fursoy_remote_image_mode", mode); }} className="h-3.5 w-3.5 border-[var(--color-border-strong)] bg-[var(--color-surface-app)] text-[var(--app-accent)] focus:ring-0 focus:ring-offset-0" />
                      <span className="text-[var(--color-text-secondary)]">{tr.remoteImages[mode]}</span>
                    </label>
                  ))}
                </div>
              </div>
              <div>
                <div className="mb-2 text-xs font-medium text-[var(--color-text-secondary)]">{tr.settings.htmlRenderMode}</div>
                <div className="inline-flex rounded-[var(--radius-md)] border border-[var(--color-border-default)] bg-[var(--color-surface-app)] p-1">
                  {(["full", "simple"] as RenderMode[]).map((mode) => (
                    <button key={mode} type="button" onClick={() => { setRenderMode(mode); localStorage.setItem("fursoy_render_mode", mode); }} className={`rounded-[var(--radius-sm)] px-3 py-1.5 text-xs transition-colors ${renderMode === mode ? "bg-[var(--color-surface-hover-strong)] text-[var(--color-text-primary)]" : "text-[var(--color-text-subtle)] hover:text-[var(--color-text-secondary)]"}`}>
                      {mode === "full" ? tr.settings.fullHtml : tr.settings.simpleHtml}
                    </button>
                  ))}
                </div>
              </div>
              <div className="rounded-[var(--radius-md)] border border-[var(--color-status-warning)] bg-[var(--color-status-warning-soft)] p-3">
                <div className="text-xs font-medium text-[var(--color-text-secondary)]">{tr.localMailbox.title}</div>
                <p className="mt-1 text-[10px] leading-relaxed text-[var(--color-text-subtle)]">{tr.localMailbox.description}</p>
                <button type="button" onClick={onResetLocalMailbox} disabled={isResettingLocalMailbox} className="mt-3 rounded-[var(--radius-sm)] border border-[var(--color-status-warning)] bg-[var(--color-status-warning-soft)] px-3 py-1.5 text-xs text-[var(--color-status-warning)] transition-colors hover:bg-[var(--color-status-warning-soft)] disabled:cursor-wait disabled:opacity-60">
                  {isResettingLocalMailbox ? tr.localMailbox.resetting : tr.localMailbox.reset}
                </button>
              </div>
            </div>
          </div>

          {/* Privacy and data */}
          <div className={`${ui.card} p-5`}>
            <div className="flex flex-wrap items-center gap-3">
              <div className="rounded-[var(--radius-md)] bg-[var(--app-accent-soft)] p-2 text-[var(--app-accent)]">
                <ShieldCheck className="h-4 w-4" />
              </div>
              <h3 className={`${typography.sectionTitle} min-w-0 flex-1`}>{tr.settings.privacyDataTitle}</h3>
              <button
                type="button"
                onClick={() => void openUrl(PRIVACY_POLICY_URL).catch(() => undefined)}
                className={`${ui.buttonSecondary} inline-flex items-center gap-2`}
              >
                {tr.settings.privacyPolicy}
                <ExternalLink className="h-3.5 w-3.5" />
              </button>
            </div>
          </div>

          {/* Updates */}
          <div id="settings-updates" className="bg-white/[0.02] border border-white/5 rounded-xl p-5">
            <h3 className={`${typography.sectionTitle} mb-1`}>{tr.update.title}</h3>
            <p className={`${typography.bodyMuted} mb-4`}>{tr.update.description}</p>
            <div className="mb-4 flex items-center justify-between rounded-lg border border-white/5 bg-[#09090b] px-3 py-2">
              <span className="text-xs text-zinc-500">{tr.update.currentVersion}</span>
              <span className="text-xs font-semibold text-zinc-200">v{currentVersion || "..."}</span>
            </div>

            <div className="space-y-4">
              {!updateProgress ? (
                <div className="flex items-center gap-3">
                  <button
                    onClick={() => onCheckForUpdates(true)}
                    disabled={isCheckingUpdate}
                    className={`${ui.buttonSecondary} flex items-center gap-2`}
                  >
                    {isCheckingUpdate ? <RefreshCw className="w-4 h-4 animate-spin" /> : <DownloadCloud className="w-4 h-4" />}
                    {isCheckingUpdate ? tr.update.checking : tr.update.check}
                  </button>
                  {updateAvailable && (
                    <button onClick={onInstallUpdate} className={ui.buttonPrimary}>
                      {tr.update.installVersion.replace("{version}", updateAvailable.version)}
                    </button>
                  )}
                </div>
              ) : (
                <div className="bg-[#09090b] border border-white/10 rounded-lg p-4">
                  <div className="flex items-center justify-between text-xs text-zinc-300 mb-2">
                    <span className="font-medium text-blue-400">{tr.update.downloading}</span>
                    <span>
                      {updateProgress.total > 0
                        ? Math.round((updateProgress.downloaded / updateProgress.total) * 100)
                        : 0}%
                    </span>
                  </div>
                  <div className="h-1.5 w-full bg-white/5 rounded-full overflow-hidden">
                    <div
                      className="h-full bg-blue-500 transition-all duration-300"
                      style={{ width: `${updateProgress.total > 0 ? (updateProgress.downloaded / updateProgress.total) * 100 : 0}%` }}
                    />
                  </div>
                  <p className="text-[10px] text-zinc-500 mt-2">{tr.update.restartHint}</p>
                </div>
              )}
              {updateError && <p className="text-xs text-red-400 font-medium">{updateError}</p>}
              {updateStatus && !updateError && <p className="text-xs text-emerald-400 font-medium">{updateStatus}</p>}
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}

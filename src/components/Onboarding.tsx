import { ShieldCheck, Cpu, Zap, Mail } from "lucide-react";
import { useLocale } from "../i18n";
import { typography, ui } from "../theme";
import { WindowTitlebar } from "./WindowTitlebar";

interface OnboardingProps {
  onConnect: () => void;
  onCancelConnect: () => void;
  isConnecting: boolean;
  isWindowMaximized: boolean;
  onWindowMaximizedChange: (maximized: boolean) => void;
}

export function Onboarding({
  onConnect,
  onCancelConnect,
  isConnecting,
  isWindowMaximized,
  onWindowMaximizedChange,
}: OnboardingProps) {
  const tr = useLocale();
  return (
    <div className="flex h-screen flex-col overflow-hidden bg-[var(--color-surface-content)] select-none">
      <WindowTitlebar
        isMaximized={isWindowMaximized}
        onMaximizedChange={onWindowMaximizedChange}
      />
      <div className="flex min-h-0 flex-1 items-center justify-center overflow-auto px-8 py-8">
        <div className="w-full max-w-sm flex flex-col items-center gap-8">

          {/* Logo */}
          <div className="flex flex-col items-center gap-3">
            <img
              src="/logo.svg"
              className="h-14 w-14 rounded-[var(--radius-xl)] shadow-[var(--shadow-accent-lg)]"
              alt={tr.app.name}
            />
            <div className="text-center">
              <h1 className={`${typography.title} tracking-tight`}>{tr.app.name}</h1>
              <p className={`${typography.bodyMuted} mt-0.5`}>{tr.onboarding.tagline}</p>
            </div>
          </div>

          {/* Features */}
          <div className="w-full flex flex-col gap-3">
            <Feature icon={<Zap className="h-4 w-4 text-[var(--app-accent)]" />} text={tr.onboarding.otpFeature} />
            <Feature icon={<ShieldCheck className="h-4 w-4 text-[var(--app-accent)]" />} text={tr.onboarding.privacyFeature} />
            <Feature icon={<Cpu className="h-4 w-4 text-[var(--app-accent)]" />} text={tr.onboarding.performanceFeature} />
          </div>

          {/* CTA */}
          <div className="w-full flex flex-col items-center gap-3">
            <button
              onClick={isConnecting ? onCancelConnect : onConnect}
              className={`${ui.buttonPrimary} flex w-full items-center justify-center gap-2 py-2.5`}
            >
              {isConnecting ? tr.auth.cancelSignIn : (
                <>
                  <Mail className="h-4 w-4" />
                  {tr.onboarding.connect}
                </>
              )}
            </button>
            <p className={`${typography.bodyMuted} text-center`}>
              {tr.onboarding.privacy}
            </p>
          </div>
        </div>
      </div>
    </div>
  );
}
function Feature({ icon, text }: { icon: React.ReactNode; text: string }) {
  return (
    <div className={`${ui.card} flex items-center gap-3 px-4 py-3`}>
      <div className="shrink-0">{icon}</div>
      <span className={typography.body}>{text}</span>
    </div>
  );
}

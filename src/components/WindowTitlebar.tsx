import { getCurrentWindow } from "@tauri-apps/api/window";
import { Copy, Minus, Square, X } from "lucide-react";
import { useLocale } from "../i18n";
import { surfaces } from "../theme";
import { ToolbarTip } from "./ToolbarTip";

interface WindowTitlebarProps {
  isMaximized: boolean;
  onMaximizedChange: (maximized: boolean) => void;
  onMouseDown?: React.MouseEventHandler<HTMLDivElement>;
}

export function WindowTitlebar({
  isMaximized,
  onMaximizedChange,
  onMouseDown,
}: WindowTitlebarProps) {
  const tr = useLocale();

  return (
    <div
      data-tauri-drag-region
      className={`relative z-[60] flex h-9 shrink-0 items-center justify-between border-b border-[var(--color-border-subtle)] pl-2 pr-0 ${surfaces.app}`}
      style={{ WebkitAppRegion: "drag" } as React.CSSProperties}
      onMouseDown={onMouseDown}
    >
      <div data-tauri-drag-region className="flex items-center gap-2 pl-1 text-xs font-medium text-[var(--color-text-muted)]">
        <img src="/logo.svg" className="h-4 w-4 object-contain" alt={tr.app.name} />
        <span className="text-[var(--color-text-secondary)]">{tr.app.name}</span>
      </div>
      <div className="flex items-center" style={{ WebkitAppRegion: "no-drag" } as React.CSSProperties}>
        <ToolbarTip label={tr.common.minimize}>
          <button
            type="button"
            onClick={() => getCurrentWindow().minimize()}
            className="flex h-9 w-11 items-center justify-center text-[var(--color-text-muted)] transition-colors hover:bg-[var(--color-surface-hover)] hover:text-[var(--color-text-secondary)]"
          >
            <Minus className="h-4 w-4" />
          </button>
        </ToolbarTip>
        <ToolbarTip label={isMaximized ? tr.common.restore : tr.common.maximize}>
          <button
            type="button"
            onClick={async () => {
              const win = getCurrentWindow();
              await win.toggleMaximize();
              onMaximizedChange(await win.isMaximized());
            }}
            className="flex h-9 w-11 items-center justify-center text-[var(--color-text-muted)] transition-colors hover:bg-[var(--color-surface-hover)] hover:text-[var(--color-text-secondary)]"
          >
            {isMaximized ? <Copy className="h-3.5 w-3.5" /> : <Square className="h-3 w-3" />}
          </button>
        </ToolbarTip>
        <ToolbarTip label={tr.common.close}>
          <button
            type="button"
            onClick={async () => { await getCurrentWindow().hide(); }}
            className="flex h-9 w-11 items-center justify-center text-[var(--color-text-muted)] transition-colors hover:bg-[var(--color-status-danger)] hover:text-white"
          >
            <X className="h-4 w-4" />
          </button>
        </ToolbarTip>
      </div>
    </div>
  );
}

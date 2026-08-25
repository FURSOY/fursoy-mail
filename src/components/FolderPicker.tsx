import { useEffect, useRef, useState } from "react";
import { FolderInput } from "lucide-react";
import { useLocale } from "../i18n";
import type { CustomMailbox } from "../types";
import { ToolbarTip } from "./ToolbarTip";

/**
 * Moves the open conversation into one of the account's own IMAP folders. The
 * fixed folders keep their own toolbar buttons, since each has its own meaning;
 * this is the one list that grows with what the user made on the server.
 */
export function FolderPicker({
  mailboxes,
  currentRole,
  disabled,
  onMove,
}: {
  mailboxes: CustomMailbox[];
  currentRole: string;
  disabled?: boolean;
  onMove: (mailbox: CustomMailbox) => Promise<void>;
}) {
  const tr = useLocale();
  const rootRef = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const [pendingRole, setPendingRole] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    const close = (event: MouseEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", escape);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", escape);
    };
  }, [open]);

  const targets = mailboxes.filter(mailbox => mailbox.role !== currentRole);

  return (
    <div ref={rootRef} className="relative inline-flex items-center">
      <ToolbarTip label={tr.actions.moveToFolder}>
        <button
          type="button"
          disabled={disabled}
          onClick={() => setOpen(value => !value)}
          aria-expanded={open}
          className="rounded-md p-2 text-zinc-400 transition-colors hover:bg-white/5 hover:text-[var(--app-accent)] disabled:cursor-not-allowed disabled:opacity-40"
        >
          <FolderInput className="h-4 w-4" />
        </button>
      </ToolbarTip>
      {open && (
        <div className="absolute right-0 top-full z-[120] mt-1 w-64 overflow-hidden rounded-lg border border-white/10 bg-[var(--color-surface-popover)] shadow-2xl">
          <div className="max-h-64 overflow-y-auto p-1.5">
            {targets.length === 0 ? (
              <div className="px-2 py-3 text-center text-xs text-zinc-600">{tr.actions.noFolders}</div>
            ) : targets.map(mailbox => (
              <button
                key={mailbox.role}
                type="button"
                disabled={pendingRole === mailbox.role}
                title={mailbox.name}
                onClick={async () => {
                  setPendingRole(mailbox.role);
                  try {
                    await onMove(mailbox);
                    setOpen(false);
                  } catch {
                    /* The parent action already reports the failure. */
                  } finally {
                    setPendingRole(null);
                  }
                }}
                className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs text-zinc-300 hover:bg-white/5 disabled:opacity-50"
              >
                <FolderInput className="h-3 w-3 shrink-0 text-zinc-600" />
                <span className="min-w-0 flex-1 truncate">{mailbox.name}</span>
              </button>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

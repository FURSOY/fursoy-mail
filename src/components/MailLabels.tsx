import { useEffect, useMemo, useRef, useState } from "react";
import { Check, ChevronLeft, Plus, Search, Tag, X } from "lucide-react";
import { useLocale } from "../i18n";
import { nestedLabelName } from "../labelHierarchy";
import type { GmailLabel } from "../types";
import { ToolbarTip } from "./ToolbarTip";

function safeColor(value: string | null, fallback: string): string {
  return value && /^#[0-9a-f]{6}$/i.test(value) ? value : fallback;
}

export function LabelChips({
  labels,
  labelIds,
  max = 3,
  compact = false,
  onRemove,
}: {
  labels: GmailLabel[];
  labelIds: string[];
  max?: number;
  compact?: boolean;
  onRemove?: (labelId: string) => Promise<void>;
}) {
  const tr = useLocale();
  const [removingId, setRemovingId] = useState<string | null>(null);
  const visible = labelIds
    .map(id => labels.find(label => label.id === id))
    .filter((label): label is GmailLabel => Boolean(label));
  if (visible.length === 0) return null;
  return (
    <div className={`flex min-w-0 items-center gap-1 overflow-hidden ${compact ? "max-w-[52%] shrink" : ""}`}>
      {visible.slice(0, max).map(label => (
        <span
          key={label.id}
          className={`group/label flex min-w-0 items-center rounded py-0.5 text-[10px] font-medium ${compact ? "max-w-16 px-1.5" : "max-w-36 pl-1.5 pr-1"}`}
          style={{
            backgroundColor: safeColor(label.background_color, "rgba(255,255,255,0.08)"),
            color: safeColor(label.text_color, "#a1a1aa"),
          }}
        >
          <span className="min-w-0 truncate" title={label.name}>{label.name}</span>
          {onRemove && (
            <button
              type="button"
              disabled={removingId === label.id}
              aria-label={`${tr.labels.remove}: ${label.name}`}
              onClick={async event => {
                event.stopPropagation();
                setRemovingId(label.id);
                try { await onRemove(label.id); } catch { /* Parent action already reports the failure. */ } finally { setRemovingId(null); }
              }}
              className="ml-1 flex h-3.5 w-3.5 shrink-0 items-center justify-center rounded opacity-0 transition-opacity hover:bg-black/15 group-hover/label:opacity-100 group-focus-within/label:opacity-100 disabled:opacity-40"
            >
              <X className="h-2.5 w-2.5" />
            </button>
          )}
        </span>
      ))}
      {visible.length > max && (
        <span
          className="shrink-0 text-[10px] text-zinc-600"
          title={visible.slice(max).map(label => label.name).join(", ")}
        >
          +{visible.length - max}
        </span>
      )}
    </div>
  );
}

export function LabelPicker({
  labels,
  labelIds,
  disabled,
  onToggle,
  onCreate,
}: {
  labels: GmailLabel[];
  labelIds: string[];
  disabled?: boolean;
  onToggle: (labelId: string, applied: boolean) => Promise<void>;
  onCreate: (name: string) => Promise<GmailLabel | null>;
}) {
  const tr = useLocale();
  const rootRef = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [creating, setCreating] = useState(false);
  const [choosingCreateLocation, setChoosingCreateLocation] = useState(false);
  const [pendingId, setPendingId] = useState<string | null>(null);
  const filtered = useMemo(() => {
    const normalized = query.trim().toLocaleLowerCase();
    return normalized ? labels.filter(label => label.name.toLocaleLowerCase().includes(normalized)) : labels;
  }, [labels, query]);

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

  useEffect(() => {
    if (!open) setChoosingCreateLocation(false);
  }, [open]);

  const createLabel = async (parentName: string | null = null) => {
    const leafName = query.trim();
    const name = nestedLabelName(parentName, leafName);
    if (!name || creating) return;
    setCreating(true);
    try {
      const label = await onCreate(name);
      if (label) {
        await onToggle(label.id, true);
        setQuery("");
        setChoosingCreateLocation(false);
      }
    } finally {
      setCreating(false);
    }
  };

  return (
    <div ref={rootRef} className="relative inline-flex items-center">
      <ToolbarTip label={tr.labels.manage}>
        <button
          type="button"
          disabled={disabled}
          onClick={() => setOpen(value => !value)}
          aria-expanded={open}
          className="rounded-md p-2 text-zinc-400 transition-colors hover:bg-white/5 hover:text-[var(--app-accent)] disabled:cursor-not-allowed disabled:opacity-40"
        >
          <Tag className="h-4 w-4" />
        </button>
      </ToolbarTip>
      {open && (
        <div className="absolute right-0 top-full z-[120] mt-1 w-64 overflow-hidden rounded-lg border border-white/10 bg-[var(--color-surface-popover)] shadow-2xl">
          <div className="flex items-center gap-2 border-b border-white/5 px-3 py-2">
            <Search className="h-3.5 w-3.5 text-zinc-600" />
            <input
              autoFocus
              value={query}
              onChange={event => {
                setQuery(event.target.value);
                setChoosingCreateLocation(false);
              }}
              onKeyDown={event => { if (event.key === "Enter") void createLabel(null); }}
              placeholder={tr.labels.search}
              aria-label={tr.labels.search}
              className="min-w-0 flex-1 bg-transparent text-xs text-zinc-200 outline-none placeholder:text-zinc-600"
            />
          </div>
          <div className="max-h-64 overflow-y-auto p-1.5">
            {choosingCreateLocation ? (
              <>
                <div className="mb-1 flex items-center gap-1 border-b border-white/5 pb-1">
                  <button
                    type="button"
                    aria-label={tr.common.back}
                    onClick={() => setChoosingCreateLocation(false)}
                    className="flex h-7 w-7 items-center justify-center rounded-md text-zinc-500 hover:bg-white/5 hover:text-zinc-200"
                  >
                    <ChevronLeft className="h-3.5 w-3.5" />
                  </button>
                  <span className="min-w-0 flex-1 truncate text-xs font-medium text-zinc-300">{tr.labels.chooseLocation}</span>
                </div>
                <button
                  type="button"
                  disabled={creating}
                  onClick={() => void createLabel(null)}
                  className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs text-zinc-300 hover:bg-white/5 disabled:opacity-50"
                >
                  <span className="h-2.5 w-2.5 shrink-0 rounded-full bg-zinc-600" />
                  <span className="truncate">{tr.labels.topLevel}</span>
                </button>
                {labels.map(parent => {
                  const targetName = nestedLabelName(parent.name, query.trim());
                  const duplicate = labels.some(label => label.name.toLocaleLowerCase() === targetName.toLocaleLowerCase());
                  return (
                    <button
                      key={parent.id}
                      type="button"
                      disabled={creating || duplicate}
                      title={parent.name}
                      onClick={() => void createLabel(parent.name)}
                      className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs text-zinc-300 hover:bg-white/5 disabled:opacity-35"
                    >
                      <span
                        className="h-2.5 w-2.5 shrink-0 rounded-full"
                        style={{ backgroundColor: safeColor(parent.background_color, "#71717a") }}
                      />
                      <span className="min-w-0 flex-1 truncate">{parent.name}</span>
                    </button>
                  );
                })}
              </>
            ) : filtered.map(label => {
              const applied = labelIds.includes(label.id);
              return (
                <button
                  key={label.id}
                  type="button"
                  disabled={pendingId === label.id}
                  onClick={async () => {
                    setPendingId(label.id);
                    try { await onToggle(label.id, !applied); } finally { setPendingId(null); }
                  }}
                  className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-xs text-zinc-300 hover:bg-white/5 disabled:opacity-50"
                >
                  <span
                    className="h-2.5 w-2.5 shrink-0 rounded-full"
                    style={{ backgroundColor: safeColor(label.background_color, "#71717a") }}
                  />
                  <span className="min-w-0 flex-1 truncate">{label.name}</span>
                  {applied && <Check className="h-3.5 w-3.5 text-[var(--app-accent)]" />}
                </button>
              );
            })}
            {!choosingCreateLocation && filtered.length === 0 && !query.trim() && (
              <div className="px-2 py-3 text-center text-xs text-zinc-600">{tr.labels.none}</div>
            )}
          </div>
          {!choosingCreateLocation && query.trim() && !labels.some(label => label.name.toLocaleLowerCase() === query.trim().toLocaleLowerCase()) && (
            <button
              type="button"
              disabled={creating}
              onClick={() => setChoosingCreateLocation(true)}
              className="flex w-full items-center gap-2 border-t border-white/5 px-3 py-2 text-left text-xs text-[var(--app-accent)] hover:bg-white/5 disabled:opacity-50"
            >
              <Plus className="h-3.5 w-3.5" />
              <span className="truncate">{tr.labels.createNamed.replace("{name}", query.trim())}</span>
            </button>
          )}
        </div>
      )}
    </div>
  );
}

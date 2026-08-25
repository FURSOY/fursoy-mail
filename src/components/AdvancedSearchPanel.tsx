import { Check, ChevronDown } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { AdvancedSearchCriteria, AdvancedSearchDateWindow } from "../advancedSearch";
import {
  createEmptyAdvancedSearch, dateInputValue, dateWindowBounds, endOfLocalDate, startOfLocalDate,
} from "../advancedSearch";
import { useLocale } from "../i18n";
import { ui } from "../theme";
import type { CustomMailbox, GmailLabel } from "../types";

interface AdvancedSearchPanelProps {
  criteria: AdvancedSearchCriteria;
  gmailLabels: GmailLabel[];
  customMailboxes: CustomMailbox[];
  onApply: (criteria: AdvancedSearchCriteria) => void;
  onClose: () => void;
}

interface SearchSelectOption<T extends string> {
  value: T;
  label: string;
}

function SearchSelect<T extends string>({
  value, label, options, onChange,
}: {
  value: T;
  label: string;
  options: SearchSelectOption<T>[];
  onChange: (value: T) => void;
}) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const selected = options.find(option => option.value === value) ?? options[0];

  useEffect(() => {
    if (!open) return;
    const handleOutside = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
    };
    window.addEventListener("pointerdown", handleOutside);
    return () => window.removeEventListener("pointerdown", handleOutside);
  }, [open]);

  return (
    <div ref={rootRef} className="relative">
      <button
        type="button"
        aria-label={label}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => setOpen(previous => !previous)}
        className={`${ui.input} flex items-center justify-between gap-2 py-1.5 text-left text-xs`}
      >
        <span className="min-w-0 truncate">{selected.label}</span>
        <ChevronDown className={`h-3.5 w-3.5 shrink-0 text-[var(--color-text-disabled)] transition-transform ${open ? "rotate-180" : ""}`} />
      </button>
      {open && (
        <div
          role="listbox"
          aria-label={label}
          className="label-scrollbar absolute left-0 right-0 top-full z-[180] mt-1 max-h-52 overflow-y-auto rounded-[var(--radius-md)] border border-[var(--color-border-default)] bg-[var(--color-surface-popover)] p-1 shadow-2xl shadow-black/50"
        >
          {options.map(option => (
            <button
              key={option.value}
              type="button"
              role="option"
              aria-selected={option.value === value}
              onClick={() => {
                onChange(option.value);
                setOpen(false);
              }}
              className={`flex w-full items-center justify-between gap-2 rounded-[var(--radius-sm)] px-2.5 py-2 text-left text-xs transition-colors ${option.value === value ? "bg-[var(--app-accent-soft)] text-[var(--color-text-primary)]" : "text-[var(--color-text-secondary)] hover:bg-[var(--color-surface-hover)]"}`}
            >
              <span className="truncate">{option.label}</span>
              {option.value === value && <Check className="h-3.5 w-3.5 shrink-0 text-[var(--app-accent)]" />}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

export function AdvancedSearchPanel({ criteria, gmailLabels, customMailboxes, onApply, onClose }: AdvancedSearchPanelProps) {
  const tr = useLocale();
  const [draft, setDraft] = useState(criteria);

  useEffect(() => setDraft(criteria), [criteria]);

  const update = <K extends keyof AdvancedSearchCriteria>(key: K, value: AdvancedSearchCriteria[K]) => {
    setDraft(previous => ({ ...previous, [key]: value }));
  };

  const inputClass = `${ui.input} py-1.5 text-xs`;
  const labelClass = "space-y-1 text-[length:var(--font-size-caption)] text-[var(--color-text-subtle)]";
  const checkboxClass = "h-3.5 w-3.5 rounded border-white/15 accent-[var(--app-accent)]";

  return (
    <form
      role="dialog"
      aria-label={tr.mail.advancedSearch.title}
      className="absolute left-0 top-full z-[160] mt-1 w-[min(620px,calc(100vw-1.5rem))] rounded-[var(--radius-lg)] border border-[var(--color-border-default)] bg-[var(--color-surface-popover)] p-4 shadow-2xl shadow-black/50"
      onSubmit={(event) => {
        event.preventDefault();
        onApply(draft.dateMode === "within"
          ? { ...draft, ...dateWindowBounds(draft.dateAnchor, draft.dateWindow) }
          : draft);
      }}
    >
      <div className="grid grid-cols-1 gap-x-4 gap-y-3 sm:grid-cols-2">
        <label className={labelClass}>
          <span>{tr.mail.advancedSearch.from}</span>
          <input autoFocus className={inputClass} value={draft.from} onChange={event => update("from", event.target.value)} />
        </label>
        <label className={labelClass}>
          <span>{tr.mail.advancedSearch.to}</span>
          <input className={inputClass} value={draft.to} onChange={event => update("to", event.target.value)} />
        </label>
        <label className={`${labelClass} sm:col-span-2`}>
          <span>{tr.mail.advancedSearch.subject}</span>
          <input className={inputClass} value={draft.subject} onChange={event => update("subject", event.target.value)} />
        </label>
        <label className={labelClass}>
          <span>{tr.mail.advancedSearch.includes}</span>
          <input className={inputClass} value={draft.includes} onChange={event => update("includes", event.target.value)} />
        </label>
        <label className={labelClass}>
          <span>{tr.mail.advancedSearch.excludes}</span>
          <input className={inputClass} value={draft.excludes} onChange={event => update("excludes", event.target.value)} />
        </label>
        <div className="sm:col-span-2 space-y-2">
          <span className="text-[length:var(--font-size-caption)] text-[var(--color-text-subtle)]">{tr.mail.advancedSearch.date}</span>
          <div className="inline-flex rounded-[var(--radius-md)] border border-[var(--color-border-default)] bg-[var(--color-surface-control)] p-0.5">
            {(["range", "within"] as const).map(mode => (
              <button
                key={mode}
                type="button"
                aria-pressed={draft.dateMode === mode}
                onClick={() => update("dateMode", mode)}
                className={`rounded-[var(--radius-sm)] px-3 py-1 text-xs transition-colors ${draft.dateMode === mode ? "bg-[var(--app-accent-soft)] text-[var(--color-text-primary)]" : "text-[var(--color-text-muted)] hover:text-[var(--color-text-secondary)]"}`}
              >
                {mode === "range" ? tr.mail.advancedSearch.dateRange : tr.mail.advancedSearch.dateWithin}
              </button>
            ))}
          </div>
          {draft.dateMode === "range" ? (
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <label className={labelClass}>
                <span>{tr.mail.advancedSearch.afterDate}</span>
                <input type="date" className={inputClass} value={dateInputValue(draft.afterDate)} max={dateInputValue(draft.beforeDate === null ? null : draft.beforeDate - 1)} onChange={event => update("afterDate", startOfLocalDate(event.target.value))} />
              </label>
              <label className={labelClass}>
                <span>{tr.mail.advancedSearch.beforeDate}</span>
                <input type="date" className={inputClass} value={dateInputValue(draft.beforeDate === null ? null : draft.beforeDate - 1)} min={dateInputValue(draft.afterDate)} onChange={event => update("beforeDate", endOfLocalDate(event.target.value))} />
              </label>
            </div>
          ) : (
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <div className={labelClass}>
                <span>{tr.mail.advancedSearch.period}</span>
                <SearchSelect<AdvancedSearchDateWindow>
                  value={draft.dateWindow}
                  label={tr.mail.advancedSearch.period}
                  onChange={value => update("dateWindow", value)}
                  options={([
                    ["1d", tr.mail.advancedSearch.oneDay], ["3d", tr.mail.advancedSearch.threeDays],
                    ["1w", tr.mail.advancedSearch.oneWeek], ["2w", tr.mail.advancedSearch.twoWeeks],
                    ["1m", tr.mail.advancedSearch.oneMonth], ["2m", tr.mail.advancedSearch.twoMonths],
                    ["6m", tr.mail.advancedSearch.sixMonths], ["1y", tr.mail.advancedSearch.oneYear],
                  ] as const).map(([value, optionLabel]) => ({ value, label: optionLabel }))}
                />
              </div>
              <label className={labelClass}>
                <span>{tr.mail.advancedSearch.selectedDate}</span>
                <input type="date" className={inputClass} value={dateInputValue(draft.dateAnchor)} onChange={event => update("dateAnchor", startOfLocalDate(event.target.value))} />
              </label>
            </div>
          )}
        </div>
        <div className={`${labelClass} sm:col-span-2`}>
          <span>{tr.mail.advancedSearch.location}</span>
          <SearchSelect
            value={draft.location}
            label={tr.mail.advancedSearch.location}
            onChange={value => setDraft(previous => ({
              ...previous,
              location: value,
              locationExplicit: true,
            }))}
            options={[
              { value: "all", label: tr.mail.advancedSearch.allMail },
              { value: "inbox", label: tr.nav.inbox },
              { value: "sent", label: tr.nav.sent },
              { value: "archive", label: tr.nav.archive },
              { value: "spam", label: tr.nav.spam },
              { value: "trash", label: tr.nav.trash },
              ...customMailboxes.map(mailbox => ({ value: mailbox.role, label: mailbox.name })),
              ...gmailLabels.map(label => ({ value: `gmail:${label.id}`, label: label.name })),
            ]}
          />
        </div>
      </div>

      <div className="mt-4 flex flex-wrap items-center gap-x-5 gap-y-2 text-xs text-[var(--color-text-secondary)]">
        <label className="flex items-center gap-2"><input type="checkbox" className={checkboxClass} checked={draft.hasAttachment} onChange={event => update("hasAttachment", event.target.checked)} />{tr.mail.advancedSearch.hasAttachment}</label>
        <label className="flex items-center gap-2"><input type="checkbox" className={checkboxClass} checked={draft.unread} onChange={event => update("unread", event.target.checked)} />{tr.mail.advancedSearch.unread}</label>
        <label className="flex items-center gap-2"><input type="checkbox" className={checkboxClass} checked={draft.starred} onChange={event => update("starred", event.target.checked)} />{tr.mail.advancedSearch.starred}</label>
      </div>

      <div className="mt-5 flex items-center justify-end gap-2">
        <button type="button" className={ui.buttonSecondary} onClick={() => setDraft(createEmptyAdvancedSearch())}>{tr.mail.advancedSearch.reset}</button>
        <button type="button" className={ui.buttonSecondary} onClick={onClose}>{tr.common.cancel}</button>
        <button type="submit" className={ui.buttonPrimary}>{tr.mail.advancedSearch.search}</button>
      </div>
    </form>
  );
}

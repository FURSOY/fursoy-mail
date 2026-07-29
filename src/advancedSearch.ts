export type AdvancedSearchDateMode = "range" | "within";
export type AdvancedSearchDateWindow = "1d" | "3d" | "1w" | "2w" | "1m" | "2m" | "6m" | "1y";

export interface AdvancedSearchCriteria {
  from: string;
  to: string;
  subject: string;
  includes: string;
  excludes: string;
  afterDate: number | null;
  beforeDate: number | null;
  dateMode: AdvancedSearchDateMode;
  dateAnchor: number | null;
  dateWindow: AdvancedSearchDateWindow;
  location: string;
  locationExplicit: boolean;
  hasAttachment: boolean;
  unread: boolean;
  starred: boolean;
}

export function createEmptyAdvancedSearch(): AdvancedSearchCriteria {
  return {
    from: "",
    to: "",
    subject: "",
    includes: "",
    excludes: "",
    afterDate: null,
    beforeDate: null,
    dateMode: "range",
    dateAnchor: null,
    dateWindow: "1d",
    location: "all",
    locationExplicit: false,
    hasAttachment: false,
    unread: false,
    starred: false,
  };
}

export function isAdvancedSearchActive(criteria: AdvancedSearchCriteria): boolean {
  return Boolean(
    criteria.from.trim()
    || criteria.to.trim()
    || criteria.subject.trim()
    || criteria.includes.trim()
    || criteria.excludes.trim()
    || criteria.afterDate !== null
    || criteria.beforeDate !== null
    || criteria.location !== "all"
    || criteria.hasAttachment
    || criteria.unread
    || criteria.starred
  );
}

export function advancedSearchKey(criteria: AdvancedSearchCriteria): string {
  return JSON.stringify(criteria);
}

export function searchSidebarTab(
  currentTab: string,
  searchActive: boolean,
  criteria: AdvancedSearchCriteria,
): string {
  if (!searchActive) return currentTab;
  if (!criteria.locationExplicit) {
    if (criteria.starred) return "starred";
    return /^(inbox|starred|all|sent|archive|spam|trash|gmail:.+)$/.test(currentTab) ? currentTab : "";
  }
  if (/^(inbox|starred|all|sent|archive|spam|trash|gmail:.+)$/.test(criteria.location)) {
    return criteria.location;
  }
  return "";
}

export function dateInputValue(timestamp: number | null): string {
  if (timestamp === null) return "";
  const date = new Date(timestamp);
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

export function startOfLocalDate(value: string): number | null {
  if (!value) return null;
  const [year, month, day] = value.split("-").map(Number);
  if (!year || !month || !day) return null;
  return new Date(year, month - 1, day).getTime();
}

export function endOfLocalDate(value: string): number | null {
  const start = startOfLocalDate(value);
  if (start === null) return null;
  const date = new Date(start);
  date.setDate(date.getDate() + 1);
  return date.getTime();
}

export function dateWindowBounds(
  anchor: number | null,
  window: AdvancedSearchDateWindow,
): Pick<AdvancedSearchCriteria, "afterDate" | "beforeDate"> {
  if (anchor === null) return { afterDate: null, beforeDate: null };
  const amount = Number(window.slice(0, -1));
  const unit = window.charAt(window.length - 1);
  const start = new Date(anchor);
  const end = new Date(anchor);
  if (unit === "d") {
    start.setDate(start.getDate() - amount);
    end.setDate(end.getDate() + amount + 1);
  } else if (unit === "w") {
    start.setDate(start.getDate() - amount * 7);
    end.setDate(end.getDate() + amount * 7 + 1);
  } else if (unit === "m") {
    start.setMonth(start.getMonth() - amount);
    end.setMonth(end.getMonth() + amount);
    end.setDate(end.getDate() + 1);
  } else {
    start.setFullYear(start.getFullYear() - amount);
    end.setFullYear(end.getFullYear() + amount);
    end.setDate(end.getDate() + 1);
  }
  return { afterDate: start.getTime(), beforeDate: end.getTime() };
}

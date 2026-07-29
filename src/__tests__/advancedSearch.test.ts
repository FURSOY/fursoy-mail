import { describe, expect, it } from "vitest";
import {
  createEmptyAdvancedSearch, dateWindowBounds, endOfLocalDate, isAdvancedSearchActive,
  searchSidebarTab, startOfLocalDate,
} from "../advancedSearch";

describe("advanced search criteria", () => {
  it("distinguishes an empty form from active filters", () => {
    const empty = createEmptyAdvancedSearch();
    expect(isAdvancedSearchActive(empty)).toBe(false);
    expect(isAdvancedSearchActive({ ...empty, hasAttachment: true })).toBe(true);
    expect(isAdvancedSearchActive({ ...empty, from: "alice@example.test" })).toBe(true);
  });

  it("uses an exclusive next-day boundary for the selected end date", () => {
    const start = startOfLocalDate("2026-07-30");
    const end = endOfLocalDate("2026-07-30");
    expect(start).not.toBeNull();
    expect(end).not.toBeNull();
    expect(new Date(end!).getDate()).not.toBe(new Date(start!).getDate());
  });

  it("builds a symmetric Gmail-style window around a selected date", () => {
    const anchor = startOfLocalDate("2026-07-30");
    const bounds = dateWindowBounds(anchor, "1w");
    expect(new Date(bounds.afterDate!).getDate()).toBe(23);
    expect(new Date(bounds.beforeDate! - 1).getDate()).toBe(6);
  });

  it("keeps or redirects the sidebar highlight without changing navigation", () => {
    const criteria = createEmptyAdvancedSearch();
    expect(searchSidebarTab("inbox", true, criteria)).toBe("inbox");
    expect(searchSidebarTab("settings", true, criteria)).toBe("");
    expect(searchSidebarTab("inbox", true, { ...criteria, starred: true })).toBe("starred");
    expect(searchSidebarTab("inbox", true, {
      ...criteria,
      location: "archive",
      locationExplicit: true,
    })).toBe("archive");
    expect(searchSidebarTab("inbox", true, {
      ...criteria,
      location: "all",
      locationExplicit: true,
    })).toBe("all");
    expect(searchSidebarTab("inbox", false, {
      ...criteria,
      location: "archive",
      locationExplicit: true,
    })).toBe("inbox");
  });
});

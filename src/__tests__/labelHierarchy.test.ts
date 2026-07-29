import { describe, expect, it } from "vitest";
import {
  buildLabelHierarchy,
  canNestLabelUnder,
  labelAncestorIds,
  labelLeafName,
  labelParentName,
  nestedLabelName,
} from "../labelHierarchy";
import type { GmailLabel } from "../types";

const label = (id: string, name: string): GmailLabel => ({
  id,
  account_id: "account-1",
  name,
  background_color: null,
  text_color: null,
});

describe("label hierarchy", () => {
  it("renders parents before indented children and can collapse a branch", () => {
    const labels = [
      label("child", "Work/Project"),
      label("other", "Personal"),
      label("parent", "Work"),
      label("grandchild", "Work/Project/Urgent"),
    ];

    expect(buildLabelHierarchy(labels, new Set()).map(row => [row.displayName, row.depth])).toEqual([
      ["Personal", 0],
      ["Work", 0],
      ["Project", 1],
      ["Urgent", 2],
    ]);
    expect(buildLabelHierarchy(labels, new Set(["parent"])).map(row => row.label.id)).toEqual([
      "other",
      "parent",
    ]);
  });

  it("keeps orphaned slash names visible at the top level", () => {
    const rows = buildLabelHierarchy([label("orphan", "Missing/Child")], new Set());
    expect(rows).toMatchObject([{ displayName: "Missing/Child", depth: 0 }]);
  });

  it("builds names safely and excludes self or descendants as parents", () => {
    const parent = label("parent", "Work");
    const child = label("child", "Work/Project");
    const other = label("other", "Personal");

    expect(labelLeafName(child.name)).toBe("Project");
    expect(labelParentName(child.name)).toBe("Work");
    expect(nestedLabelName(other.name, labelLeafName(child.name))).toBe("Personal/Project");
    expect(canNestLabelUnder(parent, parent)).toBe(false);
    expect(canNestLabelUnder(parent, child)).toBe(false);
    expect(canNestLabelUnder(parent, other)).toBe(true);
  });

  it("finds every parent that must open to reveal a selected nested label", () => {
    const labels = [
      label("root", "Work"),
      label("parent", "Work/Project"),
      label("child", "Work/Project/Urgent"),
      label("other", "Personal"),
    ];
    expect(labelAncestorIds(labels, "child")).toEqual(["parent", "root"]);
    expect(labelAncestorIds(labels, "other")).toEqual([]);
  });
});

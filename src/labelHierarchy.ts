import type { GmailLabel } from "./types";

export interface LabelHierarchyRow {
  label: GmailLabel;
  displayName: string;
  depth: number;
  hasChildren: boolean;
}

export function labelLeafName(name: string): string {
  const separator = name.lastIndexOf("/");
  return separator >= 0 ? name.slice(separator + 1) : name;
}

export function labelParentName(name: string): string | null {
  const separator = name.lastIndexOf("/");
  return separator > 0 ? name.slice(0, separator) : null;
}

export function nestedLabelName(parentName: string | null, leafName: string): string {
  return parentName ? `${parentName}/${leafName}` : leafName;
}

export function canNestLabelUnder(label: GmailLabel, candidate: GmailLabel): boolean {
  return candidate.id !== label.id && !candidate.name.startsWith(`${label.name}/`);
}

export function labelAncestorIds(labels: GmailLabel[], labelId: string): string[] {
  const selected = labels.find(label => label.id === labelId);
  if (!selected) return [];
  const byName = new Map(labels.map(label => [label.name, label]));
  const ancestors: string[] = [];
  let parentName = labelParentName(selected.name);
  while (parentName) {
    const parent = byName.get(parentName);
    if (!parent) break;
    ancestors.push(parent.id);
    parentName = labelParentName(parent.name);
  }
  return ancestors;
}

export function buildLabelHierarchy(
  labels: GmailLabel[],
  collapsedLabelIds: ReadonlySet<string>,
): LabelHierarchyRow[] {
  const byName = new Map(labels.map(label => [label.name, label]));
  const children = new Map<string | null, GmailLabel[]>();

  for (const label of labels) {
    const parentName = labelParentName(label.name);
    const parentId = parentName ? byName.get(parentName)?.id ?? null : null;
    const siblings = children.get(parentId) ?? [];
    siblings.push(label);
    children.set(parentId, siblings);
  }

  const sortLabels = (items: GmailLabel[]) => items.sort((left, right) =>
    labelLeafName(left.name).localeCompare(labelLeafName(right.name), undefined, { sensitivity: "base" })
  );
  for (const siblings of children.values()) sortLabels(siblings);

  const rows: LabelHierarchyRow[] = [];
  const visit = (label: GmailLabel, depth: number) => {
    const nested = children.get(label.id) ?? [];
    rows.push({
      label,
      displayName: depth === 0 && labelParentName(label.name) ? label.name : labelLeafName(label.name),
      depth,
      hasChildren: nested.length > 0,
    });
    if (!collapsedLabelIds.has(label.id)) {
      for (const child of nested) visit(child, depth + 1);
    }
  };

  for (const root of children.get(null) ?? []) visit(root, 0);
  return rows;
}

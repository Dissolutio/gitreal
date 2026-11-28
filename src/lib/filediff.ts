import type { FileDiffLine } from "./api";

export type DiffMode = "full_file" | "changes_only";
export type DiffDisplayRow =
  | { kind: "line"; index: number; line: FileDiffLine }
  | { kind: "collapsed"; start: number; end: number; count: number; key: string };

export const DIFF_COLLAPSE_THRESHOLD = 12;
export const DIFF_CONTEXT_LINES = 3;

export function nextChangeIndex(lines: readonly FileDiffLine[], current: number, direction: 1 | -1): number {
  const changes = lines.flatMap((line, index) => line.kind === "context" ? [] : [index]);
  if (!changes.length) return -1;
  if (direction === 1) return changes.find(index => index > current) ?? changes[0];
  return [...changes].reverse().find(index => index < current) ?? changes[changes.length - 1];
}

export function buildDiffRows(
  lines: readonly FileDiffLine[],
  mode: DiffMode,
  expandedRegions: ReadonlySet<string> = new Set(),
  expandAll = false,
): DiffDisplayRow[] {
  const included = new Array<boolean>(lines.length).fill(mode === "full_file");
  if (mode === "changes_only") {
    lines.forEach((line, index) => {
      if (line.kind === "context") return;
      const start = Math.max(0, index - DIFF_CONTEXT_LINES);
      const end = Math.min(lines.length, index + DIFF_CONTEXT_LINES + 1);
      for (let context = start; context < end; context++) included[context] = true;
    });
  }

  const rows: DiffDisplayRow[] = [];
  let index = 0;
  while (index < lines.length) {
    if (!included[index]) {
      index++;
      continue;
    }
    if (lines[index].kind !== "context") {
      rows.push({ kind: "line", index, line: lines[index] });
      index++;
      continue;
    }

    const start = index;
    while (index < lines.length && included[index] && lines[index].kind === "context") index++;
    const end = index;
    const count = end - start;
    const key = `${start}:${end}`;
    if (count <= DIFF_COLLAPSE_THRESHOLD || expandAll || expandedRegions.has(key)) {
      for (let lineIndex = start; lineIndex < end; lineIndex++) {
        rows.push({ kind: "line", index: lineIndex, line: lines[lineIndex] });
      }
      continue;
    }

    const beforeChange = start > 0 && lines[start - 1].kind !== "context";
    const afterChange = end < lines.length && lines[end].kind !== "context";
    const keepStart = beforeChange ? DIFF_CONTEXT_LINES : 0;
    const keepEnd = afterChange ? DIFF_CONTEXT_LINES : 0;
    const hiddenStart = start + keepStart;
    const hiddenEnd = end - keepEnd;
    if (hiddenEnd - hiddenStart <= 0) {
      for (let lineIndex = start; lineIndex < end; lineIndex++) {
        rows.push({ kind: "line", index: lineIndex, line: lines[lineIndex] });
      }
      continue;
    }
    for (let lineIndex = start; lineIndex < hiddenStart; lineIndex++) {
      rows.push({ kind: "line", index: lineIndex, line: lines[lineIndex] });
    }
    rows.push({ kind: "collapsed", start: hiddenStart, end: hiddenEnd, count: hiddenEnd - hiddenStart, key });
    for (let lineIndex = hiddenEnd; lineIndex < end; lineIndex++) {
      rows.push({ kind: "line", index: lineIndex, line: lines[lineIndex] });
    }
  }
  return rows;
}

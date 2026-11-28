import { describe, expect, it } from "vitest";
import type { FileDiffLine } from "./api";
import { buildDiffRows, nextChangeIndex } from "./filediff";

function lines(count: number, changeAt: number[] = []): FileDiffLine[] {
  return Array.from({ length: count }, (_, index) => ({
    kind: changeAt.includes(index) ? "added" : "context",
    old_no: index + 1,
    new_no: index + 1,
    text: `line ${index + 1}`,
  }));
}

describe("file diff presentation helpers", () => {
  it("collapses unchanged runs over the threshold and expands selected regions", () => {
    const unchanged = lines(12);
    expect(buildDiffRows(unchanged, "full_file").some(row => row.kind === "collapsed")).toBe(false);

    const changed = [...lines(15), ...lines(1, [0]), ...lines(15)];
    const collapsed = buildDiffRows(changed, "full_file");
    const regions = collapsed.filter(row => row.kind === "collapsed");
    expect(regions).toHaveLength(2);
    expect(regions[0].kind === "collapsed" && regions[0].count).toBe(12);
    expect(buildDiffRows(changed, "full_file", new Set([regions[0].key])).some(row =>
      row.kind === "collapsed" && row.key === regions[0].key,
    )).toBe(false);
    expect(buildDiffRows(changed, "full_file", new Set(), true).some(row => row.kind === "collapsed")).toBe(false);
  });

  it("keeps three context lines and filters changes-only rows", () => {
    const source = lines(20, [10]);
    const rows = buildDiffRows(source, "changes_only");
    expect(rows.filter(row => row.kind === "line").map(row => row.index)).toEqual([7, 8, 9, 10, 11, 12, 13]);
  });

  it("jumps between changes in either direction and wraps", () => {
    const source = lines(8, [2, 6]);
    expect(nextChangeIndex(source, 2, 1)).toBe(6);
    expect(nextChangeIndex(source, 6, 1)).toBe(2);
    expect(nextChangeIndex(source, 6, -1)).toBe(2);
    expect(nextChangeIndex(source, 2, -1)).toBe(6);
    expect(nextChangeIndex(lines(2), 0, 1)).toBe(-1);
  });
});

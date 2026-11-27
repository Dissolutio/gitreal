import { describe, expect, it } from "vitest";
import type { GraphRow, StashInfo } from "./api";
import { indexStashes, placeVisibleStashes } from "./stashes";

const stash = (oid: string, base_oid: string): StashInfo => ({
  index: 0,
  oid,
  message: `stash ${oid}`,
  base_oid,
  timestamp: 1,
});

const row = (commit_id: string): GraphRow => ({
  commit_id,
  lane: 0,
  parent_edges: [],
  ref_labels: [],
  summary: commit_id,
  author: "author",
  date: 1,
  parents: [],
});

describe("visible stash graph placements", () => {
  it("places stashes beside visible base rows and ignores off-screen bases", () => {
    const visible = stash("visible", "commit-a");
    const hidden = stash("hidden", "commit-b");
    const placements = placeVisibleStashes(
      indexStashes([visible, hidden]),
      [row("commit-a"), row("commit-c")],
      10,
      10 * 36,
      36,
      200,
    );
    expect(placements).toMatchObject([
      { stash: visible, rowIndex: 10, stackIndex: 0, stackCount: 1, x: 200, y: 18 },
    ]);
  });

  it("stacks multiple stashes on one base without overlapping markers", () => {
    const stashes = [stash("one", "commit-a"), stash("two", "commit-a"), stash("three", "commit-a")];
    const placements = placeVisibleStashes(indexStashes(stashes), [row("commit-a")], 0, 0, 36, 180);
    expect(placements.map(placement => placement.stackIndex)).toEqual([0, 1, 2]);
    expect(placements.map(placement => placement.x)).toEqual([180, 194, 208]);
    expect(placements.every(placement => placement.stackCount === 3)).toBe(true);
  });

  it("does not place markers when their base row is outside the viewport", () => {
    expect(placeVisibleStashes(
      indexStashes([stash("one", "commit-a")]),
      [row("commit-a")],
      0,
      100,
      36,
    )).toEqual([]);
  });
});

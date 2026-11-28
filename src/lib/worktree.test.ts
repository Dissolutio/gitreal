import { describe, expect, it } from "vitest";
import type { FileChange, WorktreeStatus } from "./api";
import { canCommit, groupWorktreeFiles, hasWorktreeChanges, selectFile, selectedPaths, sortedFiles, summaryTone, worktreeFileCount } from "./worktree";
import { createTab } from "./tabs";

const file = (path: string): FileChange => ({ path, old_path: null, kind: "modified" });
const status: WorktreeStatus = {
  staged: [file("z.txt"), file("shared.txt")],
  unstaged: [file("a.txt"), file("shared.txt")],
  untracked: [],
  conflicted: [],
  truncated: false,
  generation: 0,
};

describe("worktree helpers", () => {
  it("shows WIP only when changes exist and counts paths once", () => {
    expect(hasWorktreeChanges(status)).toBe(true);
    expect(worktreeFileCount(status)).toBe(3);
    expect(hasWorktreeChanges({ ...status, staged: [], unstaged: [], untracked: [], conflicted: [] })).toBe(false);
  });

  it("sorts file lists and supports additive and range selection", () => {
    expect(sortedFiles([file("z"), file("a")]).map(item => item.path)).toEqual(["a", "z"]);
    expect(groupWorktreeFiles(status).unstaged.map(item => item.path)).toEqual(["a.txt", "shared.txt"]);
    expect(groupWorktreeFiles(status).staged.map(item => item.path)).toEqual(["shared.txt", "z.txt"]);
    const files = [file("a"), file("b"), file("c")];
    expect(selectFile(files, ["a"], "c", true, false)).toEqual(["a", "b", "c"]);
    expect(selectFile(files, ["a"], "c", false, true)).toEqual(["a", "c"]);
    expect(selectedPaths(files, ["a", "c"])).toEqual(["a", "c"]);
  });

  it("enforces summary thresholds and commit prerequisites", () => {
    expect(summaryTone(50)).toBe("normal");
    expect(summaryTone(51)).toBe("warning");
    expect(summaryTone(72)).toBe("warning");
    expect(summaryTone(73)).toBe("error");
    expect(canCommit(status, "summary")).toBe(true);
    expect(canCommit(status, "  ")).toBe(false);
    expect(canCommit({ ...status, staged: [] }, "summary")).toBe(false);
  });

  it("keeps commit drafts isolated per repository tab", () => {
    const first = createTab("/one");
    const second = createTab("/two");
    first.draftSummary = "first draft";
    expect(second.draftSummary).toBe("");
  });
});

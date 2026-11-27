import { describe, expect, it } from "vitest";
import { formatAheadBehind, formatOperationBanner } from "./status";

describe("branch status formatting", () => {
  it("omits zero ahead and behind counts", () => {
    expect(formatAheadBehind(2, 0)).toBe("↑2");
    expect(formatAheadBehind(0, 3)).toBe("↓3");
    expect(formatAheadBehind(0, 0)).toBe("");
    expect(formatAheadBehind(null, null)).toBe("");
    expect(formatAheadBehind(10_000, 0, true)).toBe("↑10000…");
  });

  it("formats a rebase with progress and conflicts", () => {
    expect(formatOperationBanner({
      kind: "rebase",
      head_name: "feature",
      onto: "main",
      step: 3,
      total_steps: 7,
    }, 2)).toBe("Rebasing feature onto main — step 3 of 7 — 2 conflicted files");
  });

  it("formats operation and singular conflict without optional details", () => {
    expect(formatOperationBanner({
      kind: "cherry-pick",
      head_name: null,
      onto: null,
      step: null,
      total_steps: null,
    }, 1)).toBe("Cherry-picking — 1 conflicted file");
  });
});

import { describe, expect, it } from "vitest";
import { validateBranchName } from "./refnames";

describe("validateBranchName", () => {
  it("rejects names Git rejects", () => {
    for (const name of ["-x", "a..b", "foo.lock", "@{", "topic/", "has space"]) {
      expect(validateBranchName(name), name).not.toBeNull();
    }
  });

  it("accepts valid slash-separated branch names", () => {
    expect(validateBranchName("feature/topic-1")).toBeNull();
  });
});

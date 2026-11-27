import { describe, expect, it } from "vitest";
import { safeRefLabel } from "./display";

describe("safeRefLabel", () => {
  it("reveals every bidi and zero-width code point in the protected ranges", () => {
    const ranges = [
      [0x202a, 0x202e],
      [0x2066, 0x2069],
      [0x200b, 0x200f],
      [0xfeff, 0xfeff],
    ];

    for (const [start, end] of ranges) {
      for (let codePoint = start; codePoint <= end; codePoint++) {
        const character = String.fromCodePoint(codePoint);
        const marker = `⟨U+${codePoint.toString(16).toUpperCase().padStart(4, "0")}⟩`;
        expect(safeRefLabel(`branch${character}name`)).toBe(`branch${marker}name`);
      }
    }
  });

  it("leaves plain ASCII and emoji unchanged", () => {
    expect(safeRefLabel("feature/my-branch")).toBe("feature/my-branch");
    expect(safeRefLabel("feature/🦀")).toBe("feature/🦀");
  });
});

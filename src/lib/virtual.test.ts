import { describe, expect, it } from "vitest";
import { PageCache, pageOffsets, ROW_HEIGHT, visibleRange } from "./virtual";
import { nativeError } from "./api";

describe("fixed-row virtualization", () => {
  it("renders a bounded viewport for 50,000+ commits", () => {
    const range = visibleRange(40_000 * ROW_HEIGHT + 5, 720, 80_000);
    expect(range).toEqual({ start: 39_992, end: 40_029 });
    expect(range.end - range.start).toBeLessThan(40);
  });
  it("clamps empty, first, last and stale scroll positions", () => {
    expect(visibleRange(0, 720, 0)).toEqual({ start: 0, end: 0 });
    expect(visibleRange(-100, 36, 10)).toEqual({ start: 0, end: 9 });
    expect(visibleRange(999_999, 720, 3)).toEqual({ start: 3, end: 3 });
    expect(visibleRange(9 * ROW_HEIGHT, 720, 10)).toEqual({ start: 1, end: 10 });
  });
  it("requests only intersecting pages including overscan boundaries", () => {
    expect(pageOffsets(0, 0)).toEqual([]);
    expect(pageOffsets(0, 200)).toEqual([0]);
    expect(pageOffsets(199, 401)).toEqual([0, 200, 400]);
    expect(pageOffsets(50_001, 50_021)).toEqual([50_000]);
  });
});

describe("bounded page cache", () => {
  it("evicts least recently used pages and preserves viewport reads", () => {
    const cache = new PageCache<string>(2, 2);
    cache.set(0, ["a", "b"]);
    cache.set(2, ["c", "d"]);
    expect(cache.row(1)).toBe("b");
    cache.set(4, ["e"]);
    expect(cache.size).toBe(2);
    expect(cache.has(2)).toBe(false);
    expect(cache.row(4)).toBe("e");
    expect(cache.row(5)).toBeUndefined();
    cache.clear();
    expect(cache.size).toBe(0);
  });
  it("bounds storage after traversing a large history", () => {
    const cache = new PageCache<number>();
    for (let i = 0; i < 80_000; i += 200) cache.set(i, [i]);
    expect(cache.size).toBe(12);
    expect(cache.has(0)).toBe(false);
  });
  it("replaces pages without evicting an unrelated page", () => {
    const cache = new PageCache<number>(2);
    cache.set(0, [1]); cache.set(200, [2]); cache.set(0, [3]);
    expect(cache.size).toBe(2);
    expect(cache.row(0)).toBe(3);
    expect(() => new PageCache(0)).toThrow();
  });
});

it("preserves serialized native errors and normalizes unknown failures", () => {
  expect(nativeError({ code: "repository_not_found", message: "Missing repository" }))
    .toEqual({ code: "repository_not_found", message: "Missing repository" });
  expect(nativeError("Connection failed")).toEqual({ code: "unexpected", message: "Connection failed" });
});

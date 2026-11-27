import { describe, expect, it } from "vitest";
import { closeTab, createTab, cycleTabs, openTab, restoreTabs, type TabsState } from "./tabs";

function state(...paths: string[]): TabsState {
  return {
    tabs: paths.map(path => createTab(path)),
    activePath: paths.at(-1) ?? null,
  };
}

describe("repository tab state", () => {
  it("opens a repository and focuses an existing path instead of duplicating it", () => {
    const first = openTab(state("/one"), createTab("/two"));
    expect(first.added).toBe(true);
    expect(first.state.activePath).toBe("/two");
    const duplicate = openTab(first.state, createTab("/one"));
    expect(duplicate.added).toBe(false);
    expect(duplicate.state.tabs).toHaveLength(2);
    expect(duplicate.state.activePath).toBe("/one");
  });

  it("closes the active tab and selects its neighbor", () => {
    const closed = closeTab(state("/one", "/two", "/three"), "/two");
    expect(closed.tabs.map(tab => tab.path)).toEqual(["/one", "/three"]);
    expect(closed.activePath).toBe("/three");
    expect(closeTab(state("/one"), "/one")).toEqual({ tabs: [], activePath: null });
  });

  it("cycles through tabs in both directions and wraps", () => {
    const tabs = state("/one", "/two", "/three");
    expect(cycleTabs(tabs, 1).activePath).toBe("/one");
    expect(cycleTabs(tabs, -1).activePath).toBe("/two");
  });

  it("restores existing and missing paths without aborting startup", () => {
    const restored = restoreTabs([
      { path: "/existing/repo", name: "repo" },
      { path: "/missing/repo" },
      { path: "" },
      null,
    ], "/missing/repo");
    expect(restored.tabs.map(tab => tab.path)).toEqual(["/existing/repo", "/missing/repo"]);
    expect(restored.tabs[1].error).toBeNull();
    expect(restored.activePath).toBe("/missing/repo");
  });
});

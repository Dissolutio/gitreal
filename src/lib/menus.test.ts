import { describe, expect, it } from "vitest";
import { branchMenuItems, commitMenuItems, mergeCommitRefLabels } from "./menus";
import type { RefInfo } from "./api";

const ref = (
  name: string,
  kind: RefInfo["kind"],
  target: string,
  is_head = false,
): RefInfo => ({
  name,
  kind,
  target,
  is_head,
  full_name: kind === "local"
    ? `refs/heads/${name}`
    : kind === "remote"
      ? `refs/remotes/${name}`
      : `refs/tags/${name}`,
});

describe("branchMenuItems", () => {
  it("shows actions appropriate to a checked-out local branch", () => {
    const items = branchMenuItems({
      ref: ref("main", "local", "abc", true),
      running: false,
      hasRemoteCounterpart: true,
    });
    expect(items.map(item => item.id)).toEqual([
      "checkout", "create-branch", "rename", "pull", "delete-local", "delete-both",
      "reset-remote", "copy",
    ]);
    expect(items[0].disabled).toBe(true);
    expect(items.find(item => item.id === "delete-local")?.disabled).toBe(true);
  });

  it("offers remote branch actions and disables mutations while a job runs", () => {
    const items = branchMenuItems({
      ref: ref("origin/topic", "remote", "abc"),
      running: true,
      hasLocalCounterpart: true,
    });
    expect(items.map(item => item.id)).toEqual([
      "checkout", "create-branch", "delete-remote", "delete-both", "reset-remote", "copy",
    ]);
    expect(items.filter(item => item.id !== "copy").every(item => item.disabled)).toBe(true);
  });

  it("does not offer rename or delete actions for tags", () => {
    const items = branchMenuItems({ ref: ref("v1", "tag", "abc"), running: false });
    expect(items.map(item => item.id)).toEqual(["checkout", "create-branch", "copy"]);
  });

  it("exposes commit actions with mutations disabled while running", () => {
    const items = commitMenuItems(true);
    expect(items.map(item => item.id)).toEqual(["checkout-commit", "create-branch", "copy-commit"]);
    expect(items.filter(item => item.id !== "copy-commit").every(item => item.disabled)).toBe(true);
  });
});

describe("mergeCommitRefLabels", () => {
  it("combines a local branch and its remote counterpart at the same commit", () => {
    const labels = mergeCommitRefLabels(
      ["topic", "origin/topic"],
      [ref("topic", "local", "abc"), ref("origin/topic", "remote", "abc")],
    );
    expect(labels).toHaveLength(1);
    expect(labels[0].kinds).toEqual(["local", "remote"]);
    expect(labels[0].fullNames).toEqual(["refs/heads/topic", "refs/remotes/origin/topic"]);
  });

  it("keeps local and remote labels separate when their commits differ", () => {
    const labels = mergeCommitRefLabels(
      ["topic", "origin/topic"],
      [ref("topic", "local", "abc"), ref("origin/topic", "remote", "def")],
    );
    expect(labels).toHaveLength(2);
  });
});

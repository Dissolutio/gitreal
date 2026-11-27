import type { RefInfo } from "./api";

export interface MenuItem {
  id: string;
  label: string;
  disabled?: boolean;
  danger?: boolean;
}

export interface BranchMenuState {
  ref: RefInfo;
  running: boolean;
  hasLocalCounterpart?: boolean;
  hasRemoteCounterpart?: boolean;
}

export interface CommitRefLabel {
  name: string;
  fullNames: string[];
  kinds: Array<"local" | "remote" | "tag" | "head">;
  refs: RefInfo[];
}

const item = (id: string, label: string, running: boolean, danger = false): MenuItem => ({
  id, label, disabled: running, danger,
});

export function branchMenuItems(state: BranchMenuState): MenuItem[] {
  const { ref, running } = state;
  const items = [item("checkout", "Checkout", running || ref.is_head)];
  if (ref.target) items.push(item("create-branch", "Create branch here…", running));
  if (ref.kind === "local") {
    items.push(item("rename", "Rename…", running));
    items.push(item(ref.is_head ? "pull" : "push", ref.is_head ? "Pull" : "Push", running));
    items.push(item("delete-local", "Delete local branch…", running, true));
    if (state.hasRemoteCounterpart) {
      items.push(item("delete-both", "Delete local and remote…", running, true));
    }
    if (state.hasRemoteCounterpart) {
      items.push(item("reset-remote", "Reset local to remote…", running, true));
    }
  } else if (ref.kind === "remote") {
    items.push(item("delete-remote", "Delete remote branch…", running, true));
    if (state.hasLocalCounterpart) {
      items.push(item("reset-remote", "Reset local to remote…", running, true));
    }
  }
  items.push(item("copy", "Copy branch name", false));
  return items;
}

export function commitMenuItems(running: boolean): MenuItem[] {
  return [
    item("checkout-commit", "Checkout this commit…", running),
    item("create-branch", "Create branch here…", running),
    item("copy-commit", "Copy commit SHA", false),
  ];
}

export function mergeCommitRefLabels(
  labels: string[],
  refs: RefInfo[],
): CommitRefLabel[] {
  const resolved = labels.map(name => ({
    name,
    ref: refs.find(reference => reference.name === name),
  }));
  const consumed = new Set<string>();
  const merged: CommitRefLabel[] = [];

  for (const label of resolved) {
    if (consumed.has(label.name)) continue;
    const ref = label.ref;
    const match = ref?.kind === "local"
      ? resolved.find(candidate =>
        candidate.ref?.kind === "remote" &&
        candidate.ref.target === ref.target &&
        candidate.name.endsWith(`/${ref.name}`),
      )
      : undefined;
    if (match) consumed.add(match.name);
    consumed.add(label.name);
    const refsForLabel = [ref, match?.ref].filter((value): value is RefInfo => !!value);
    merged.push({
      name: ref?.kind === "local"
        ? ref.name
        : ref?.kind === "remote"
          ? label.name.replace(/^[^/]+\//, "")
          : label.name.replace(/^refs\/(heads|remotes|tags)\//, ""),
      fullNames: refsForLabel.map(value => value.full_name),
      kinds: refsForLabel.map(value => value.kind),
      refs: refsForLabel,
    });
  }
  return merged;
}

export function refIcon(kind: RefInfo["kind"]): string {
  if (kind === "remote") return "☁";
  if (kind === "tag") return "◆";
  return kind === "head" ? "●" : "⑂";
}

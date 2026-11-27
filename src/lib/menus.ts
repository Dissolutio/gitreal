import type { RefInfo } from "./api";
import { safeRefLabel } from "./display";

export interface MenuItem {
  id: string;
  label: string;
  disabled?: boolean;
  danger?: boolean;
  title?: string;
}

export interface BranchMenuState {
  ref: RefInfo;
  running: boolean;
  currentBranch?: string | null;
  operationInProgress?: boolean;
  hasLocalCounterpart?: boolean;
  hasRemoteCounterpart?: boolean;
}

export interface CommitMenuState {
  running: boolean;
  currentBranch?: string | null;
  operationInProgress?: boolean;
}

export interface CommitRefLabel {
  name: string;
  fullNames: string[];
  kinds: Array<"local" | "remote" | "tag" | "head">;
  refs: RefInfo[];
}

const item = (
  id: string,
  label: string,
  disabled: boolean,
  danger = false,
  title?: string,
): MenuItem => ({
  id, label, disabled, danger, title,
});

export function branchMenuItems(state: BranchMenuState): MenuItem[] {
  const { ref, running } = state;
  const currentBranch = state.currentBranch ?? null;
  const operationInProgress = state.operationInProgress ?? false;
  const mutationsDisabled = running || operationInProgress;
  const historyDisabled = mutationsDisabled || !currentBranch;
  const checkOutFirst = !currentBranch ? "Check out a branch first" : undefined;
  const items: MenuItem[] = [];
  if ((ref.kind === "local" || ref.kind === "remote") && !ref.is_head) {
    items.push(
      item("merge-into-current", `Merge ${safeRefLabel(ref.name)} into ${currentBranch ? safeRefLabel(currentBranch) : "current branch"}`, historyDisabled, false, checkOutFirst),
      item("rebase-current-onto", `Rebase ${currentBranch ? safeRefLabel(currentBranch) : "current branch"} onto ${safeRefLabel(ref.name)}`, historyDisabled, false, checkOutFirst),
    );
  }
  items.push(item("checkout", "Checkout", mutationsDisabled || ref.is_head));
  if (ref.target) items.push(item("create-branch", "Create branch here…", mutationsDisabled));
  if (ref.kind === "local") {
    items.push(item("rename", "Rename…", mutationsDisabled));
    items.push(item(ref.is_head ? "pull" : "push", ref.is_head ? "Pull" : "Push", mutationsDisabled));
    items.push(item("delete-local", "Delete local branch…", mutationsDisabled || ref.is_head, true));
    if (state.hasRemoteCounterpart) {
      items.push(item("delete-both", "Delete local and remote…", mutationsDisabled || ref.is_head, true));
    }
    if (state.hasRemoteCounterpart) {
      items.push(item("reset-remote", "Reset local to remote…", mutationsDisabled, true));
    }
  } else if (ref.kind === "remote") {
    items.push(item("delete-remote", "Delete remote branch…", mutationsDisabled, true));
    if (state.hasLocalCounterpart) {
      items.push(item("delete-both", "Delete local and remote…", mutationsDisabled, true));
      items.push(item("reset-remote", "Reset local to remote…", mutationsDisabled, true));
    }
  }
  items.push(item("copy", "Copy branch name", false));
  return items;
}

export function commitMenuItems(state: CommitMenuState | boolean): MenuItem[] {
  const { running, operationInProgress = false, currentBranch = null } = typeof state === "boolean"
    ? { running: state, operationInProgress: false, currentBranch: null }
    : state;
  const disabled = running || operationInProgress;
  const branch = currentBranch ? safeRefLabel(currentBranch) : "HEAD";
  return [
    item("checkout-commit", "Checkout this commit…", disabled),
    item("create-branch", "Create branch here…", disabled),
    item("cherry-pick", "Cherry-pick commit", disabled),
    item("revert-commit", "Revert commit", disabled),
    item("reset-soft", `Reset ${branch} to this commit · Soft`, disabled),
    item("reset-mixed", `Reset ${branch} to this commit · Mixed`, disabled),
    item("reset-hard", `Reset ${branch} to this commit · Hard…`, disabled, true),
    item("copy-commit", "Copy commit SHA", false),
  ];
}

export function stashMenuItems(running: boolean, operationInProgress: boolean): MenuItem[] {
  const disabled = running || operationInProgress;
  return [
    item("stash-apply", "Apply stash", disabled),
    item("stash-pop-at", "Pop stash", disabled),
    item("stash-drop", "Drop stash…", disabled, true),
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
      name: safeRefLabel(ref?.kind === "local"
        ? ref.name
        : ref?.kind === "remote"
          ? label.name.replace(/^[^/]+\//, "")
          : label.name.replace(/^refs\/(heads|remotes|tags)\//, "")),
      fullNames: refsForLabel.map(value => safeRefLabel(value.full_name)),
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

import type { OperationInfo } from "./api";

export function formatAheadBehind(
  ahead: number | null,
  behind: number | null,
  truncated = false,
): string {
  const counts = [
    ahead ? `↑${ahead}` : "",
    behind ? `↓${behind}` : "",
  ].filter(Boolean);
  if (!counts.length) return "";
  return `${counts.join(" ")}${truncated ? "…" : ""}`;
}

export function formatOperationBanner(
  operation: OperationInfo,
  conflictedFiles: number,
): string {
  const labels = {
    merge: "Merge in progress",
    rebase: "Rebasing",
    "cherry-pick": "Cherry-picking",
    revert: "Reverting",
    bisect: "Bisecting",
  } as const;
  const name = operation.head_name ? ` ${operation.head_name}` : "";
  const onto = operation.onto ? ` onto ${operation.onto}` : "";
  const title = `${labels[operation.kind]}${name}${operation.kind === "rebase" ? onto : ""}`;
  const details = [];
  if (operation.step != null && operation.total_steps != null) {
    details.push(`step ${operation.step} of ${operation.total_steps}`);
  }
  if (conflictedFiles > 0) {
    details.push(`${conflictedFiles} conflicted file${conflictedFiles === 1 ? "" : "s"}`);
  }
  return [title, ...details].join(" — ");
}

export function operationBannerActions(
  operation: OperationInfo,
  conflictedFiles: number,
  running: boolean,
) {
  return {
    continueDisabled: running || conflictedFiles > 0,
    skipVisible: operation.kind === "rebase" || operation.kind === "cherry-pick",
    otherActionsDisabled: running,
  };
}

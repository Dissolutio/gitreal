import type { FileChange, WorktreeStatus } from "./api";

export function worktreeFileCount(status: WorktreeStatus | null): number {
  if (!status) return 0;
  return new Set([
    ...status.staged.map(file => file.path),
    ...status.unstaged.map(file => file.path),
    ...status.untracked.map(file => file.path),
    ...status.conflicted.map(file => file.path),
  ]).size;
}

export function hasWorktreeChanges(status: WorktreeStatus | null): boolean {
  return worktreeFileCount(status) > 0;
}

export function sortedFiles(files: FileChange[]): FileChange[] {
  return [...files].sort((a, b) => a.path.localeCompare(b.path));
}

export function summaryTone(length: number): "normal" | "warning" | "error" {
  return length > 72 ? "error" : length > 50 ? "warning" : "normal";
}

export function canCommit(status: WorktreeStatus | null, summary: string): boolean {
  return !!status?.staged.length && !!summary.trim();
}

export function selectFile(
  files: FileChange[],
  selected: string[],
  path: string,
  shift: boolean,
  additive: boolean,
): string[] {
  if (shift && selected.length) {
    const anchor = files.findIndex(file => file.path === selected[selected.length - 1]);
    const target = files.findIndex(file => file.path === path);
    if (anchor >= 0 && target >= 0) {
      const range = files.slice(Math.min(anchor, target), Math.max(anchor, target) + 1).map(file => file.path);
      return additive ? [...new Set([...selected, ...range])] : range;
    }
  }
  if (additive) {
    return selected.includes(path) ? selected.filter(item => item !== path) : [...selected, path];
  }
  return [path];
}

export function selectedPaths(files: FileChange[], selected: string[]): string[] {
  const paths = new Set(selected);
  return files.filter(file => paths.has(file.path)).map(file => file.path);
}

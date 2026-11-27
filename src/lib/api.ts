import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { JobFinishedEvent, JobStartedEvent } from "./snackbar";

export interface Repository { repo_id: string; path: string; name: string }
export interface KnownRepo { path: string; name: string; last_opened: number }
export interface PersistedTab { path: string }
export interface AppConfig {
  version: number;
  active_tab: string | null;
  repos: KnownRepo[];
  tabs: PersistedTab[];
}
export interface RefInfo {
  name: string;
  full_name: string;
  kind: "local" | "remote" | "tag" | "head";
  target: string | null;
  is_head: boolean;
}
export interface RefsResponse { refs: RefInfo[]; generation: number }
export interface RepoChangedEvent { repo_id: string; generation: number }
export interface RepoErrorEvent { repo_id: string; message: string }
export interface GraphEdge { from_lane: number; to_lane: number; parent_id: string }
export interface GraphRow {
  commit_id: string;
  lane: number;
  parent_edges: GraphEdge[];
  ref_labels: string[];
  summary: string;
  author: string;
  date: number;
  parents: string[];
}
export interface GraphPage { rows: GraphRow[]; total: number; max_lane: number; generation: number }
export interface ChangedPath {
  path: string;
  status: "added" | "modified" | "deleted" | "renamed";
  old_path: string | null;
}
export interface CommitDetail {
  commit_id: string;
  message: string;
  summary: string;
  author: string;
  author_email: string;
  date: number;
  parents: string[];
  changed_paths: ChangedPath[];
}
export interface NativeError { code: string; message: string }

export function nativeError(error: unknown): NativeError {
  if (typeof error === "object" && error !== null && "code" in error && "message" in error) {
    return { code: String(error.code), message: String(error.message) };
  }
  return { code: "unexpected", message: typeof error === "string" ? error : "An unexpected error occurred." };
}

async function call<T>(command: string, args: Record<string, unknown>): Promise<T> {
  try { return await invoke<T>(command, args); }
  catch (error) { throw nativeError(error); }
}

export const desktop = isTauri();
export function onRepoChanged(handler: (event: RepoChangedEvent) => void): Promise<UnlistenFn> {
  return desktop ? listen<RepoChangedEvent>("repo-changed", event => handler(event.payload)) : Promise.resolve(() => {});
}
export function onRepoError(handler: (event: RepoErrorEvent) => void): Promise<UnlistenFn> {
  return desktop ? listen<RepoErrorEvent>("repo-error", event => handler(event.payload)) : Promise.resolve(() => {});
}
export function onJobStarted(handler: (event: JobStartedEvent) => void): Promise<UnlistenFn> {
  return desktop ? listen<JobStartedEvent>("job-started", event => handler(event.payload)) : Promise.resolve(() => {});
}
export function onJobFinished(handler: (event: JobFinishedEvent) => void): Promise<UnlistenFn> {
  return desktop ? listen<JobFinishedEvent>("job-finished", event => handler(event.payload)) : Promise.resolve(() => {});
}
export const api = {
  open: (path: string) => call<Repository>("open_repo", { path }),
  close: (repoId: string) => call<null>("close_repo", { repoId }),
  refs: (repoId: string) => call<RefsResponse>("get_refs", { repoId }),
  page: (repoId: string, offset: number, limit: number) =>
    call<GraphPage>("get_graph_page", { repoId, offset, limit }),
  detail: (repoId: string, commitId: string) =>
    call<CommitDetail>("get_commit_detail", { repoId, commitId }),
  fetch: (repoId: string, remote?: string) =>
    call<string>("fetch", { repoId, remote }),
  pull: (repoId: string) => call<string>("pull", { repoId }),
  push: (repoId: string, branch: string) =>
    call<string>("push", { repoId, branch }),
  forcePush: (repoId: string, branch: string) =>
    call<string>("force_push", { repoId, branch }),
  pushRemote: (repoId: string, branch: string) =>
    call<string>("get_push_remote", { repoId, branch }),
  config: () => call<AppConfig>("get_config", {}),
  setOpenTabs: (paths: string[], active: string | null) =>
    call<void>("set_open_tabs", { paths, active }),
  addKnownRepo: (path: string) => call<void>("add_known_repo", { path }),
  removeKnownRepo: (path: string) => call<void>("remove_known_repo", { path }),
};

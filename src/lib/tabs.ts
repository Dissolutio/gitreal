import type { CommitDetail, NativeError, RefInfo, Repository, StashInfo, WorktreeStatus } from "./api";

export const MAX_TABS = 16;
let nextTabId = 0;

export interface RepoTab {
  tabId: number;
  path: string;
  name: string;
  repository: Repository | null;
  refs: RefInfo[];
  stashes: StashInfo[];
  worktreeStatus: WorktreeStatus | null;
  draftSummary: string;
  draftDescription: string;
  amend: boolean;
  error: NativeError | null;
  scrollTop: number;
  scrollLeft: number;
  selected: string | null;
  detail: CommitDetail | null;
  total: number;
  initialLoaded: boolean;
  maxLane: number;
  /** Repository generation the tab's cached pages and refs belong to. */
  generation: number;
  /** Newest `repo-changed` generation the tab has applied. */
  eventGeneration: number;
  /** Newest generation seen while the tab was inactive; 0 when up to date. */
  staleGeneration: number;
}

export interface TabsState {
  tabs: RepoTab[];
  activePath: string | null;
}

export function createTab(path: string, name = repositoryName(path)): RepoTab {
  return {
    tabId: nextTabId++,
    path,
    name,
    repository: null,
    refs: [],
    stashes: [],
    worktreeStatus: null,
    draftSummary: "",
    draftDescription: "",
    amend: false,
    error: null,
    scrollTop: 0,
    scrollLeft: 0,
    selected: null,
    detail: null,
    total: 0,
    initialLoaded: false,
    maxLane: 0,
    generation: 0,
    eventGeneration: 0,
    staleGeneration: 0,
  };
}

export function openTab(state: TabsState, tab: RepoTab): { state: TabsState; added: boolean } {
  const existing = state.tabs.find(item => item.path === tab.path);
  if (existing) return { state: { ...state, activePath: existing.path }, added: false };
  if (state.tabs.length >= MAX_TABS) return { state, added: false };
  return {
    state: { tabs: [...state.tabs, tab], activePath: tab.path },
    added: true,
  };
}

export function closeTab(state: TabsState, path: string): TabsState {
  const index = state.tabs.findIndex(tab => tab.path === path);
  if (index < 0) return state;
  const tabs = state.tabs.filter(tab => tab.path !== path);
  if (state.activePath !== path) return { ...state, tabs };
  const neighbor = tabs[Math.min(index, tabs.length - 1)];
  return { tabs, activePath: neighbor?.path ?? null };
}

export function cycleTabs(state: TabsState, direction: 1 | -1): TabsState {
  if (state.tabs.length < 2) return state;
  const index = state.tabs.findIndex(tab => tab.path === state.activePath);
  const current = index < 0 ? 0 : index;
  const next = (current + direction + state.tabs.length) % state.tabs.length;
  return { ...state, activePath: state.tabs[next].path };
}

export function restoreTabs(entries: unknown, activePath: string | null): TabsState {
  if (!Array.isArray(entries)) return { tabs: [], activePath: null };
  const tabs: RepoTab[] = [];
  const seen = new Set<string>();
  for (const entry of entries) {
    if (typeof entry !== "object" || entry === null || !("path" in entry)) continue;
    const path = (entry as { path?: unknown }).path;
    if (typeof path !== "string" || path.length === 0 || seen.has(path)) continue;
    seen.add(path);
    const name = "name" in entry && typeof entry.name === "string" ? entry.name : repositoryName(path);
    tabs.push(createTab(path, name));
    if (tabs.length === MAX_TABS) break;
  }
  const active = tabs.find(tab => tab.path === activePath)?.path ?? tabs[0]?.path ?? null;
  return { tabs, activePath: active };
}

function repositoryName(path: string): string {
  return path.replace(/[\\/]+$/, "").split(/[\\/]/).at(-1) || path;
}

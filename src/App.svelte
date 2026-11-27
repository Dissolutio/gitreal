<script lang="ts">
  import { onMount } from "svelte";
  import { open } from "@tauri-apps/plugin-dialog";
  import Tabs from "./Tabs.svelte";
  import GraphCanvas from "./GraphCanvas.svelte";
  import VirtualList from "./VirtualList.svelte";
  import Snackbar from "./Snackbar.svelte";
  import ContextMenu from "./ContextMenu.svelte";
  import { api, desktop, nativeError, onJobFinished, onJobStarted, onRepoChanged, onRepoError, type CommitDetail, type GraphRow, type KnownRepo, type NativeError, type RefInfo, type Repository } from "./lib/api";
  import { closeTab, createTab, cycleTabs, MAX_TABS, openTab, restoreTabs, type RepoTab } from "./lib/tabs";
  import { PageCache, PAGE_SIZE, ROW_HEIGHT, pageOffsets, visibleRange } from "./lib/virtual";
  import { snackbarStore, type JobAction, type JobFinishedEvent, type JobStartedEvent, type SnackbarEntry } from "./lib/snackbar";
  import { branchMenuItems, commitMenuItems, mergeCommitRefLabels, refIcon, type MenuItem } from "./lib/menus";
  import { validateBranchName } from "./lib/refnames";

  type MenuTarget = { kind: "ref"; ref: RefInfo } | { kind: "commit"; oid: string };
  type PromptState = { kind: "create" | "rename"; x: number; y: number; ref?: RefInfo; oid: string; name: string; checkout: boolean; message: string };

  let tabs = $state<RepoTab[]>([]);
  let activePath = $state<string | null>(null);
  let knownRepos = $state<KnownRepo[]>([]);
  let activeTab = $derived(tabs.find(tab => tab.path === activePath) ?? null);
  let repository = $state<Repository | null>(null);
  let refs = $state<RefInfo[]>([]);
  let filter = $state("");
  let collapsed = $state<Record<string, boolean>>({});
  let opening = $state(false);
  let choosing = $state(false);
  let pushMenuOpen = $state(false);
  let forceConfirmation = $state<{ repoId: string; branch: string; remote: string } | null>(null);
  let contextMenu = $state<{ x: number; y: number; target: MenuTarget } | null>(null);
  let inlinePrompt = $state<PromptState | null>(null);
  let remoteCheckoutPrompt = $state<{ local: RefInfo; remote: RefInfo; x: number; y: number } | null>(null);
  let stashPopover = $state(false);
  let stashIncludeUntracked = $state(false);
  let runningRepoIds = $state<string[]>([]);
  let requestingRepoIds = $state<string[]>([]);
  let error = $state<NativeError | null>(null);
  let detailError = $state<NativeError | null>(null);
  let selected = $state<string | null>(null);
  let detail = $state<CommitDetail | null>(null);
  let detailLoading = $state(false);
  let total = $state(0);
  let maxLane = $state(0);
  let initialLoaded = $state(false);
  let scrollTop = $state(0);
  let scrollLeft = $state(0);
  let viewportHeight = $state(600);
  let viewport: HTMLDivElement;
  let revision = $state(0);
  let generation = 0;
  let eventGeneration = 0;
  let epoch = 0;
  let selectionEpoch = 0;
  let cache = new PageCache<GraphRow>();
  const caches = new Map<string, PageCache<GraphRow>>();
  const pending = new Set<number>();
  const failed = new Set<number>();
  const openingTabs = new Set<number>();
  const groups = [{ kind: "local", label: "Local", icon: "⑂" }, { kind: "remote", label: "Remote", icon: "◎" }, { kind: "tag", label: "Tags", icon: "◇" }];
  let initialized = false;
  let disposed = false;
  const runningJobs = new Map<string, string>();
  const activeRepoRunning = $derived(!!repository && (
    runningRepoIds.includes(repository.repo_id) || requestingRepoIds.includes(repository.repo_id)
  ));
  const currentBranch = $derived(refs.find(ref => ref.kind === "local" && ref.is_head)?.name ?? null);
  const contextItems = $derived.by(() => {
    if (!contextMenu) return [];
    if (contextMenu.target.kind === "commit") return commitMenuItems(activeRepoRunning);
    const ref = contextMenu.target.ref;
    const counterpart = ref.kind === "local"
      ? refs.some(candidate => candidate.kind === "remote" && candidate.name.endsWith(`/${ref.name}`))
      : ref.kind === "remote" && refs.some(candidate =>
        candidate.kind === "local" && candidate.name === ref.name.slice(ref.name.indexOf("/") + 1),
      );
    return branchMenuItems({
      ref,
      running: activeRepoRunning,
      hasLocalCounterpart: ref.kind === "remote" && counterpart,
      hasRemoteCounterpart: ref.kind === "local" && counterpart,
    });
  });
  let persistTimer: ReturnType<typeof setTimeout> | undefined;
  const range = $derived(visibleRange(scrollTop, viewportHeight, total));
  const visibleRows = $derived.by(() => {
    void revision;
    return Array.from({ length: range.end - range.start }, (_, i) => cache.row(range.start + i));
  });
  const maxLaneWidth = $derived(Math.max(168, 32 + maxLane * 15));
  const graphWidth = $derived(Math.min(360, maxLaneWidth));
  const filtered = $derived(refs.filter(ref => ref.name.toLowerCase().includes(filter.toLowerCase())));
  const head = $derived(refs.find(ref => ref.kind === "local" && ref.is_head)?.name ?? refs.find(ref => ref.is_head)?.name ?? "Detached HEAD");
  const date = (timestamp: number) => new Date(timestamp * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" });

  function saveActiveTab() {
    const tab = tabs.find(item => item.path === activePath);
    if (!tab) return;
    tab.selected = selected;
    tab.detail = detail;
    tab.total = total;
    tab.initialLoaded = initialLoaded;
    tab.maxLane = maxLane;
    tab.generation = generation;
    tab.eventGeneration = eventGeneration;
    tab.scrollTop = scrollTop;
    tab.scrollLeft = scrollLeft;
    if (cache.size) caches.set(tab.path, cache);
  }

  function activateView(tab: RepoTab | undefined) {
    repository = tab?.repository ?? null;
    refs = tab?.refs ?? [];
    error = null;
    detailError = null;
    selected = tab?.selected ?? null;
    detail = tab?.detail ?? null;
    detailLoading = false;
    total = tab?.total ?? 0;
    initialLoaded = tab?.initialLoaded ?? false;
    maxLane = tab?.maxLane ?? 0;
    generation = tab?.generation ?? 0;
    eventGeneration = tab?.eventGeneration ?? 0;
    scrollTop = tab?.scrollTop ?? 0;
    scrollLeft = tab?.scrollLeft ?? 0;
    cache = tab ? caches.get(tab.path) ?? new PageCache<GraphRow>() : new PageCache<GraphRow>();
    if (tab) caches.set(tab.path, cache);
    pending.clear();
    failed.clear();
    revision++;
    if (viewport) {
      viewport.scrollTop = scrollTop;
      viewport.scrollLeft = scrollLeft;
    }
    // An inactive tab only records that its repository changed; catch up now.
    if (tab?.repository && tab.staleGeneration > eventGeneration) {
      void refreshRepository({ repo_id: tab.repository.repo_id, generation: tab.staleGeneration });
    }
  }

  function schedulePersist() {
    if (!initialized || !desktop) return;
    clearTimeout(persistTimer);
    persistTimer = setTimeout(() => {
      void api.setOpenTabs(tabs.map(tab => tab.path), activePath)
        .catch(failure => { error = nativeError(failure); });
    }, 500);
  }

  function focusTab(path: string) {
    const tab = tabs.find(item => item.path === path);
    if (!tab) return;
    saveActiveTab();
    ++epoch;
    ++selectionEpoch;
    pushMenuOpen = false;
    forceConfirmation = null;
    contextMenu = null;
    inlinePrompt = null;
    remoteCheckoutPrompt = null;
    activePath = tab.path;
    activateView(tab);
    opening = openingTabs.has(tab.tabId);
    schedulePersist();
    if (!tab.repository && !tab.error) void openRepositoryTab(tab, false);
  }

  async function openRepositoryTab(tab: RepoTab, remember: boolean) {
    const openingPath = tab.path;
    if (openingTabs.has(tab.tabId)) return;
    openingTabs.add(tab.tabId);
    if (activePath === openingPath) opening = true;
    let opened: Repository | null = null;
    try {
      const next = await api.open(openingPath);
      opened = next;
      if (disposed || !tabs.some(item => item.tabId === tab.tabId)) {
        void api.close(next.repo_id).catch(() => {});
        return;
      }
      const duplicate = tabs.find(item => item.tabId !== tab.tabId && item.path === next.path);
      if (duplicate) {
        void api.close(next.repo_id).catch(() => {});
        const closed = closeTab({ tabs, activePath }, openingPath);
        tabs = closed.tabs;
        if (activePath === openingPath) {
          activePath = duplicate.path;
          activateView(duplicate);
        }
        schedulePersist();
        return;
      }
      const oldPath = tab.path;
      tab.path = next.path;
      tab.name = next.name;
      tab.repository = next;
      tab.error = null;
      if (activePath === oldPath) activePath = next.path;
      const loaded = await api.refs(next.repo_id);
      if (disposed || !tabs.some(item => item.tabId === tab.tabId)) {
        void api.close(next.repo_id).catch(() => {});
        opened = null;
        return;
      }
      opened = null;
      tab.refs = loaded.refs;
      tab.generation = loaded.generation;
      tab.eventGeneration = loaded.generation;
      if (activePath === next.path) activateView(tab);
      if (remember) {
        try {
          await api.addKnownRepo(next.path);
          knownRepos = (await api.config()).repos;
        } catch (failure) { error = nativeError(failure); }
      }
      schedulePersist();
    } catch (failure) {
      if (opened) void api.close(opened.repo_id).catch(() => {});
      if (!disposed && tabs.some(item => item.tabId === tab.tabId)) {
        tab.error = nativeError(failure);
        tab.repository = null;
        if (activePath === openingPath || activePath === tab.path) {
          repository = null;
          refs = [];
          opening = false;
        }
      }
    } finally {
      openingTabs.delete(tab.tabId);
      if (activeTab?.tabId === tab.tabId) opening = false;
    }
  }

  async function openTabPath(path: string, remember = false) {
    const existing = tabs.find(tab => tab.path === path);
    if (existing) {
      focusTab(existing.path);
      if (remember) {
        try {
          await api.addKnownRepo(existing.path);
          knownRepos = (await api.config()).repos;
        } catch (failure) { error = nativeError(failure); }
      }
      return;
    }
    if (tabs.length >= MAX_TABS) {
      error = { code: "tab_limit", message: `Gitreal can keep up to ${MAX_TABS} repository tabs open.` };
      return;
    }
    const result = openTab({ tabs, activePath }, createTab(path));
    tabs = result.state.tabs;
    activePath = result.state.activePath;
    schedulePersist();
    const tab = tabs.find(item => item.path === path);
    if (!tab) return;
    activateView(tab);
    opening = true;
    void openRepositoryTab(tab, remember);
  }

  function closeRepositoryTab(path: string) {
    if (!tabs.some(item => item.path === path)) return;
    const wasActive = activePath === path;
    saveActiveTab();
    const tab = tabs.find(item => item.path === path);
    const next = closeTab({ tabs, activePath }, path);
    tabs = next.tabs;
    activePath = next.activePath;
    if (tab?.repository) void api.close(tab.repository.repo_id).catch(() => {});
    caches.delete(path);
    if (!wasActive) {
      schedulePersist();
      return;
    }
    ++epoch;
    ++selectionEpoch;
    activateView(tabs.find(item => item.path === activePath));
    opening = !!activeTab && openingTabs.has(activeTab.tabId);
    schedulePersist();
  }

  function cycleTabsInOrder(direction: 1 | -1) {
    const next = cycleTabs({ tabs, activePath }, direction);
    if (next.activePath && next.activePath !== activePath) focusTab(next.activePath);
  }

  async function chooseRepository() {
    if (!desktop || choosing) return;
    choosing = true;
    try {
      const path = await open({ directory: true, multiple: false, title: "Open Git repository" });
      if (typeof path === "string") await openTabPath(path, true);
    } catch (failure) { error = nativeError(failure); }
    finally { choosing = false; }
  }

  async function restoreConfig() {
    try {
      const config = await api.config();
      knownRepos = config.repos;
      const restored = restoreTabs(config.tabs, config.active_tab);
      const currentTabs = tabs;
      const currentActivePath = activePath;
      const mergedTabs = restored.tabs.map(
        tab => currentTabs.find(existing => existing.path === tab.path) ?? tab,
      );
      for (const tab of currentTabs) {
        if (!mergedTabs.some(existing => existing.path === tab.path)) mergedTabs.push(tab);
      }
      tabs = mergedTabs;
      activePath = currentActivePath && currentTabs.some(tab => tab.path === currentActivePath)
        ? currentActivePath
        : restored.activePath;
      initialized = true;
      activateView(tabs.find(tab => tab.path === activePath));
      schedulePersist();
      await Promise.all(tabs.map(tab => openRepositoryTab(tab, false)));
    } catch (failure) {
      initialized = true;
      error = nativeError(failure);
    }
  }

  async function loadPage(repoId: string, offset: number, token: number) {
    pending.add(offset);
    try {
      const page = await api.page(repoId, offset, PAGE_SIZE);
      if (token !== epoch) return;
      if (page.generation < generation) return;
      generation = page.generation;
      cache.set(offset, page.rows);
      total = page.total;
      maxLane = page.max_lane;
      initialLoaded = true;
      saveActiveTab();
    } catch (failure) {
      if (token !== epoch) return;
      failed.add(offset);
      error = nativeError(failure);
    } finally {
      if (token === epoch) { pending.delete(offset); revision++; }
    }
  }

  $effect(() => {
    void revision;
    if (!repository) return;
    const offsets = initialLoaded ? pageOffsets(range.start, range.end) : [0];
    for (const offset of offsets) {
      if (pending.size >= 3) break;
      if (!cache.has(offset) && !pending.has(offset) && !failed.has(offset)) {
        void loadPage(repository.repo_id, offset, epoch);
      }
    }
  });

  async function selectCommit(commitId: string) {
    if (!repository) return;
    const repoId = repository.repo_id;
    const token = ++selectionEpoch;
    const repoToken = epoch;
    selected = commitId; detail = null; detailError = null; detailLoading = true;
    saveActiveTab();
    try {
      const result = await api.detail(repoId, commitId);
      if (token === selectionEpoch && repoToken === epoch) { detail = result; saveActiveTab(); }
    } catch (failure) {
      if (token === selectionEpoch && repoToken === epoch) detailError = nativeError(failure);
    } finally {
      if (token === selectionEpoch && repoToken === epoch) detailLoading = false;
    }
  }

  async function refreshRepository(event: { repo_id: string; generation: number }) {
    const owner = tabs.find(tab => tab.repository?.repo_id === event.repo_id);
    if (owner && owner.repository?.repo_id !== repository?.repo_id) {
      // No work for inactive tabs: drop their cached pages and refresh on focus.
      owner.staleGeneration = Math.max(owner.staleGeneration, event.generation);
      caches.delete(owner.path);
      return;
    }
    if (!repository || event.repo_id !== repository.repo_id || event.generation <= eventGeneration) return;
    const repoId = repository.repo_id;
    eventGeneration = event.generation;
    const token = ++epoch;
    const selectedAtRefresh = selected;
    const selectionToken = selectionEpoch;
    cache.clear(); pending.clear(); failed.clear(); revision++;
    error = null;
    detail = null;
    detailError = null;
    detailLoading = !!selectedAtRefresh;
    try {
      const result = await api.refs(repoId);
      if (token !== epoch || repository?.repo_id !== repoId) return;
      refs = result.refs;
      generation = Math.max(event.generation, result.generation);
      const tab = tabs.find(item => item.repository?.repo_id === repoId);
      if (tab) {
        tab.refs = result.refs;
        tab.staleGeneration = 0;
      }
      saveActiveTab();
      opening = false;
      if (selectedAtRefresh && selectionToken === selectionEpoch) {
        try {
          const nextDetail = await api.detail(repoId, selectedAtRefresh);
          if (token !== epoch || selectionToken !== selectionEpoch) return;
          detail = nextDetail;
          detailError = null;
          detailLoading = false;
        } catch (failure) {
          if (token !== epoch || selectionToken !== selectionEpoch) return;
          const nextError = nativeError(failure);
          if (nextError.code === "unknown_commit") {
            selected = null;
            detail = null;
            detailError = null;
          } else {
            detail = null;
            detailError = nextError;
          }
          detailLoading = false;
        }
      }
    } catch (failure) {
      if (token === epoch) {
        error = nativeError(failure);
        opening = false;
        detailLoading = false;
      }
    } finally {
      if (token === epoch) revision++;
    }
  }

  function reportRepositoryError(event: { repo_id: string; message: string }) {
    if (repository?.repo_id === event.repo_id) {
      error = { code: "repo_error", message: event.message };
    }
  }

  function retry() { error = null; failed.clear(); revision++; }
  function reopenActiveTab() {
    if (!activeTab) return;
    activeTab.error = null;
    void openRepositoryTab(activeTab, false);
  }
  function onScroll() { scrollTop = viewport.scrollTop; scrollLeft = viewport.scrollLeft; saveActiveTab(); }

  function repositoryNameFor(repoId: string) {
    return tabs.find(tab => tab.repository?.repo_id === repoId)?.name ?? repoId;
  }

  function startJob(event: JobStartedEvent) {
    requestingRepoIds = requestingRepoIds.filter(repoId => repoId !== event.repo_id);
    runningJobs.set(event.job_id, event.repo_id);
    runningRepoIds = [...new Set(runningJobs.values())];
    snackbarStore.jobStarted(event, repositoryNameFor(event.repo_id));
  }

  function finishJob(event: JobFinishedEvent) {
    requestingRepoIds = requestingRepoIds.filter(repoId => repoId !== event.repo_id);
    runningJobs.delete(event.job_id);
    runningRepoIds = [...new Set(runningJobs.values())];
    snackbarStore.jobFinished(event, repositoryNameFor(event.repo_id));
  }

  function showOperationError(repo: Repository, kind: string, label: string, failure: unknown) {
    const native = nativeError(failure);
    const event: JobFinishedEvent = {
      job_id: `request-${Date.now()}-${Math.random().toString(36).slice(2)}`,
      repo_id: repo.repo_id,
      kind,
      label,
      ok: false,
      message: native.message,
      stderr_tail: "",
    };
    snackbarStore.failed(event, repo.name);
  }

  async function confirmForcePush() {
    if (!repository || !currentBranch) return;
    const repo = repository;
    const branch = currentBranch;
    pushMenuOpen = false;
    try {
      const remote = await api.pushRemote(repo.repo_id, branch);
      if (repository?.repo_id === repo.repo_id && currentBranch === branch) {
        forceConfirmation = { repoId: repo.repo_id, branch, remote };
      }
    } catch (failure) {
      showOperationError(repo, "force-push", `Force push ${branch}`, failure);
    }
  }

  async function executeForcePush() {
    const confirmation = forceConfirmation;
    if (!confirmation) return;
    if (repository?.repo_id !== confirmation.repoId || currentBranch !== confirmation.branch) {
      forceConfirmation = null;
      return;
    }
    const repo = repository;
    try {
      const remote = await api.pushRemote(confirmation.repoId, confirmation.branch);
      if (remote !== confirmation.remote) {
        forceConfirmation = null;
        showOperationError(repo, "force-push", `Force push ${confirmation.branch}`, {
          code: "push_target_changed",
          message: "The configured push remote changed; confirm the operation again",
        });
        return;
      }
      if (repository?.repo_id !== confirmation.repoId || currentBranch !== confirmation.branch) {
        forceConfirmation = null;
        return;
      }
      forceConfirmation = null;
      await runOperation("force-push", confirmation);
    } catch (failure) {
      forceConfirmation = null;
      showOperationError(repo, "force-push", `Force push ${confirmation.branch}`, failure);
    }
  }

  async function runOperation(
    kind: "fetch" | "pull" | "push" | "force-push",
    expected?: { repoId: string; branch: string },
  ) {
    if (!repository || activeRepoRunning) return;
    const repo = repository;
    if (expected && (repo.repo_id !== expected.repoId || currentBranch !== expected.branch)) return;
    const branch = expected?.branch ?? currentBranch;
    const label = kind === "force-push" ? `Force push ${branch ?? ""}` : `${kind[0].toUpperCase()}${kind.slice(1)}${kind === "push" ? ` ${branch ?? ""}` : ""}`;
    requestingRepoIds = [...new Set([...requestingRepoIds, repo.repo_id])];
    let enqueued = false;
    try {
      if (kind === "fetch") await api.fetch(repo.repo_id);
      else if (kind === "pull") await api.pull(repo.repo_id);
      else if (kind === "push" && branch) await api.push(repo.repo_id, branch);
      else if (kind === "force-push" && branch) await api.forcePush(repo.repo_id, branch);
      enqueued = true;
    } catch (failure) {
      showOperationError(repo, kind, label, failure);
    } finally {
      if (!enqueued) requestingRepoIds = requestingRepoIds.filter(repoId => repoId !== repo.repo_id);
    }
  }

  async function enqueueCustom(
    kind: string,
    label: string,
    operation: (repoId: string) => Promise<string>,
  ) {
    if (!repository || activeRepoRunning) return;
    const repo = repository;
    requestingRepoIds = [...new Set([...requestingRepoIds, repo.repo_id])];
    let enqueued = false;
    try {
      await operation(repo.repo_id);
      enqueued = true;
    } catch (failure) {
      showOperationError(repo, kind, label, failure);
    } finally {
      if (!enqueued) requestingRepoIds = requestingRepoIds.filter(repoId => repoId !== repo.repo_id);
    }
  }

  function showContextMenu(event: MouseEvent, target: MenuTarget) {
    event.preventDefault();
    event.stopPropagation();
    contextMenu = { x: event.clientX, y: event.clientY, target };
    inlinePrompt = null;
    remoteCheckoutPrompt = null;
  }

  function counterpartFor(ref: RefInfo) {
    if (ref.kind === "local") {
      return refs.find(candidate => candidate.kind === "remote" && candidate.name.endsWith(`/${ref.name}`));
    }
    if (ref.kind === "remote") {
      const branch = ref.name.slice(ref.name.indexOf("/") + 1);
      return refs.find(candidate => candidate.kind === "local" && candidate.name === branch);
    }
    return undefined;
  }

  function startRemoteCheckout(ref: RefInfo, event?: Event) {
    const local = counterpartFor(ref);
    if (local && local.target !== ref.target) {
      const bounds = event?.currentTarget instanceof HTMLElement
        ? event.currentTarget.getBoundingClientRect()
        : { left: 24, bottom: 120 };
      remoteCheckoutPrompt = { local, remote: ref, x: bounds.left, y: bounds.bottom };
      return;
    }
    void enqueueCustom("checkout-remote", `Checkout ${ref.name}`, repoId => api.checkoutRemote(repoId, ref.name));
  }

  function doubleClickRef(event: Event, ref: RefInfo) {
    event.preventDefault();
    if (ref.kind === "local" && !ref.is_head) {
      void enqueueCustom("checkout", `Checkout ${ref.name}`, repoId => api.checkoutLocal(repoId, ref.name));
    } else if (ref.kind === "remote") {
      startRemoteCheckout(ref, event);
    }
  }

  function startInlinePrompt(kind: "create" | "rename", target: MenuTarget) {
    const source = target.kind === "ref" ? target.ref : undefined;
    const bounds = contextMenu;
    inlinePrompt = {
      kind,
      x: bounds?.x ?? 100,
      y: bounds?.y ?? 100,
      ref: source,
      oid: target.kind === "commit" ? target.oid : source?.target ?? "",
      name: kind === "rename" ? source?.name ?? "" : "new-branch",
      checkout: false,
      message: "",
    };
  }

  async function submitInlinePrompt(event: SubmitEvent) {
    event.preventDefault();
    const prompt = inlinePrompt;
    if (!prompt || !repository) return;
    const validation = validateBranchName(prompt.name);
    if (validation) {
      inlinePrompt = { ...prompt, message: validation };
      return;
    }
    inlinePrompt = null;
    if (prompt.kind === "create") {
      await enqueueCustom("create-branch", `Create branch ${prompt.name}`, repoId =>
        api.createBranch(repoId, prompt.name, prompt.oid, prompt.checkout),
      );
    } else if (prompt.ref?.kind === "local") {
      await enqueueCustom("rename-branch", `Rename ${prompt.ref.name}`, repoId =>
        api.renameBranch(repoId, prompt.ref!.name, prompt.name),
      );
    }
  }

  function remoteBranchParts(ref: RefInfo) {
    const separator = ref.name.indexOf("/");
    return separator < 0
      ? null
      : { remote: ref.name.slice(0, separator), branch: ref.name.slice(separator + 1) };
  }

  async function selectContextMenuItem(id: string) {
    const target = contextMenu?.target;
    contextMenu = null;
    if (!target || !repository) return;
    if (target.kind === "commit") {
      if (id === "checkout-commit" && window.confirm(`Checkout commit ${target.oid.slice(0, 7)} in detached HEAD?`)) {
        await enqueueCustom("checkout-commit", `Checkout commit ${target.oid.slice(0, 7)}`, repoId =>
          api.checkoutCommit(repoId, target.oid),
        );
      } else if (id === "create-branch") {
        startInlinePrompt("create", target);
      } else if (id === "copy-commit") {
        await navigator.clipboard.writeText(target.oid).catch(() => {});
      }
      return;
    }
    const ref = target.ref;
    const local = ref.kind === "local" ? ref : counterpartFor(ref);
    const remote = ref.kind === "remote" ? ref : counterpartFor(ref);
    if (id === "checkout") {
      if (ref.kind === "local") {
        await enqueueCustom("checkout", `Checkout ${ref.name}`, repoId => api.checkoutLocal(repoId, ref.name));
      } else if (ref.kind === "remote") {
        startRemoteCheckout(ref);
      }
    } else if (id === "create-branch" && ref.target) {
      startInlinePrompt("create", target);
    } else if (id === "rename" && ref.kind === "local") {
      startInlinePrompt("rename", target);
    } else if (id === "pull" && ref.kind === "local") {
      await enqueueCustom("pull", `Pull ${ref.name}`, repoId => api.pull(repoId));
    } else if (id === "push" && ref.kind === "local") {
      await enqueueCustom("push", `Push ${ref.name}`, repoId => api.push(repoId, ref.name));
    } else if (id === "delete-local" && ref.kind === "local") {
      if (window.confirm(`Delete local branch '${ref.name}'?`)) {
        await enqueueCustom("delete-local", `Delete local branch ${ref.name}`, repoId => api.deleteLocal(repoId, ref.name, false));
      }
    } else if (id === "delete-remote" && ref.kind === "remote") {
      const parts = remoteBranchParts(ref);
      if (parts && window.confirm(`Delete remote branch '${ref.name}'?`)) {
        await enqueueCustom("delete-remote", `Delete remote branch ${ref.name}`, repoId =>
          api.deleteRemote(repoId, parts.remote, parts.branch),
        );
      }
    } else if (id === "delete-both" && local && remote) {
      const parts = remoteBranchParts(remote);
      if (parts && window.confirm(`Delete local branch '${local.name}' and remote branch '${remote.name}'?`)) {
        await enqueueCustom("delete-local-and-remote", `Delete local and remote branch ${local.name}`, repoId =>
          api.deleteLocalAndRemote(repoId, local.name, parts.remote, parts.branch),
        );
      }
    } else if (id === "reset-remote" && local && remote?.target && local.target) {
      if (window.confirm(`Reset local branch '${local.name}' to remote branch '${remote.name}'?`)) {
        await enqueueCustom("reset-local-to-remote", `Reset ${local.name} to ${remote.name}`, repoId =>
          api.resetLocalToRemote(repoId, local.name, remote.name, local.target!),
        );
      }
    } else if (id === "copy") {
      await navigator.clipboard.writeText(ref.name).catch(() => {});
    }
  }

  async function followUpAction(action: JobAction, entry: SnackbarEntry) {
    if (action.kind === "force_delete") {
      const branch = String(action.payload?.branch ?? "");
      if (branch && repository?.repo_id === entry.repo_id) {
        const remote = action.payload?.remote ? String(action.payload.remote) : undefined;
        const remoteBranch = action.payload?.remote_branch ? String(action.payload.remote_branch) : undefined;
        await enqueueCustom("delete-local", `Force delete local branch ${branch}`, async repoId => {
          const jobId = await api.deleteLocal(repoId, branch, true);
          if (remote && remoteBranch) await api.deleteRemote(repoId, remote, remoteBranch);
          return jobId;
        });
      }
    } else if (action.kind === "stash_and_retry") {
      const targetKind = String(action.payload?.target_kind ?? "");
      const target = String(action.payload?.target ?? "");
      if (targetKind && target && repository?.repo_id === entry.repo_id) {
        await enqueueCustom("stash-and-checkout", action.label, repoId =>
          api.stashAndCheckout(repoId, targetKind, target),
        );
      }
    }
  }

  async function runStash(includeUntracked: boolean) {
    stashPopover = false;
    await enqueueCustom("stash", "Stash changes", repoId => api.stashPush(repoId, includeUntracked));
  }

  async function runStashPop() {
    await enqueueCustom("stash-pop", "Pop stash", repoId => api.stashPop(repoId));
  }

  onMount(() => {
    let unlistenChanged: (() => void) | undefined;
    let unlistenError: (() => void) | undefined;
    let unlistenJobStarted: (() => void) | undefined;
    let unlistenJobFinished: (() => void) | undefined;
    void onRepoChanged(event => { void refreshRepository(event); }).then(unlisten => {
      if (disposed) unlisten();
      else unlistenChanged = unlisten;
    }).catch(failure => { error = nativeError(failure); });
    void onJobStarted(startJob).then(unlisten => {
      if (disposed) unlisten();
      else unlistenJobStarted = unlisten;
    }).catch(failure => { error = nativeError(failure); });
    void onJobFinished(finishJob).then(unlisten => {
      if (disposed) unlisten();
      else unlistenJobFinished = unlisten;
    }).catch(failure => { error = nativeError(failure); });
    void onRepoError(reportRepositoryError).then(unlisten => {
      if (disposed) unlisten();
      else unlistenError = unlisten;
    }).catch(failure => { error = nativeError(failure); });
    const observer = new ResizeObserver(() => { viewportHeight = viewport.clientHeight; });
    observer.observe(viewport);
    viewportHeight = viewport.clientHeight;
    if (desktop) void restoreConfig();
    return () => {
      disposed = true;
      clearTimeout(persistTimer);
      unlistenChanged?.();
      unlistenError?.();
      unlistenJobStarted?.();
      unlistenJobFinished?.();
      observer.disconnect();
      ++epoch; ++selectionEpoch;
      for (const tab of tabs) {
        if (tab.repository) void api.close(tab.repository.repo_id).catch(() => {});
      }
    };
  });
</script>

<svelte:head><title>{repository ? `${repository.name} · Gitreal` : "Gitreal — Repository explorer"}</title></svelte:head>

<div class="workspace">
  <header class="topbar">
    <div class="brand"><span class="brand-mark">⑂</span> gitreal<span class="brand-caption">REPOSITORY EXPLORER</span></div>
    {#if activeRepoRunning}<div class="job-indicator"><span class="snackbar-spinner"></span> Git operation running</div>{/if}
  </header>

  <Tabs
    {tabs}
    {activePath}
    {knownRepos}
    onFocus={focusTab}
    onClose={closeRepositoryTab}
    onOpenKnown={(path) => openTabPath(path, true)}
    onOpenFolder={chooseRepository}
    onCycle={cycleTabsInOrder}
  />

  <aside class="sidebar">
    <div class="repository-block">
      <span class="eyebrow">WORKSPACE</span>
      <h1 title={repository?.path ?? activeTab?.path}><span class="repo-icon">▣</span> {repository?.name ?? activeTab?.name ?? "No repository"}</h1>
      <p title={repository?.path}>{repository?.path ?? "Your history, in perspective."}</p>
    </div>
    <div class="sidebar-heading">REFERENCES <span>{refs.length}</span></div>
    <label class="filter"><span aria-hidden="true">⌕</span><input aria-label="Filter references" placeholder="Filter references…" bind:value={filter} disabled={!repository} /></label>
    <nav aria-label="Repository references">
      {#each groups as group}
        {@const items = filtered.filter(ref => ref.kind === group.kind || (group.kind === "local" && ref.kind === "head"))}
        <section class="ref-group">
          <button class="group-toggle" aria-expanded={!collapsed[group.kind]} onclick={() => { collapsed[group.kind] = !collapsed[group.kind]; }}>
            <span class:closed={collapsed[group.kind]} class="chevron">⌄</span><span class="group-icon">{group.icon}</span>{group.label}<span class="count">{items.length}</span>
          </button>
          {#if !collapsed[group.kind]}
            <VirtualList {items} maxHeight={216}>
              {#snippet children(ref)}
                <button class="ref-item" class:current={ref.is_head} title={ref.full_name} disabled={!ref.target}
                  onclick={() => { if (ref.target) void selectCommit(ref.target); }}
                  ondblclick={(event) => doubleClickRef(event, ref)}
                  oncontextmenu={(event) => showContextMenu(event, { kind: "ref", ref })}>
                  <span class="ref-line" aria-hidden="true">{refIcon(ref.kind)}</span><span class="truncate">{ref.kind === "remote" ? ref.name.slice(ref.name.indexOf("/") + 1) : ref.name}</span>{#if ref.is_head}<span class="head-dot" title="HEAD"></span>{/if}
                </button>
              {/snippet}
            </VirtualList>
            {#if items.length === 0}<p class="no-refs">{filter ? "No matching references" : `No ${group.label.toLowerCase()}`}</p>{/if}
          {/if}
        </section>
      {/each}
    </nav>
    <div class="sidebar-bottom"><span class="status-dot"></span> Git on disk is source of truth.<small>Watching Git metadata · No polling</small></div>
  </aside>

  <main class="history">
    <div class="history-toolbar">
      <div><span class="eyebrow">REPOSITORY HISTORY</span><h2>{repository ? head : "Commit graph"}<span class="snapshot">LIVE</span></h2></div>
      <div class="history-actions">
        <span class="commit-count">{initialLoaded ? total.toLocaleString() : "—"} <span>commits</span></span>
        <div class="operation-toolbar">
          <div class="stash-menu">
            <button disabled={!repository || activeRepoRunning} onclick={() => { stashPopover = !stashPopover; }}>Stash</button>
            {#if stashPopover}
              <div class="stash-popover">
                <label><input type="checkbox" bind:checked={stashIncludeUntracked} /> Include untracked files</label>
                <button disabled={!repository || activeRepoRunning} onclick={() => { void runStash(stashIncludeUntracked); }}>Stash changes</button>
              </div>
            {/if}
          </div>
          <button disabled={!repository || activeRepoRunning} onclick={() => { void runStashPop(); }}>Pop</button>
          <button disabled={!repository || activeRepoRunning} onclick={() => runOperation("fetch")}>Fetch</button>
          <button disabled={!repository || !currentBranch || activeRepoRunning} onclick={() => runOperation("pull")}>Pull</button>
          <button disabled={!repository || !currentBranch || activeRepoRunning} onclick={() => runOperation("push")}>Push</button>
          <div class="push-menu">
            <button class="push-menu-toggle" aria-label="More push options" aria-expanded={pushMenuOpen} disabled={!repository || !currentBranch || activeRepoRunning} onclick={() => { pushMenuOpen = !pushMenuOpen; }}>⌄</button>
            {#if pushMenuOpen}
              <div class="push-menu-panel">
                <button onclick={() => { void confirmForcePush(); }}>Force push with lease…</button>
              </div>
            {/if}
            {#if forceConfirmation}
              <div class="force-confirm" role="dialog" aria-label="Confirm force push">
                <p>Force push {forceConfirmation.branch} to {forceConfirmation.remote} with lease?</p>
                <div><button onclick={() => { forceConfirmation = null; }}>Cancel</button><button class="force-confirm-action" onclick={() => { void executeForcePush(); }}>Force push</button></div>
              </div>
            {/if}
          </div>
        </div>
      </div>
    </div>
    {#if error}
      <div class="error-banner" role="alert"><div><strong>{error.code}</strong> {error.message}</div><button onclick={retry}>Retry pages</button>{#if repository}<button onclick={reopenActiveTab}>Reopen</button>{/if}<button aria-label="Dismiss error" onclick={() => { error = null; }}>×</button></div>
    {/if}
    <div class="column-clip"><div class="column-head" style:--graph-width="{graphWidth}px" style:transform="translateX(-{scrollLeft}px)"><span>REFERENCES</span><span>GRAPH</span><span>COMMIT MESSAGE</span><span>AUTHOR</span><span>DATE</span></div></div>
    <div class="history-scroll" bind:this={viewport} onscroll={onScroll} aria-label="Commit history" aria-busy={opening || (!!repository && !initialLoaded)}>
      {#if !repository}
        <div class="empty-state">
          <div class="empty-graph" aria-hidden="true"><i></i><i></i><i></i><span>⑂</span></div>
          <span class="eyebrow">A CLEARER VIEW OF YOUR CODE</span>
          <h2>{opening ? "Opening repository…" : activeTab?.error ? "Repository unavailable." : "Every commit tells a story."}</h2>
          <p>{activeTab?.error ? activeTab.error.message : desktop ? "Open a local Git repository to explore its branches, commits, and connections." : "Gitreal is a desktop Git explorer. Open the desktop app to browse a local repository."}</p>
          {#if activeTab?.error}<button class="primary-button" onclick={reopenActiveTab}>Try again</button>{/if}
          {#if desktop}<button class="primary-button" onclick={chooseRepository} disabled={opening || choosing}>▱ &nbsp; Open repository</button>{:else}<span class="desktop-note">DESKTOP APP REQUIRED</span>{/if}
          <div class="empty-features"><span>○ Virtualized history</span><span>⑂ Branch topology</span><span>◇ Commit details</span></div>
        </div>
      {:else if initialLoaded && total === 0}
        <div class="empty-state"><h2>A fresh beginning.</h2><p>This repository has no commits yet.</p></div>
      {:else}
        <div class="history-spacer" style:height="{total * ROW_HEIGHT}px" style:--graph-width="{graphWidth}px">
          {#each visibleRows as row, i (range.start + i)}
            <button class="commit-row" class:selected={row?.commit_id === selected} class:placeholder={!row}
              style:top="{(range.start + i) * ROW_HEIGHT}px" disabled={!row}
              aria-label={row ? `${row.summary}, ${row.author}, ${row.commit_id.slice(0, 7)}` : "Loading commit"}
              aria-pressed={!!row && row.commit_id === selected}
              onclick={() => { if (row) void selectCommit(row.commit_id); }}
              oncontextmenu={(event) => { if (row) showContextMenu(event, { kind: "commit", oid: row.commit_id }); }}>
              <span class="row-refs">{#if row}{@const labelItems = mergeCommitRefLabels(row.ref_labels, refs)}{#each labelItems.slice(0, 2) as label}
                <span class="ref-badge" class:current={label.refs.some(ref => ref.is_head)} role="button" tabindex="0" aria-label={`Checkout ${label.name}`} title={label.fullNames.join(", ")}
                  onkeydown={(event) => {
                    if (event.key === "Enter" || event.key === " ") {
                      event.preventDefault();
                      event.stopPropagation();
                      const ref = label.refs[0];
                      if (ref) doubleClickRef(event, ref);
                    }
                  }}
                  ondblclick={(event) => { event.stopPropagation(); const ref = label.refs[0]; if (ref) doubleClickRef(event, ref); }}
                  oncontextmenu={(event) => { const ref = label.refs[0]; if (ref) showContextMenu(event, { kind: "ref", ref }); }}>
                  {#each label.kinds as kind}<span class="ref-icon" aria-hidden="true">{kind === "remote" ? "☁" : kind === "tag" ? "◆" : kind === "local" ? "⑂" : "●"}</span>{/each}
                  <span>{label.name}</span>
                </span>
              {/each}{/if}</span>
              <span></span><span class="commit-message">{row?.summary ?? "Loading commits…"}{#if row && row.parents.length > 1}<span class="merge-label">MERGE</span>{/if}</span>
              <span class="commit-author">{row?.author ?? ""}</span><span class="commit-date">{row ? date(row.date) : ""}</span>
            </button>
          {/each}
        </div>
        <div class="canvas-position" style:left="156px" style:top="{scrollTop}px">
          <GraphCanvas rows={visibleRows} start={range.start} {scrollTop} height={viewportHeight} {selected} width={graphWidth} laneWidth={maxLaneWidth} />
        </div>
        {#if !initialLoaded}<div class="loading-history">Loading repository history…</div>{/if}
      {/if}
    </div>
    <footer class="history-footer"><span><span class="status-dot"></span>{repository ? "Live repository history" : "Ready when you are"}</span><span>{total ? `${(Math.floor(scrollTop / ROW_HEIGHT) + 1).toLocaleString()}–${Math.min(total, Math.ceil((scrollTop + viewportHeight) / ROW_HEIGHT)).toLocaleString()} of ${total.toLocaleString()}` : "No repository loaded"}</span></footer>
  </main>

  <aside class="details" aria-label="Commit details" aria-busy={detailLoading}>
    <div class="details-heading"><span>COMMIT DETAILS</span><span class="detail-symbol">⌘</span></div>
    {#if detailLoading}
      <div class="details-empty"><span class="details-icon">◷</span><h3>Loading commit…</h3></div>
    {:else if detailError}
      <div class="detail-error" role="alert"><strong>{detailError.code}</strong><p>{detailError.message}</p><button onclick={() => { if (selected) void selectCommit(selected); }}>Try again</button></div>
    {:else if detail}
      <div class="detail-content">
        <span class="eyebrow">COMMIT</span><code class="full-hash">{detail.commit_id}</code>
        <h2 class="detail-summary">{detail.summary}</h2>
        <div class="author-card"><span class="avatar">{detail.author.slice(0, 1).toUpperCase()}</span><div><strong>{detail.author}</strong><small>{detail.author_email}</small></div></div>
        <p class="detail-date">{new Date(detail.date * 1000).toLocaleString()}</p>
        {#if detail.message !== detail.summary}<pre class="commit-body">{detail.message}</pre>{/if}
        <div class="parent-section"><span class="eyebrow">PARENTS · {detail.parents.length}</span>
          <VirtualList items={detail.parents} maxHeight={108}>
            {#snippet children(parent)}<button class="parent-link" onclick={() => selectCommit(parent)} title={parent}>⑂ &nbsp; {parent.slice(0, 12)} <span>↗</span></button>{/snippet}
          </VirtualList>
          {#if !detail.parents.length}<p class="root-label">Initial commit</p>{/if}
        </div>
        <div class="paths-heading"><span class="eyebrow">CHANGED FILES</span><span class="count">{detail.changed_paths.length}</span></div>
        <VirtualList items={detail.changed_paths} maxHeight={360}>
          {#snippet children(change)}<div class="changed-path" title={change.old_path ? `${change.old_path} → ${change.path}` : change.path}><span class="file-status" class:added={change.status === "added"} class:deleted={change.status === "deleted"}>{change.status.slice(0, 1).toUpperCase()}</span><span class="truncate">{change.path}</span></div>{/snippet}
        </VirtualList>
        {#if !detail.changed_paths.length}<p class="root-label">No changed files</p>{/if}
      </div>
    {:else}
      <div class="details-empty"><span class="details-icon">◇</span><h3>The details matter.</h3><p>Select a commit to explore its author, message, parents, and changed files.</p><span class="keyboard-hint">CLICK A COMMIT TO INSPECT</span></div>
    {/if}
  </aside>
  <Snackbar onAction={followUpAction} />
  {#if contextMenu}
    <ContextMenu x={contextMenu.x} y={contextMenu.y} items={contextItems} onSelect={selectContextMenuItem} onClose={() => { contextMenu = null; }} />
  {/if}
  {#if inlinePrompt}
    <form class="branch-prompt" style:left="{inlinePrompt.x}px" style:top="{inlinePrompt.y}px" onsubmit={submitInlinePrompt}>
      <label>{inlinePrompt.kind === "rename" ? "Rename local branch" : "Create branch"}<input bind:value={inlinePrompt.name} /></label>
      {#if inlinePrompt.kind === "create"}<label class="prompt-check"><input type="checkbox" bind:checked={inlinePrompt.checkout} /> Check out after creating</label>{/if}
      {#if inlinePrompt.message}<p role="alert">{inlinePrompt.message}</p>{/if}
      <div><button type="button" onclick={() => { inlinePrompt = null; }}>Cancel</button><button type="submit" disabled={activeRepoRunning}>Save</button></div>
    </form>
  {/if}
  {#if remoteCheckoutPrompt}
    <div class="branch-prompt remote-checkout-prompt" style:left="{remoteCheckoutPrompt.x}px" style:top="{remoteCheckoutPrompt.y}px" role="dialog" aria-label="Choose remote branch checkout">
      <p>Local {remoteCheckoutPrompt.local.name} is at {remoteCheckoutPrompt.local.target?.slice(0, 7)}, {remoteCheckoutPrompt.remote.name} is at {remoteCheckoutPrompt.remote.target?.slice(0, 7)}.</p>
      <div>
        <button disabled={activeRepoRunning} onclick={() => { const local = remoteCheckoutPrompt!.local; remoteCheckoutPrompt = null; void enqueueCustom("checkout", `Checkout ${local.name}`, repoId => api.checkoutLocal(repoId, local.name)); }}>Checkout local</button>
        <button disabled={activeRepoRunning} onclick={() => { const prompt = remoteCheckoutPrompt!; remoteCheckoutPrompt = null; void enqueueCustom("reset-local-to-remote", `Reset ${prompt.local.name} to ${prompt.remote.name}`, repoId => api.resetLocalToRemote(repoId, prompt.local.name, prompt.remote.name, prompt.local.target!)); }}>Reset local to {remoteCheckoutPrompt.remote.name}</button>
        <button onclick={() => { remoteCheckoutPrompt = null; }}>Cancel</button>
      </div>
    </div>
  {/if}
</div>

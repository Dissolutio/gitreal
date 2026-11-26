<script lang="ts">
  import { onMount } from "svelte";
  import { open } from "@tauri-apps/plugin-dialog";
  import GraphCanvas from "./GraphCanvas.svelte";
  import VirtualList from "./VirtualList.svelte";
  import { api, desktop, nativeError, onRepoChanged, onRepoError, type CommitDetail, type GraphRow, type NativeError, type RefInfo, type Repository } from "./lib/api";
  import { PageCache, PAGE_SIZE, ROW_HEIGHT, pageOffsets, visibleRange } from "./lib/virtual";

  let repository = $state<Repository | null>(null);
  let refs = $state<RefInfo[]>([]);
  let filter = $state("");
  let collapsed = $state<Record<string, boolean>>({});
  let opening = $state(false);
  let choosing = $state(false);
  let error = $state<NativeError | null>(null);
  let detailError = $state<NativeError | null>(null);
  let selected = $state<string | null>(null);
  let detail = $state<CommitDetail | null>(null);
  let detailLoading = $state(false);
  let total = $state(0);
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
  const cache = new PageCache<GraphRow>();
  const pending = new Set<number>();
  const failed = new Set<number>();
  const groups = [{ kind: "local", label: "Local", icon: "⑂" }, { kind: "remote", label: "Remote", icon: "◎" }, { kind: "tag", label: "Tags", icon: "◇" }];
  const range = $derived(visibleRange(scrollTop, viewportHeight, total));
  const visibleRows = $derived.by(() => {
    void revision;
    return Array.from({ length: range.end - range.start }, (_, i) => cache.row(range.start + i));
  });
  const maxLaneWidth = $derived(Math.max(168, ...visibleRows.flatMap(row => row ? [
    32 + row.lane * 15, ...row.parent_edges.map(edge => 32 + Math.max(edge.from_lane, edge.to_lane) * 15),
  ] : [])));
  const graphWidth = $derived(Math.min(360, maxLaneWidth));
  const filtered = $derived(refs.filter(ref => ref.name.toLowerCase().includes(filter.toLowerCase())));
  const head = $derived(refs.find(ref => ref.kind === "local" && ref.is_head)?.name ?? refs.find(ref => ref.is_head)?.name ?? "Detached HEAD");
  const date = (timestamp: number) => new Date(timestamp * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" });

  async function chooseRepository() {
    if (!desktop || choosing || opening) return;
    choosing = true;
    try {
      const path = await open({ directory: true, multiple: false, title: "Open Git repository" });
      if (typeof path === "string") await loadRepository(path);
    } catch (failure) { error = nativeError(failure); }
    finally { choosing = false; }
  }

  async function loadRepository(path: string) {
    const token = ++epoch;
    ++selectionEpoch;
    const previous = repository;
    repository = null;
    refs = []; filter = ""; selected = null; detail = null; detailError = null;
    generation = 0; eventGeneration = 0;
    detailLoading = false; error = null; total = 0; initialLoaded = false;
    cache.clear(); pending.clear(); failed.clear(); revision++;
    scrollTop = 0; scrollLeft = 0;
    if (viewport) { viewport.scrollTop = 0; viewport.scrollLeft = 0; }
    opening = true;
    if (previous) void api.close(previous.repo_id).catch(() => {});
    try {
      const next = await api.open(path);
      if (token !== epoch) { void api.close(next.repo_id).catch(() => {}); return; }
      repository = next;
      const result = await api.refs(next.repo_id);
      if (token !== epoch) return;
      refs = result.refs;
      generation = result.generation;
      eventGeneration = result.generation;
    } catch (failure) {
      if (token === epoch) error = nativeError(failure);
    } finally { if (token === epoch) opening = false; }
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
      initialLoaded = true;
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
    try {
      const result = await api.detail(repoId, commitId);
      if (token === selectionEpoch && repoToken === epoch) detail = result;
    } catch (failure) {
      if (token === selectionEpoch && repoToken === epoch) detailError = nativeError(failure);
    } finally {
      if (token === selectionEpoch && repoToken === epoch) detailLoading = false;
    }
  }

  async function refreshRepository(event: { repo_id: string; generation: number }) {
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
  function onScroll() { scrollTop = viewport.scrollTop; scrollLeft = viewport.scrollLeft; }

  onMount(() => {
    let disposed = false;
    let unlistenChanged: (() => void) | undefined;
    let unlistenError: (() => void) | undefined;
    void onRepoChanged(event => { void refreshRepository(event); }).then(unlisten => {
      if (disposed) unlisten();
      else unlistenChanged = unlisten;
    }).catch(failure => { error = nativeError(failure); });
    void onRepoError(reportRepositoryError).then(unlisten => {
      if (disposed) unlisten();
      else unlistenError = unlisten;
    }).catch(failure => { error = nativeError(failure); });
    const observer = new ResizeObserver(() => { viewportHeight = viewport.clientHeight; });
    observer.observe(viewport);
    viewportHeight = viewport.clientHeight;
    return () => {
      disposed = true;
      unlistenChanged?.();
      unlistenError?.();
      observer.disconnect();
      ++epoch; ++selectionEpoch;
      if (repository) void api.close(repository.repo_id).catch(() => {});
    };
  });
</script>

<svelte:head><title>{repository ? `${repository.name} · Gitreal` : "Gitreal — Repository explorer"}</title></svelte:head>

<div class="workspace">
  <header class="topbar">
    <div class="brand"><span class="brand-mark">⑂</span> gitreal<span class="brand-caption">REPOSITORY EXPLORER</span></div>
    <div class="read-only"><span></span> READ ONLY</div>
    <button class="open-button" onclick={chooseRepository} disabled={!desktop || opening || choosing}><span aria-hidden="true">▱</span> {opening ? "Opening…" : "Open repository"}</button>
  </header>

  <aside class="sidebar">
    <div class="repository-block">
      <span class="eyebrow">WORKSPACE</span>
      <h1 title={repository?.path}><span class="repo-icon">▣</span> {repository?.name ?? "No repository"}</h1>
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
                  onclick={() => { if (ref.target) void selectCommit(ref.target); }}>
                  <span class="ref-line" aria-hidden="true">⑂</span><span class="truncate">{ref.name}</span>{#if ref.is_head}<span class="head-dot" title="HEAD"></span>{/if}
                </button>
              {/snippet}
            </VirtualList>
            {#if items.length === 0}<p class="no-refs">{filter ? "No matching references" : `No ${group.label.toLowerCase()}`}</p>{/if}
          {/if}
        </section>
      {/each}
    </nav>
    <div class="sidebar-bottom"><span class="status-dot"></span> Local. Private. Read-only.<small>Watching Git metadata · No polling</small></div>
  </aside>

  <main class="history">
    <div class="history-toolbar">
      <div><span class="eyebrow">REPOSITORY HISTORY</span><h2>{repository ? head : "Commit graph"}<span class="snapshot">LIVE</span></h2></div>
      <span class="commit-count">{initialLoaded ? total.toLocaleString() : "—"} <span>commits</span></span>
    </div>
    {#if error}
      <div class="error-banner" role="alert"><div><strong>{error.code}</strong> {error.message}</div><button onclick={retry}>Retry pages</button>{#if repository}<button onclick={() => { if (repository) void loadRepository(repository.path); }}>Reopen</button>{/if}<button aria-label="Dismiss error" onclick={() => { error = null; }}>×</button></div>
    {/if}
    <div class="column-clip"><div class="column-head" style:--graph-width="{graphWidth}px" style:transform="translateX(-{scrollLeft}px)"><span>REFERENCES</span><span>GRAPH</span><span>COMMIT MESSAGE</span><span>AUTHOR</span><span>DATE</span></div></div>
    <div class="history-scroll" bind:this={viewport} onscroll={onScroll} aria-label="Commit history" aria-busy={opening || (!!repository && !initialLoaded)}>
      {#if !repository}
        <div class="empty-state">
          <div class="empty-graph" aria-hidden="true"><i></i><i></i><i></i><span>⑂</span></div>
          <span class="eyebrow">A CLEARER VIEW OF YOUR CODE</span>
          <h2>{opening ? "Opening repository…" : "Every commit tells a story."}</h2>
          <p>{desktop ? "Open a local Git repository to explore its branches, commits, and connections." : "Gitreal is a desktop Git explorer. Open the desktop app to browse a local repository."}</p>
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
              aria-pressed={!!row && row.commit_id === selected} onclick={() => { if (row) void selectCommit(row.commit_id); }}>
              <span class="row-refs">{#if row}{#each row.ref_labels.slice(0, 2) as label}<span class="ref-badge" title={row.ref_labels.join(", ")}>{label.replace(/^refs\/(heads|remotes|tags)\//, "")}</span>{/each}{/if}</span>
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
</div>

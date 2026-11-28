<script lang="ts">
  import { onMount } from "svelte";
  import type { FileDiff } from "./lib/api";
  import { safeRefLabel } from "./lib/display";
  import { buildDiffRows, nextChangeIndex, type DiffDisplayRow, type DiffMode } from "./lib/filediff";

  const OVERSCAN = 8;

  let {
    diff,
    loading,
    error,
    onClose,
    onShowAnyway,
  }: {
    diff: FileDiff | null;
    loading: boolean;
    error: string | null;
    onClose: () => void;
    onShowAnyway: () => void;
  } = $props();

  let viewport = $state<HTMLDivElement>();
  let scrollTop = $state(0);
  let viewportHeight = $state(300);
  let mode = $state<DiffMode>("full_file");
  let expandedRegions = $state(new Set<string>());
  let expandAll = $state(false);
  let wrap = $state(false);
  let currentChange = $state(-1);
  const ROW_HEIGHT = $derived(wrap ? 44 : 24);
  const rows = $derived(diff ? buildDiffRows(diff.lines, mode, expandedRegions, expandAll) : []);
  const start = $derived(Math.min(rows.length, Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - OVERSCAN)));
  const end = $derived(Math.min(rows.length, Math.ceil((scrollTop + viewportHeight) / ROW_HEIGHT) + OVERSCAN));
  const visibleRows = $derived(rows.slice(start, end));

  onMount(() => onScroll());

  function onScroll() {
    if (!viewport) return;
    scrollTop = viewport.scrollTop;
    viewportHeight = viewport.clientHeight;
  }

  function toggleRegion(key: string) {
    const next = new Set(expandedRegions);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    expandedRegions = next;
  }

  function jumpToChange(direction: 1 | -1) {
    if (!diff) return;
    currentChange = nextChangeIndex(diff.lines, currentChange, direction);
    if (currentChange < 0) return;
    const rowIndex = rows.findIndex(row => row.kind === "line" && row.index === currentChange);
    if (rowIndex >= 0 && viewport) viewport.scrollTop = rowIndex * ROW_HEIGHT;
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      onClose();
    } else if (event.key.toLowerCase() === "n" || event.key.toLowerCase() === "p") {
      if (event.target instanceof HTMLElement && event.target.closest("input, textarea, select, [contenteditable=true]")) return;
      event.preventDefault();
      jumpToChange(event.key.toLowerCase() === "n" ? 1 : -1);
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

<section class="file-diff-view" aria-label="Combined file diff">
  <header class="file-diff-header">
    <div class="file-diff-heading">
      <strong title={diff ? safeRefLabel(diff.path) : ""}>
        {#if diff?.old_path}{safeRefLabel(diff.old_path)} → {/if}{safeRefLabel(diff?.path ?? "File diff")}
      </strong>
      {#if diff}<span class="file-diff-stats">+{diff.stats.added} −{diff.stats.removed}</span>{/if}
      <button class="file-diff-close" aria-label="Close file diff" onclick={onClose}>×</button>
    </div>
    {#if diff && !diff.binary && !diff.too_large}
      <div class="file-diff-tools">
        <div class="file-diff-modes" role="group" aria-label="Diff view">
          <button class:active={mode === "full_file"} onclick={() => { mode = "full_file"; }}>Full file</button>
          <button class:active={mode === "changes_only"} onclick={() => { mode = "changes_only"; }}>Changes only</button>
        </div>
        <button onclick={() => { expandAll = !expandAll; expandedRegions = new Set(); }}>
          {expandAll ? "Collapse all" : "Expand all"}
        </button>
        <button class:active={wrap} aria-pressed={wrap} onclick={() => { wrap = !wrap; }}>Wrap</button>
        <button aria-label="Previous change" title="Previous change (p)" onclick={() => jumpToChange(-1)}>↑</button>
        <button aria-label="Next change" title="Next change (n)" onclick={() => jumpToChange(1)}>↓</button>
      </div>
    {/if}
    {#if diff && (diff.old_no_newline || diff.new_no_newline)}
      <div class="file-diff-newline">No newline at end of file{#if diff.old_no_newline && diff.new_no_newline} on both sides{:else if diff.old_no_newline} on the old side{:else} on the new side{/if}</div>
    {/if}
  </header>

  {#if loading}
    <div class="file-diff-message" role="status">Loading file diff…</div>
  {:else if error}
    <div class="file-diff-message" role="alert">{safeRefLabel(error)}</div>
  {:else if diff?.binary}
    <div class="file-diff-message">Binary files cannot be shown as text.</div>
  {:else if diff?.too_large}
    <div class="file-diff-message">
      <p>This file is too large to show by default ({diff.stats.added} added, {diff.stats.removed} removed).</p>
      <button onclick={onShowAnyway}>Show anyway</button>
    </div>
  {:else if diff}
    <div class="file-diff-viewport" class:wrap bind:this={viewport} onscroll={onScroll} aria-label="File contents">
      <div class="file-diff-spacer" style:height="{rows.length * ROW_HEIGHT}px">
        {#each visibleRows as row, offset (`${start + offset}:${row.kind === "line" ? row.index : row.key}`)}
          {@const rowIndex = start + offset}
          {#if row.kind === "collapsed"}
            <button class="file-diff-collapsed" style:top="{rowIndex * ROW_HEIGHT}px" style:height="{ROW_HEIGHT}px" onclick={() => toggleRegion(row.key)}>
              ⋯ {row.count} unchanged lines
            </button>
          {:else}
            <div class="file-diff-line" class:added={row.line.kind === "added"} class:removed={row.line.kind === "removed"} class:current-change={currentChange === row.index}
              style:top="{rowIndex * ROW_HEIGHT}px" style:height="{ROW_HEIGHT}px">
              <span class="file-diff-number">{row.line.old_no ?? ""}</span>
              <span class="file-diff-number">{row.line.new_no ?? ""}</span>
              <span class="file-diff-marker">{row.line.kind === "added" ? "+" : row.line.kind === "removed" ? "−" : " "}</span>
              <span class="file-diff-code">{safeRefLabel(row.line.text)}</span>
            </div>
          {/if}
        {/each}
      </div>
    </div>
  {/if}
</section>

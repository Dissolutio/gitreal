<script lang="ts">
  import type { KnownRepo } from "./lib/api";
  import type { RepoTab } from "./lib/tabs";

  interface Props {
    tabs: RepoTab[];
    activePath: string | null;
    knownRepos: KnownRepo[];
    onFocus: (path: string) => void;
    onClose: (path: string) => void;
    onOpenKnown: (path: string) => void;
    onOpenFolder: () => void;
    onCycle: (direction: 1 | -1) => void;
  }

  let {
    tabs,
    activePath,
    knownRepos,
    onFocus,
    onClose,
    onOpenKnown,
    onOpenFolder,
    onCycle,
  }: Props = $props();
  let pickerOpen = $state(false);
  let filter = $state("");
  const filteredRepos = $derived(knownRepos.filter(repo =>
    `${repo.name} ${repo.path}`.toLowerCase().includes(filter.toLowerCase()),
  ));

  function keydown(event: KeyboardEvent) {
    if (!event.ctrlKey || event.altKey || event.isComposing) return;
    if (event.key === "Tab") {
      event.preventDefault();
      onCycle(event.shiftKey ? -1 : 1);
    } else if (event.key.toLowerCase() === "w" && activePath) {
      event.preventDefault();
      onClose(activePath);
    } else if (event.key.toLowerCase() === "t") {
      event.preventDefault();
      pickerOpen = true;
    } else if (event.key === "Escape" && pickerOpen) {
      event.preventDefault();
      pickerOpen = false;
    }
  }

  function openFolder() {
    pickerOpen = false;
    filter = "";
    onOpenFolder();
  }

  function openKnown(path: string) {
    pickerOpen = false;
    filter = "";
    onOpenKnown(path);
  }
</script>

<svelte:window onkeydown={keydown} />

<div class="tabs-bar">
  <div class="repo-tabs" role="tablist" aria-label="Open repositories">
    {#each tabs as tab (tab.path)}
      <div class:active={activePath === tab.path} class:unavailable={!!tab.error} class="repo-tab-shell">
        <button
          class="repo-tab"
          role="tab"
          aria-selected={activePath === tab.path}
          title={tab.path}
          onclick={() => onFocus(tab.path)}
          onauxclick={(event) => {
            if (event.button === 1) {
              event.preventDefault();
              onClose(tab.path);
            }
          }}
        >
          <span class="tab-repo-icon" aria-hidden="true">⑂</span>
          <span class="tab-name">{tab.name}</span>
        </button>
        {#if tab.error}
          <span class="tab-error" title={tab.error.message}>Unavailable</span>
          <button class="tab-remove" aria-label={`Remove ${tab.name}`} onclick={() => onClose(tab.path)}>Remove</button>
        {:else}
          <button class="tab-close" aria-label={`Close ${tab.name}`} title="Close tab" onclick={() => onClose(tab.path)}>×</button>
        {/if}
      </div>
    {/each}
  </div>
  <button class="tab-add" aria-label="Open repository tab" title="Open repository (Ctrl+T)" onclick={() => { pickerOpen = true; }}>
    <span aria-hidden="true">+</span>
  </button>
  {#if pickerOpen}
    <div class="repo-picker-backdrop" role="presentation" onclick={(event) => {
      if (event.target === event.currentTarget) pickerOpen = false;
    }}>
      <dialog open class="repo-picker" aria-labelledby="repo-picker-title">
        <div class="repo-picker-heading">
          <div><span class="eyebrow">WORKSPACE</span><h2 id="repo-picker-title">Open a repository</h2></div>
          <button aria-label="Close picker" onclick={() => { pickerOpen = false; }}>×</button>
        </div>
        <label class="repo-picker-filter"><span aria-hidden="true">⌕</span>
          <input aria-label="Filter known repositories" placeholder="Filter repositories…" bind:value={filter} />
        </label>
        <div class="repo-picker-list">
          {#each filteredRepos as repo (repo.path)}
            <button class="repo-picker-item" title={repo.path} onclick={() => openKnown(repo.path)}>
              <span class="tab-repo-icon" aria-hidden="true">⑂</span>
              <span><strong>{repo.name}</strong><small>{repo.path}</small></span>
              {#if tabs.some(tab => tab.path === repo.path)}<span class="already-open">OPEN</span>{/if}
            </button>
          {:else}
            <p class="repo-picker-empty">{knownRepos.length ? "No matching repositories" : "No known repositories yet"}</p>
          {/each}
        </div>
        <button class="repo-picker-folder" onclick={openFolder}><span aria-hidden="true">▱</span> Open folder…</button>
      </dialog>
    </div>
  {/if}
</div>

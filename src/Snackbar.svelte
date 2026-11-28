<script lang="ts">
  import { snackbarStore, type JobAction, type SnackbarEntry } from "./lib/snackbar";
  import { safeRefLabel } from "./lib/display";
  import { updateStore, type UpdateSnackbarEntry } from "./lib/update";

  let {
    onAction,
    onUpdateAction,
  }: {
    onAction?: (action: JobAction, entry: SnackbarEntry) => void;
    onUpdateAction?: (entry: UpdateSnackbarEntry) => void;
  } = $props();
  let copied = $state<Record<string, boolean>>({});

  function formatBytes(bytes: number) {
    if (bytes < 1024) return `${bytes} B`;
    const units = ["KB", "MB", "GB"];
    let value = bytes / 1024;
    let unit = 0;
    while (value >= 1024 && unit < units.length - 1) {
      value /= 1024;
      unit++;
    }
    return `${value.toFixed(1)} ${units[unit]}`;
  }

  async function copy(entry: SnackbarEntry) {
    try {
      await navigator.clipboard.writeText(entry.stderr_tail);
      copied[entry.job_id] = true;
      setTimeout(() => { copied[entry.job_id] = false; }, 1200);
    } catch {
      copied[entry.job_id] = false;
    }
  }
</script>

<section class="snackbars" aria-label="Git operation notifications" aria-live="polite">
  {#if $updateStore}
    {@const update = $updateStore}
    <article class:failed={update.status === "error"} class="snackbar update-snackbar">
      {#if update.status === "downloading"}<span class="snackbar-spinner" aria-hidden="true"></span>{:else}<span class="snackbar-state" aria-hidden="true">{update.status === "error" ? "!" : "↻"}</span>{/if}
      <div class="snackbar-content">
        <strong>{update.version ? `Gitreal ${update.version} is available` : "Update check failed"}</strong>
        {#if update.status === "downloading"}
          <small>
            Downloading update
            {formatBytes(update.downloaded)}
            {#if update.total !== null} of {formatBytes(update.total)}{/if}
          </small>
          {#if update.total}<progress value={update.downloaded} max={update.total} aria-label="Update download progress"></progress>{/if}
        {:else if update.error}
          <small>{safeRefLabel(update.error)}</small>
        {:else if update.notes}
          <small>{safeRefLabel(update.notes)}</small>
        {/if}
        {#if update.version}
          <div class="snackbar-actions">
            <button disabled={update.status === "downloading"} onclick={() => onUpdateAction?.(update)}>
              {update.can_install ? "Update & restart" : "View release"}
            </button>
          </div>
        {/if}
      </div>
      <button class="snackbar-dismiss" aria-label="Dismiss update notification" onclick={() => updateStore.dismiss()}>×</button>
    </article>
  {/if}
  {#each $snackbarStore as toast (toast.job_id)}
    <article class:failed={toast.ok === false} class:success={toast.ok === true} class="snackbar">
      {#if toast.ok === null}<span class="snackbar-spinner" aria-hidden="true"></span>{:else}<span class="snackbar-state" aria-hidden="true">{toast.ok ? "✓" : "!"}</span>{/if}
      <div class="snackbar-content">
        <strong>{safeRefLabel(toast.message)}</strong>
        <small>{toast.repo_name} · {safeRefLabel(toast.label)}</small>
        {#if toast.ok === true && toast.action}
          <div class="snackbar-actions"><button onclick={() => onAction?.(toast.action!, toast)}>{safeRefLabel(toast.action.label)}</button></div>
        {/if}
        {#if toast.ok === false}
          <div class="snackbar-actions">
            {#if toast.action}<button onclick={() => onAction?.(toast.action!, toast)}>{safeRefLabel(toast.action.label)}</button>{/if}
            <button onclick={() => snackbarStore.toggleDetails(toast.job_id)}>{toast.details_open ? "Hide details" : "Details"}</button>
            <button onclick={() => copy(toast)}>{copied[toast.job_id] ? "Copied" : "Copy"}</button>
          </div>
          {#if toast.details_open}<pre>{safeRefLabel(toast.stderr_tail) || "No additional output."}</pre>{/if}
        {/if}
      </div>
      {#if toast.ok !== null}
        <button class="snackbar-dismiss" aria-label="Dismiss notification" onclick={() => snackbarStore.dismiss(toast.job_id)}>×</button>
      {/if}
    </article>
  {/each}
</section>

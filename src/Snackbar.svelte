<script lang="ts">
  import { snackbarStore, type SnackbarEntry } from "./lib/snackbar";

  let copied = $state<Record<string, boolean>>({});

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
  {#each $snackbarStore as toast (toast.job_id)}
    <article class:failed={toast.ok === false} class:success={toast.ok === true} class="snackbar">
      {#if toast.ok === null}<span class="snackbar-spinner" aria-hidden="true"></span>{:else}<span class="snackbar-state" aria-hidden="true">{toast.ok ? "✓" : "!"}</span>{/if}
      <div class="snackbar-content">
        <strong>{toast.message}</strong>
        <small>{toast.repo_name} · {toast.label}</small>
        {#if toast.ok === false}
          <div class="snackbar-actions">
            <button onclick={() => snackbarStore.toggleDetails(toast.job_id)}>{toast.details_open ? "Hide details" : "Details"}</button>
            <button onclick={() => copy(toast)}>{copied[toast.job_id] ? "Copied" : "Copy"}</button>
          </div>
          {#if toast.details_open}<pre>{toast.stderr_tail || "No additional output."}</pre>{/if}
        {/if}
      </div>
      {#if toast.ok !== null}
        <button class="snackbar-dismiss" aria-label="Dismiss notification" onclick={() => snackbarStore.dismiss(toast.job_id)}>×</button>
      {/if}
    </article>
  {/each}
</section>

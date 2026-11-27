export interface JobStartedEvent {
  job_id: string;
  repo_id: string;
  kind: string;
  label: string;
}

export interface JobFinishedEvent extends JobStartedEvent {
  ok: boolean;
  message: string;
  stderr_tail: string;
}

export interface SnackbarEntry extends JobStartedEvent {
  repo_name: string;
  ok: boolean | null;
  message: string;
  stderr_tail: string;
  details_open: boolean;
}

export function createSnackbarStore() {
  let entries: SnackbarEntry[] = [];
  const listeners = new Set<(value: SnackbarEntry[]) => void>();
  const timers = new Map<string, ReturnType<typeof setTimeout>>();

  function publish() {
    for (const listener of listeners) listener(entries);
  }

  function put(entry: SnackbarEntry, moveToTop: boolean) {
    const existing = entries.findIndex(item => item.job_id === entry.job_id);
    if (existing >= 0) {
      entries = entries.map((item, index) => index === existing ? entry : item);
      if (moveToTop) {
        entries = [entries[existing], ...entries.filter((_, index) => index !== existing)];
      }
    } else {
      entries = [entry, ...entries];
    }
    while (entries.length > 4) {
      const removed = entries.pop();
      if (removed) {
        clearTimeout(timers.get(removed.job_id));
        timers.delete(removed.job_id);
      }
    }
    publish();
  }

  function dismiss(jobId: string) {
    clearTimeout(timers.get(jobId));
    timers.delete(jobId);
    entries = entries.filter(entry => entry.job_id !== jobId);
    publish();
  }

  return {
    subscribe(listener: (value: SnackbarEntry[]) => void) {
      listeners.add(listener);
      listener(entries);
      return () => listeners.delete(listener);
    },
    jobStarted(event: JobStartedEvent, repoName: string) {
      clearTimeout(timers.get(event.job_id));
      timers.delete(event.job_id);
      put({
        ...event,
        repo_name: repoName,
        ok: null,
        message: event.label,
        stderr_tail: "",
        details_open: false,
      }, true);
    },
    jobFinished(event: JobFinishedEvent, repoName: string) {
      const previous = entries.find(entry => entry.job_id === event.job_id);
      clearTimeout(timers.get(event.job_id));
      timers.delete(event.job_id);
      put({
        ...event,
        repo_name: repoName,
        ok: event.ok,
        message: event.message,
        stderr_tail: event.stderr_tail,
        details_open: previous?.details_open ?? false,
      }, !previous);
      if (event.ok && entries.some(entry => entry.job_id === event.job_id)) {
        timers.set(event.job_id, setTimeout(() => dismiss(event.job_id), 4000));
      }
    },
    failed(event: JobFinishedEvent, repoName: string) {
      this.jobFinished(event, repoName);
    },
    dismiss,
    toggleDetails(jobId: string) {
      entries = entries.map(entry => entry.job_id === jobId
        ? { ...entry, details_open: !entry.details_open }
        : entry);
      publish();
    },
  };
}

export const snackbarStore = createSnackbarStore();

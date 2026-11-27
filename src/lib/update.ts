export interface UpdateInfo {
  version: string;
  notes: string | null;
  date: string | null;
  can_install: boolean;
}

export interface UpdateProgress {
  downloaded: number;
  total: number | null;
}

export interface UpdateSnackbarEntry extends UpdateInfo {
  status: "available" | "downloading" | "error";
  downloaded: number;
  total: number | null;
  error: string | null;
}

export function createUpdateStore() {
  let entry: UpdateSnackbarEntry | null = null;
  const seenVersions = new Set<string>();
  const listeners = new Set<(value: UpdateSnackbarEntry | null) => void>();

  function publish() {
    for (const listener of listeners) listener(entry);
  }

  return {
    subscribe(listener: (value: UpdateSnackbarEntry | null) => void) {
      listeners.add(listener);
      listener(entry);
      return () => listeners.delete(listener);
    },
    found(update: UpdateInfo) {
      if (seenVersions.has(update.version)) return false;
      seenVersions.add(update.version);
      entry = {
        ...update,
        status: "available",
        downloaded: 0,
        total: null,
        error: null,
      };
      publish();
      return true;
    },
    startInstall() {
      if (!entry) return;
      entry = { ...entry, status: "downloading", downloaded: 0, total: null, error: null };
      publish();
    },
    progress(progress: UpdateProgress) {
      if (!entry) return;
      entry = {
        ...entry,
        status: "downloading",
        downloaded: progress.downloaded,
        total: progress.total,
        error: null,
      };
      publish();
    },
    failed(message: string) {
      entry = entry
        ? { ...entry, status: "error", error: message }
        : {
            version: "",
            notes: null,
            date: null,
            can_install: false,
            status: "error",
            downloaded: 0,
            total: null,
            error: message,
          };
      publish();
    },
    dismiss() {
      entry = null;
      publish();
    },
  };
}

export const updateStore = createUpdateStore();

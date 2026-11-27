import { describe, expect, it } from "vitest";
import { createUpdateStore, type UpdateSnackbarEntry } from "./update";

const update = {
  version: "0.1.7",
  notes: "Fixes and improvements",
  date: "2026-11-27",
  can_install: true,
};

function snapshot(store: ReturnType<typeof createUpdateStore>) {
  const values: (UpdateSnackbarEntry | null)[] = [];
  store.subscribe(entry => { values.push(entry); })();
  return values.at(-1) ?? null;
}

describe("update snackbar state", () => {
  it("tracks an update through progress and error in one persistent entry", () => {
    const store = createUpdateStore();
    expect(store.found(update)).toBe(true);
    expect(snapshot(store)).toMatchObject({ version: update.version, status: "available" });
    store.progress({ downloaded: 512, total: 1024 });
    expect(snapshot(store)).toMatchObject({
      version: update.version,
      status: "downloading",
      downloaded: 512,
      total: 1024,
    });
    store.failed("Signature verification failed");
    expect(snapshot(store)).toMatchObject({
      version: update.version,
      status: "error",
      error: "Signature verification failed",
    });
  });

  it("shows a version only once per session, including after dismissal", () => {
    const store = createUpdateStore();
    expect(store.found(update)).toBe(true);
    store.dismiss();
    expect(store.found(update)).toBe(false);
    expect(snapshot(store)).toBeNull();
    expect(store.found({ ...update, version: "0.1.8" })).toBe(true);
    expect(snapshot(store)?.version).toBe("0.1.8");
  });

  it("keeps check failures in a persistent update entry", () => {
    const store = createUpdateStore();
    store.failed("Network unavailable");
    expect(snapshot(store)).toMatchObject({ status: "error", error: "Network unavailable" });
  });
});

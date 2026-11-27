import { afterEach, describe, expect, it, vi } from "vitest";
import { createSnackbarStore, type JobFinishedEvent, type JobStartedEvent, type SnackbarEntry } from "./snackbar";

function events(jobId: string): { started: JobStartedEvent; finished: JobFinishedEvent } {
  const started = { job_id: jobId, repo_id: "repo-1", kind: "fetch", label: `Fetch ${jobId}` };
  return {
    started,
    finished: { ...started, ok: true, message: `Fetched ${jobId}`, stderr_tail: "" },
  };
}

function snapshot(store: ReturnType<typeof createSnackbarStore>) {
  let value: SnackbarEntry[] = [];
  store.subscribe(entries => { value = entries; })();
  return value;
}

describe("snackbar state", () => {
  afterEach(() => vi.useRealTimers());

  it("keeps newest notifications first and caps the stack at four", () => {
    const store = createSnackbarStore();
    for (let index = 1; index <= 5; index++) {
      store.jobStarted(events(String(index)).started, "repo");
    }
    expect(snapshot(store).map(entry => entry.job_id)).toEqual(["5", "4", "3", "2"]);
  });

  it("auto-dismisses successful results after four seconds", () => {
    vi.useFakeTimers();
    const store = createSnackbarStore();
    const { started, finished } = events("success");
    store.jobStarted(started, "repo");
    store.jobFinished(finished, "repo");
    vi.advanceTimersByTime(3999);
    expect(snapshot(store)).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(snapshot(store)).toHaveLength(0);
  });

  it("keeps failed results until explicitly dismissed", () => {
    vi.useFakeTimers();
    const store = createSnackbarStore();
    const { started, finished } = events("failure");
    store.jobStarted(started, "repo");
    store.jobFinished({ ...finished, ok: false, message: "Failed", stderr_tail: "fatal: denied" }, "repo");
    vi.advanceTimersByTime(60_000);
    expect(snapshot(store)[0].message).toBe("Failed");
    expect(snapshot(store)[0].stderr_tail).toBe("fatal: denied");
    store.dismiss("failure");
    expect(snapshot(store)).toHaveLength(0);
  });

  it("updates the running toast by job id when its result arrives", () => {
    const store = createSnackbarStore();
    const { started, finished } = events("same-job");
    store.jobStarted(started, "repo");
    expect(snapshot(store)[0].ok).toBeNull();
    store.jobFinished({ ...finished, ok: false, message: "Push rejected", stderr_tail: "details" }, "repo");
    expect(snapshot(store)).toHaveLength(1);
    expect(snapshot(store)[0]).toMatchObject({
      job_id: "same-job",
      ok: false,
      message: "Push rejected",
      stderr_tail: "details",
    });
  });
});

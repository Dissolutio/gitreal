import { describe, expect, it } from "vitest";
import { editLaunch, launchClick, resetLaunch } from "./launch";

const detected = { command: "pnpm tauri dev", reason: "pnpm-lock.yaml + package.json script 'tauri'" };

describe("launch popover state", () => {
  it("opens setup for the first launch and launches a saved command immediately", () => {
    expect(launchClick(undefined, detected)).toEqual({
      kind: "configure",
      popover: { mode: "setup", ...detected },
    });
    expect(launchClick("npm run dev", detected)).toEqual({
      kind: "launch",
      command: "npm run dev",
    });
  });

  it("opens the saved command in edit mode", () => {
    expect(editLaunch("npm run dev", detected)).toEqual({
      mode: "edit",
      command: "npm run dev",
      reason: detected.reason,
    });
  });

  it("resets the field to the detected suggestion", () => {
    expect(resetLaunch(detected)).toEqual({ mode: "setup", ...detected });
  });
});

export interface LaunchSuggestion {
  command: string;
  reason: string;
}

export interface LaunchPopover {
  mode: "setup" | "edit";
  command: string;
  reason: string;
}

export type LaunchClick =
  | { kind: "launch"; command: string }
  | { kind: "configure"; popover: LaunchPopover };

const noSuggestion = "No command detected. Enter a command to launch this repository.";

export function launchClick(savedCommand: string | undefined, suggestion: LaunchSuggestion | null): LaunchClick {
  if (savedCommand) return { kind: "launch", command: savedCommand };
  return {
    kind: "configure",
    popover: {
      mode: "setup",
      command: suggestion?.command ?? "",
      reason: suggestion?.reason ?? noSuggestion,
    },
  };
}

export function editLaunch(savedCommand: string | undefined, suggestion: LaunchSuggestion | null): LaunchPopover {
  return {
    mode: "edit",
    command: savedCommand ?? suggestion?.command ?? "",
    reason: suggestion?.reason ?? noSuggestion,
  };
}

export function resetLaunch(suggestion: LaunchSuggestion | null): LaunchPopover {
  return {
    mode: "setup",
    command: suggestion?.command ?? "",
    reason: suggestion?.reason ?? noSuggestion,
  };
}

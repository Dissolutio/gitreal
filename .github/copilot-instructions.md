# Gitreal contributor instructions

## Architecture and safety

- Tauri 2 Rust backend; Svelte 5 + TypeScript + Vite frontend; use pnpm.
- All git reads go through `gix`. Never add git2/libgit2.
- Git on disk is the source of truth. Never update the UI optimistically.
- Future writes must use the `git` CLI through a per-repository serialized job
  queue. After any mutation, re-read state and notify the UI through events.
- Force pushes must always use `--force-with-lease`, never plain `--force`.
- This MVP is read-only. Do not add mutations, GitHub API calls, tabs, file
  watching, or tray behavior unless the issue explicitly requests them.

## Module boundaries

- `src-tauri/src/repo/reader.rs`: repository discovery, refs, topological history,
  and commit details, using gix.
- `src-tauri/src/repo/graph.rs`: pure lane assignment; no IO or Tauri dependencies.
- `src-tauri/src/commands.rs`: typed, serializable IPC and repository handles;
  offload blocking repository work rather than blocking the UI/runtime.
- Frontend: present backend state, virtualize history, and render only visible
  graph rows on canvas.

## Performance

- Prioritize low idle RAM and CPU: no polling loops or continuous animation.
- No work in inactive tabs beyond ref watching once tabs/watchers are implemented.
- Virtualize all long lists. Page history and avoid unbounded frontend caches.
- Keep expensive git operations off the main thread.

## Changes and checks

- Keep PRs small and scoped to the issue. Add tests for all Rust logic, including
  edge cases for repository reads and graph layout.
- Before finishing, run `cargo fmt --check`,
  `cargo clippy --locked --all-targets -- -D warnings`, and `cargo test --locked`
  from `src-tauri`; run `pnpm check`, `pnpm test`, and `pnpm build` from the root.
- Keep both dependency lockfiles committed. Do not commit build artifacts.

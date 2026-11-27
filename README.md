# Gitreal

A lightweight, always-open Linux desktop git browser: a minimal GitKraken
alternative built with Tauri 2, Svelte 5, TypeScript, Vite, and gix. Target:
Pop!_OS 24.04 with COSMIC/Wayland, with low idle RAM and CPU as a priority.

## Repository explorer and local operations

Open a repository with the native folder picker, browse local branches, remote
branches and tags, scroll a virtualized commit graph, and select a commit for its
message, author, date, parents and changed paths. Git reads use gix, not libgit2.
Fetch, fast-forward-only pull, push, and lease-protected force push run through a
per-repository serialized Git CLI queue. The UI refreshes from confirmed Git
state after each operation; it never updates optimistically. A metadata-only
watcher keeps references and history current after external Git activity; it
never watches the working tree. It watches Git metadata in the resolved worktree
and common directories, debounces events for 150 ms, and ignores lock and
transient files.

History is cached as a compact, topologically ordered snapshot (lane assignments
plus lane-occupancy checkpoints; edges are reconstructed per page) and atomically
rebuilt when references change; subsequent pages share the same generation. The
frontend keeps only a bounded page cache and reloads visible pages after refresh.
Changed paths are compared with the first parent (merges included); initial and
shallow-boundary commits are compared with an empty tree. Renames appear as
added/deleted paths rather than requiring expensive similarity detection.

## Development on Pop!_OS 24.04

Install Git, Rust stable (including rustfmt and clippy), Node.js LTS, and the pnpm
version in `package.json`:

```sh
sudo apt-get update
sudo apt-get install -y build-essential curl file git pkg-config \
  libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev
rustup default stable
rustup component add rustfmt clippy
npm install -g pnpm@10.34.9
pnpm install --frozen-lockfile
pnpm tauri dev
```

Install Rust with [rustup](https://rustup.rs/) and Node.js LTS with your preferred
version manager if they are not already available. Run the desktop app in your
normal graphical session; `pnpm dev` alone serves the frontend but does not
provide native repository access.

### Always-on setup on Pop!_OS (COSMIC)

Install the `.deb` package with `sudo apt install ./gitreal_*.deb` for the
`gitreal` terminal command and autostart entry. Alternatively, make the AppImage
executable and launch it; for CLI commands, ensure its executable is available
on `PATH` as `gitreal`. The **Start at login** entry uses the running binary's
absolute path, so AppImage autostart does not depend on `PATH`. Gitreal stays
available in the background when its window is closed; use the tray menu's
**Start at login** checkbox to enable or disable autostart.

To toggle Gitreal with a COSMIC keyboard shortcut:

1. Open **Settings → Keyboard → Custom shortcuts**.
2. Add a shortcut named **Toggle Gitreal**.
3. Set its command to `gitreal --toggle`.
4. Choose the key combination you want and save it.

Wayland applications cannot register global hotkeys themselves, so the shortcut
is configured in COSMIC rather than inside Gitreal.

From VSCode, a task or keybinding can open the current workspace with
`gitreal open ${workspaceFolder}`.

### Install & updates

Download the `.AppImage` from [Gitreal Releases](https://github.com/Dissolutio/gitreal/releases/latest), then make it executable and place it in `~/Applications/`:

```sh
mkdir -p ~/Applications
chmod +x ~/Downloads/Gitreal_*.AppImage
mv ~/Downloads/Gitreal_*.AppImage ~/Applications/
```

Start the AppImage from that location. The **Start at login** tray entry uses
the running AppImage path from `$APPIMAGE`, so keep the file in its chosen
location. Gitreal checks for updates about 30 seconds after startup and every
six hours; use **Check for updates** in the tray menu to check manually. Update
checks can be disabled from the tray menu. AppImages offer **Update & restart**;
`.deb` installs cannot update themselves and instead open the release page.

To enable signed updates for the owner, generate a signing key once:

```sh
pnpm tauri signer generate -w ~/.tauri/gitreal.key
```

Add the generated private key and password as repository secrets named
`TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. Copy the
generated public key into `src-tauri/tauri.conf.json`, replacing
`REPLACE_WITH_OWNER_PUBKEY`. Never commit the private key or password. Every
push to `main` then publishes a signed release with an AppImage, `.deb`, and
updater metadata.

### Checks

```sh
pnpm check
pnpm test
pnpm build
cd src-tauri
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Rust integration tests create temporary repositories using the Git CLI; the
application reads repositories through gix and uses the CLI only for queued
Git operations. Graph tests cover
linear, branched/merged, octopus, multiple-root and criss-cross histories.

### Layout

- `src/`: Svelte UI, virtualized history and visible-row canvas rendering.
- `src-tauri/src/repo/reader.rs`: gix-backed refs, history and commit details.
- `src-tauri/src/repo/graph.rs`: pure graph lane assignment.
- `src-tauri/src/commands.rs`: typed Tauri IPC and repository handles.

### Native API

- `open_repo(path)` returns `{ repo_id, path, name }`; subsequent commands use
  the opaque handle. `close_repo(repo_id)` releases it when switching repos.
- `get_refs(repo_id)` returns local/remote/tag/HEAD entries. Annotated tags are
  peeled; unborn HEAD and non-commit tags have a null target. Local branches
  include `upstream` (for example, `origin/main`), `ahead`, and `behind`; absent
  tracking data is null, a deleted upstream sets `upstream_gone`, and counts
  stop at 10,000 commits per side with `truncated` set when the bound is hit.
  The response also includes `operation` (null or merge/rebase/cherry-pick/
  revert/bisect state), `conflicted_files`, and the repository generation.
  Merge, cherry-pick, revert, and bisect markers are read from their Git metadata
  files; rebase state is read from `rebase-merge/` or `rebase-apply/`.
- `get_graph_page(repo_id, offset, limit)` returns
  `{ rows, total, max_lane, generation }`, with page sizes 1–500. `max_lane` is
  the widest lane index in the snapshot. Each row has commit metadata, labels, a
  lane and parent edges. Edges connect this row's center to the next row's
  center; pass-through edges preserve connections across intervening rows and
  page boundaries.
- `get_commit_detail(repo_id, commit_id)` accepts a full hexadecimal object ID
  in the snapshot, not revision expressions. It returns the message, author,
  Unix date, original parents and changed-path objects.
- `fetch(repo_id, remote?)`, `pull(repo_id)`, `push(repo_id, branch)`, and
  `force_push(repo_id, branch)` enqueue an operation and immediately return a
  job ID. `job-started` and `job-finished` events report progress and results.
  Pull uses `--ff-only`; force push always uses `--force-with-lease` against the
  remote-tracking object ID currently known to Gitreal. `get_push_remote`
  resolves the configured push target for the force-push confirmation.
- Errors serialize as `{ code, message }`. Blocking git work runs off the
  async runtime. At most 16 repository handles may remain open.

Keep the Rust lockfile: the pinned Tauri release uses matching runtime and macro
versions, which should be updated together.

Git on disk remains the source of truth. Queued operations re-read repository
state after completion and emit `repo-changed` only when the snapshot advances.
No optimistic updates; force pushes must use `--force-with-lease`.

## Roadmap

- Tabs and configuration persistence.
- Merge/rebase via drag and context menu, with a conflict banner.

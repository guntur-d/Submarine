# Fork notes — guntur-d/Submarine

Fork of [SinaXhpm/Submarine](https://github.com/SinaXhpm/Submarine) at v0.3.8.
Upstream remote: `upstream` → `SinaXhpm/Submarine`. This fork's `main` tracks
`origin` → `guntur-d/Submarine`.

Feature PRs sent upstream contain **only `src/` changes**. Fork-only files
(`src-tauri/tauri.conf.json` identity, `FORK.md`, `rules.md`) never go into
upstream PRs — see `rules.md` for the push cycle.

## Install identity (fork-only)

- `productName`: **Submarine Dev**, window title **Submarine Dev**
- `identifier`: `dev.guntur.submarine`
- The official release and this fork install side-by-side (separate folder,
  Start-menu entry, and app-data dir). Same version number, no clobbering.

## Windows build notes

- Prereqs: Node 22 + Rust stable + VS 2022 Build Tools + WebView2
  (`npx tauri info` should show all green).
- `npm run tauri dev` — daily loop, hot-reloads frontend edits.
- `npx tauri build --debug` — installable `.msi`/setup `.exe` for testing.
- Release (`npx tauri build`) links hundreds of tiny build-script exes;
  real-time AV (Avast here) can lock them and fail the link with `LNK1104`
  / `Access denied`. Add an AV exception for
  `src-tauri\target\` before release builds.
- `cargo` must be on PATH in the build shell:
  `$env:Path += ";$env:USERPROFILE\.cargo\bin"`.

## Features (all in `src/`, upstream-eligible)

### Rename session tabs
Right-click a session tab → **Rename tab** (or double-click the tab, or
press **F2**). Inline editor in the menu: Enter commits, empty text resets
to the server name, Esc cancels. Display-only — `serverName` stays the
backend identity. Names clear when the session closes.

### Rename terminal tabs
Right-click a terminal tab (`1`, `2`, container names…) → **Rename tab**
(same double-click / F2 gestures; F2 renames the active terminal when the
focus is inside a terminal). Renames flow into Wall tiles, the Wall picker
and split panes automatically.

### Copy file name / path (SFTP)
File-browser right-click menu: **Copy name** + **Copy path** for one file,
**Copy N paths** (newline-separated) for multi-selections.

### Keyboard paste in terminal
**Ctrl+Shift+V** and **Ctrl+V** paste into the focused terminal (same path
as right-click paste, incl. CRLF normalization). Plain **Ctrl+C is
deliberately untouched** — it must keep sending SIGINT; copying is already
covered by select-to-copy.

### Session restore
On every layout change the app snapshots open servers, their terminal tabs
(custom titles, container tabs) and the focused tab to per-profile local
storage (`submarine-open-sessions.v1.<profile>`). Unlock reopens everything
and reconnects each session. Toggle: **Settings → Sessions → Restore
previous sessions** (defaults on; autostart servers open regardless).
Limits: quick-connect sessions can't be restored (in-memory credentials);
split-view tiling and Wall pins reset; tab titles are plaintext in local
storage. See `src/sessionSnapshot.ts` (pure build/parse) for the format.

## Code map for the above

- `src/DesktopApp.tsx` — tab strip, tab menu + rename, F2 router,
  snapshot save effect, restore staging on unlock.
- `src/components/SessionView.tsx` — terminal tabs, rename menu,
  F2-event handling, relaunch seeds (`initialTerminals`).
- `src/components/TerminalView.tsx` — keyboard paste.
- `src/components/FilePanel.tsx` — copy name/path menu items.
- `src/components/SettingsPanel.tsx` — Sessions toggle.
- `src/sessionSnapshot.ts` — snapshot build/parse (pure, boundary-validated).
- `src/ui/renameInput.tsx` — shared rename editor.

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

## Upstream batch (all in `src/`, upstream-eligible)

Staged for one upstream PR (see `PR-DRAFT.md` Items 1–4, 8–9 — verified:
typecheck ✅, vite build ✅, `tauri dev` boots ✅, WS smoke green ✅):

- **Plain-browser fallback** (`src/App.tsx`, `src/util/platform.ts`,
  `src/DesktopApp.tsx`): opening Vite dev without Tauri renders a static
  "runs in Tauri" card instead of a blank screen; `getCurrentWindow()`
  moved off module scope. `src/` diff is import-lines only otherwise.
- **Frontend IPC boundary** (`src/ipc.ts` new + 22 files repointed,
  import lines only): `invoke`/`listen` pass-throughs mirroring
  `@tauri-apps/api` exactly — the swap point for the gateway transport
  below. Zero behavior change under Tauri.
- **Gateway request/response + first events** (`src/ipc.ts` WS transport,
  `src/gatewayWindow.ts` no-op window stub, `App.tsx` gateway mount,
  title-bar controls hidden in gateway mode).
- **Gateway vault-lock + browser file transfer** (`src/DesktopApp.tsx`
  `vault-locked` listener → silent reset to picker; `src/fs/gatewayFiles.ts`
  + SftpWorkspace Upload button + FilePanel HTTP download branch).
- **Stale-token UX** (`src/App.tsx` mount probe + "token rejected" card
  with paste-a-token form; `src/ipc.ts` `forgetGateway`/`checkGateway`).
- **Terminal copy hardening** (`src/components/TerminalView.tsx`):
  `Ctrl+Shift+C` copies; select-to-copy skips unchanged (mouse-tracking
  TUIs) and whitespace-only selections instead of lying; `OSC 52`
  clipboard bridge so TUI-internal copies reach the OS clipboard; debug
  logging behind `localStorage submarine-term-debug=1`.

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

### Keyboard copy/paste in terminal
**Ctrl+Shift+C** copies the current selection and **Ctrl+Shift+V** /
**Ctrl+V** paste into the focused terminal (same path as right-click paste,
incl. CRLF normalization). Plain **Ctrl+C is deliberately untouched** — it
must keep sending SIGINT; copying is covered by select-to-copy and the Shift
variant.

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

## Gateway mode (fork epic track, `src-tauri/` — NOT for upstream)

`submarine serve` runs the backend headless and serves the built UI over
HTTP + a WebSocket `invoke` bridge, so any browser becomes a client
(tracks upstream issue #270; implementation stays fork-side per the
`src/`-only upstream rule above).

- Run: `.\src-tauri\target\debug\submarine.exe serve --frontend-dir .\dist`
  (after `npm run build`) → prints `http://127.0.0.1:17531/?token=…`.
  Flags: `--port`, `--bind` (+ `--allow-remote` required off-loopback),
  `--data-dir`, `--frontend-dir`, `--token` / `SUBMARINE_SERVE_TOKEN`,
  `--tls-cert/--tls-key`, `--lock-after` (idle auto-lock minutes, default 30).
- Poison-pill notes: each boot mints a new token (a pasted/bookmarked URL
  dies on restart — use the fresh console URL or paste the new token);
  `/api/*` needs the token, the static bundle is public; browser tabs need
  a hard refresh (Ctrl+Shift+R) after each `npm run build`.
- Debug: `localStorage.setItem('submarine-term-debug','1')` in the browser
  console + reload → `[term …]` logs for copy/OSC 52 diagnosis.

## Code map for the gateway

- `src-tauri/src/serve.rs` — HTTP static + token gate, WS protocol
  (`{id,cmd,args}` ↔ `{id,ok,result/error}`, `{type:sub/unsub}` +
  `{event,payload}` fan-out), `POST /api/files/upload` (multipart→temp→
  SFTP→cleanup) + `GET /api/files/download` (temp→stream→cleanup),
  `GET /api/ping`, bind consent, in-process TLS, idle auto-lock.
- `src-tauri/src/emit.rs` — `Emit` enum (`AppHandle` | broadcast sender),
  the seam every ported command funnels through.
- `src-tauri/src/lib.rs` — `*_inner` splits (body byte-identical, thin
  `#[tauri::command]` shells kept): full vault CRUD, tunnels, terminal,
  SFTP, mirrors, monitors, docker, SSH probes, auth continuations.
  Explicit `CLOUD_ONLY` (cloud/share/identity) and `DESKTOP_ONLY`
  (native dialogs, server-local paths) errors for the rest.
- `src/ipc.ts` — WS transport, gateway detection, `forgetGateway` /
  `checkGateway` / `rememberGatewayToken`, pending-invoke map.
- `src/gatewayWindow.ts` — `Pick<Window, 11 methods>` no-op stub.
- `src/fs/gatewayFiles.ts` — browser upload/download bridge.
- Smoke tests (throwaway, in `%TEMP%\opencode\`): `serve-smoke.py`
  (WS dispatch, fan-out, auth, files, tunnels, CRUD fail-closed),
  `serve-smoke-phase4.py` (bind/TLS/lock), `assetcheck.py`, `cmp*.py`,
  `dedup.py`, `fixends.py`, `sigs.py`, `dupcheck.py`, `checkends.py`.

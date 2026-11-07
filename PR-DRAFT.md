# Upstream PR draft — batch staging (FORK-ONLY, never in upstream diff)

> Per `rules.md` §5–6: upstream PRs branch off `upstream/main` and contain
> **only `src/` changes**. This file, `FORK.md`, `rules.md`, and
> `src-tauri/tauri.conf.json` identity must be excluded from the PR diff —
> verify with `git diff --stat` before pushing.

## Target

- Repo: `SinaXhpm/Submarine`, base `main`, head `guntur-d:<branch>`
- Checked 2026-10-30: no duplicate — 3 open PRs (#251 website, #200 README,
  #44 Tailcat), 6 open issues, none about browser/blank-screen fallback.

## Item 1 (done, verified) — plain-browser fallback instead of blank screen

**Problem:** opening the Vite dev server (`npm run dev`) in a plain browser
rendered a blank screen with `Uncaught TypeError: Cannot read properties of
undefined (reading 'metadata')` from `getCurrentWindow()` in `DesktopApp.tsx`.

**Change (`src/` only, +32/−4):**
- `src/App.tsx` — single `IS_TAURI` boundary; non-Tauri renders a static
  "Submarine runs in Tauri, use `npm run tauri dev`" card (no providers,
  effects, nothing to throw).
- `src/DesktopApp.tsx` — `getCurrentWindow()` moved from module scope into
  the component via `useState(() => getCurrentWindow())`; all 17
  `appWindow.*` uses untouched, still non-null.
- `src/util/platform.ts` — comment-only; reuses canonical `IS_TAURI`.

**Verified:** `npm run typecheck` ✅, `npm run build` ✅,
`npm run tauri dev` ✅ (app launched, SSH handshake succeeded).

## Item 2 (done, verified) — frontend IPC boundary (`serve` phase 1)

**Problem:** all 147 Tauri call sites imported `invoke`/`listen` directly
from `@tauri-apps/api`, so swapping the transport for gateway mode would
have meant touching every file. Blocks upstream #270.

**Change (new `src/ipc.ts` + 22 files, import lines only, zero logic change):**
- `src/ipc.ts` (new) — `invoke`/`listen` pass-throughs with signatures
  mirroring `@tauri-apps/api/{core,event}` exactly, plus type re-exports.
  The documented swap point for the WebSocket transport.
- 22 files repointed to `./ipc` / `../ipc`; dual imports collapsed to one
  line. `window` imports intentionally untouched (Tauri-only, guarded by the
  App.tsx boundary from Item 1).
- `useTauriListen` → `useIpcListen`, file included
  (`src/hooks/useTauriListen.ts` → `src/hooks/useIpcListen.ts`): was dead
  code — zero call sites — so the whole rename is a `git mv` + one line,
  no churn.

**Verified:** `npm run typecheck` ✅, `npm run build` ✅,
`npm run tauri dev` ✅ (Vite ready, cargo built, `submarine.exe` running).

## Item 3 (done, verified) — gateway request/response + first events (2b)

**Problem:** the browser had a transport with no commands behind it, and the
backend had no way to push events to a browser — both halves of #270's
loop were missing.

**Change (backend `src-tauri/`, all mechanical splits, bodies untouched):**
- New `src-tauri/src/emit.rs` — `Emit` enum (`AppHandle` | broadcast
  sender) with identical `emit(&str, payload)` shape; 2 unit tests.
- `disconnect_session` → `disconnect_session_inner` (+ thin shell). Chosen
  as first emitter deliberately: unconditional `session-disconnected-{id}`
  event, provable headless with no vault/session.
- Read-model ports (same 5-line pattern): `get_notes`, `get_ssh_keys`,
  `get_commands`, `get_folders`, `get_credentials`, plus `select_profile`,
  `check_db_exists`, `setup_master_db` (+ its vault-open flow — signature-
  only seam, body untouched) so the browser can unlock for real.
- `serve.rs`: broadcast bus (512), `{type:sub/unsub}` + `{event,payload}`
  protocol with sub-ack (closes the listen registration race), per-
  connection filtered fan-out, 12-command dispatch with `MISSING_ARG` /
  `UNKNOWN_COMMAND` structure. `initiate_connection` + tunnel worker tree
  explicitly deferred to 2c (1116-line driver, own review loop).

**Change (frontend `src/`, transport only, zero Tauri-path change):**
- `src/ipc.ts` — gateway detection (`?gateway=`/`?token=`, same-origin
  default, sessionStorage persist) + WS transport (id-mapped invoke,
  multiplexed listen with sub-ack, unsub on last listener, reconnect with
  resub, binary-arg guard).
- `src/gatewayWindow.ts` (new) — `Pick<Window, …11 methods>` no-op stub
  backed by the viewport; compiler enforces coverage of every `appWindow`
  call site.
- `App.tsx` mounts the app when gateway configured; title-bar window
  controls hidden in gateway mode.

**Verified:** `cargo check`/`build` clean (only pre-existing warnings),
12-assertion WS smoke ✅ (incl. real sub→event delivery, unlock-fails-
closed, Bearer parity), `npm run typecheck` ✅, `vite build` ✅,
desktop `tauri dev` boots on the new backend ✅.

## Item 4 (done, verified) — gateway vault-lock + browser file transfer (src/)

**Problem:** two gateway UI gaps — (1) the server broadcasts
`vault-locked` on idle timeout but no UI listened, so the browser kept
showing an unlocked vault with a dead backend; (2) the file endpoints
existed but no frontend called them (no `<input type=file>` anywhere,
local pane needs server-local paths).

**Change (`src/` only):**
- `src/DesktopApp.tsx` — `silentLock` (no confirm: backend already tore
  down) + `listen("vault-locked")` effect resetting to the picker.
- `src/ipc.ts` — new `gatewayHttpBase()` (WS URL → HTTP base).
- `src/fs/gatewayFiles.ts` (new) — `uploadBrowserFiles` (multipart POST
  per file) + `downloadRemoteFile` (GET → blob → anchor download).
- `src/components/SftpWorkspace.tsx` — gateway forces Remote tab, hides
  the Local tab, adds an Upload button (hidden file input) posting to the
  current remote dir then refreshing.
- `src/components/FilePanel.tsx` — gateway download branch: files stream
  via HTTP, folders get a clear "not supported in browser yet" message
  instead of a broken picker.

**Verified:** `npm run typecheck` ✅, `vite build` ✅ (1451 modules),
desktop `tauri dev` boots ✅. WS smoke still green (no UI regressions).

## Item 5 (done, verified) — TUI copy: no more false "Copied"

**Problem:** select-to-copy inside a TUI (opencode) toasted "Copied" but
paste yielded nothing. Verified against the xterm source that extraction
is coordinate-correct on both buffers (public `getLine` indexes the same
buffer-absolute space the selection model produces; the translate
primitive is literally the reference impl's) — so the copied bytes were
real but whitespace-only (blank TUI padding rows yield truthy `"\n"`s).

**Change (`src/components/TerminalView.tsx`, 1 line):** treat
whitespace-only selections as empty — no clipboard write, no toast.
Worst case is now silence instead of a lie; real selections copy exactly
as before, via the same path `Ctrl+Shift+C` uses.

**Verified:** `npm run build` ✅ (typecheck + vite). Not unit-provable
headless (needs a live alt-buffer + clipboard); confirm in the browser:
select real TUI text → "Copied" + pastes; select blank padding → silence,
previous clipboard content preserved.

## Item 6 (done, verified) — select-to-copy ignores app-consumed drags

**Problem:** in mouse-tracking TUIs (opencode, zellij) the *app* consumes
the drag and shows its own "Copied" UI — xterm creates no local selection.
Our mouseup handler then re-copied whatever stale selection predated the
TUI (or whitespace) with a lying "Copied" toast. The earlier
whitespace-only guard couldn't catch stale *non*-blank text.

**Change (`src/components/TerminalView.tsx`, same handler cluster):**
snapshot the local selection on mousedown; on mouseup, skip when the text
is unchanged (no fresh local selection was produced). Explicit
`Ctrl+Shift+C` still copies unconditionally — implicit paths must prove
freshness, explicit user action trusts the visible selection.

**Verified:** `npm run build` ✅ (typecheck + vite). Needs a live
mouse-tracking TUI to confirm end-to-end: drag in opencode → app's own
copy UI only, no toast from us; drag on the `$` prompt → "Copied" as
before; plain clicks stay silent.

## Item 7 (done, verified) — terminal copy diagnostics + OSC 52 observer

**Problem:** in mouse-tracking TUIs the app consumes the drag, so there
was no way to tell from the outside where a "copied but unpasteable"
report comes from — our side, or the TUI's own copy path (typically
`OSC 52`, which xterm.js drops on the floor).

**Change (`src/components/TerminalView.tsx`, diagnostics only):**
flag-gated (`localStorage submarine-term-debug=1`) `[term <id>]` console
logs at every copy decision point (mousedown snapshot, mouseup
changed/unchanged + buffer type, copy outcome with char count + preview,
blank-skip) plus an `OSC 52` observer that logs target + payload size
(never content) and returns false so behavior is unchanged. Disposed with
the other terminal disposables.

**Verified:** `npm run build` ✅. Next step depends on what the logs show:
if `osc52 received` lines appear on TUI copy, the real fix is an OSC 52
→ `navigator.clipboard` bridge; if not, opencode copies internally and
the answer is documenting its own paste key.

## Item 8 (done, verified) — paste-a-token on the hint card

**Problem:** the token is scrubbed from the address bar on load, so the
URL copied to another browser carries no token — which lands on the
"runs in Tauri" card with no way forward.

**Change (`src/ipc.ts`, `src/App.tsx`):** `rememberGatewayToken()` (same
`sameOriginWsUrl()` derivation as the `?token=` bootstrap — extracted to
one shared helper) + a password-style token form on the hint card. A bad
paste degrades gracefully: mount → WS fails → probe 401s → back to the
"token rejected" card.

**Verified:** `npm run build` ✅ (typecheck + vite).

## Item 9 (done, verified) — OSC 52 clipboard bridge (TUI copy actually works)

**Problem:** mouse-tracking TUIs consume the drag, so xterm never makes a
local selection — proven by debug logs (`mouseup skip: unchanged`,
`buf=alternate`). opencode announces its own copy via `OSC 52 ; c ;
base64` (420 payload chars observed), which xterm.js drops on the floor.

**Change (`src/components/TerminalView.tsx`):** the OSC 52 observer became
a bridge — decode `Pc=c` via the same `writeClipboard` path as
select-to-copy (+ "Copied" toast). Non-`c` targets, bad base64, and empty
payloads fall through silently; clipboard denials (background tab) never
toast. `localStorage submarine-term-debug=1` logging stays for future
diagnosis.

**Verified:** `npm run build` ✅. Needs the live TUI to confirm end to
end: copy in opencode → "Copied" toast → paste works outside.

## Item 10 (done, verified) — gateway process lifecycle docs

**Problem:** starting `serve` was documented, but listing running
instances and stopping them wasn't anywhere.

**Change (`FORK.md` only):** "Managing the gateway process" section —
one-instance-per-port, check commands (Windows/Linux/macOS), graceful
Ctrl+C vs `taskkill`/`pkill` (verified the vault write path is atomic:
tmp → fsync → rename in `save_vault_blocking`, so a force-kill loses at
most the unflushed in-memory tail), background-run pointers, stable-token
advice, and why there is deliberately no remote `stop` endpoint.

**Verified:** docs-only; save path confirmed in code, no build needed.

## Item 11 (staging — add while testing)

<!-- Append each finding as its own item: problem / change / verification. -->

- [ ] _pending — fill in while testing the app_

## Parked epics (NOT in this batch — separate upstream issue/PR each)

### Epic A (chosen): `submarine serve` — remote-capable gateway mode

**Goal:** run the Rust backend on a machine (local or remote/VPS), open the
full UI from any client browser. Same SSH/SFTP/tunnels/Docker, zero
reimplementation of backend logic.

**Sketch (to refine before any code):**
- `src-tauri`: new serve mode (e.g. `submarine serve --port --bind`) —
  HTTP server serving the built frontend (`dist/`) + WebSocket bridge that
  mirrors the Tauri `invoke`/event surface.
- `src/`: IPC abstraction so `invoke`/`listen` can swap Tauri IPC ↔
  WebSocket without touching every call site.
- Remote access implies, at minimum: per-launch auth token (or password),
  TLS (or documented reverse-proxy setup), bind-address control
  (`127.0.0.1` default, explicit opt-in for `0.0.0.0`), idle/lock timeout.
  Without these, an open gateway is an open SSH vault — this is the part
  that must be designed, not bolted on.

**Why parked:** epic-sized (Rust serve mode + frontend IPC layer + auth
story); batching it here would sink the current guardrail fix. Tracked as
upstream issue **#270** (filed 2026-10-30) to gather interest; implementation
splits into stacked PRs (IPC abstraction → serve mode → auth hardening).

**Progress (fork, 2026-10-31) — 2a DONE, not in the upstream batch:**
- `submarine serve [--port/--bind/--data-dir/--frontend-dir]` works:
  axum static + token-gated WS (`?token=`/Bearer, constant-time compare),
  JSON `{id,cmd,args}` ↔ `{id,ok,result/error}` protocol.
- Command-split pattern proven (body byte-identical, thin Tauri shell kept):
  `app_info` (direct), `list_profiles` → `list_profiles_inner(&Path)`,
  `get_servers` → `get_servers_impl(&DbState)`; shared `new_db_state()`.
- Smoke vs real vault dir: `app_info` ✅ version, `list_profiles` ✅ real
  names, `get_servers` locked ✅ `DATABASE_NOT_INITIALIZED`, unknown cmd +
  malformed frame ✅ structured errors, HTTP 401 no-token / 200 with token.
- Deps (`axum`, `tower-http`) gated desktop-only like `rfd`/`open`;
  `serve` module `#[cfg(not(target_os = "android"))]`.
- Thermo loop closed on 2a: single auth gate (a divergent WS-handler
  re-check that 401'd Bearer upgrades is gone), warn-on-mkdirs-failure,
  no-`unwrap` send path. 7-assertion smoke incl. Bearer regression.
- **2b DONE (Item 3 in the batch for its `src/` half):** `Emit` enum
  (AppHandle | broadcast) + sub/unsub + sub-ack protocol + per-connection
  fan-out; `disconnect_session` as first emitter; read-model ports
  (`get_notes/ssh_keys/commands/folders/credentials`, `select_profile`,
  `check_db_exists`, `setup_master_db` + vault-open seam);
  frontend WS transport + gateway mount + window stub (`src/gatewayWindow.ts`).
  13-assertion smoke incl. real sub→event delivery, unlock-fails-closed.
- **2c DONE (backend, epic track):** `initiate_connection` ported via the
  `*_inner` + `Emit` split (body byte-identical). Cascade contained:
  `ClientHandler.app`, `connect_jump_host`, `start_tunnel_specs_on`,
  `run_keyboard_interactive`, `tunnel::start_tunnel` + 10 worker fns
  (`emit_update`, `set_state`, `enter`, 3× `run_*_forward`, 4× `handle_*`,
  `ActiveGuard`/`ForwardEntry.app` fields). Desktop shells unchanged.
  Smoke: quick-connect to refused port → 8 real `session-log` events +
  `connection-failed`, fails closed. Desktop `tauri dev` boots clean.
- **Phase 3 DONE (terminal + files):** terminal splits (`open/write/
  resize/close_terminal` + inners), 9 SFTP read splits, HTTP `POST
  /api/files/upload` (multipart→temp→SFTP→cleanup, chunked streaming)
  + `GET /api/files/download` (temp→stream→cleanup, attachment
  disposition). Actionable `MOVED`/`DESKTOP_ONLY` errors for
  local-path-only commands. Fail-closed smoke throughout (incl. no-temp-
  leak regression test). Binary WS frames deliberately NOT built —
  terminal output is already base64-in-JSON and uploads stream over HTTP;
  no measured need. Naming unified to `*_inner` everywhere.
- **Serve static 401 fix (2026-11-02):** the token gate covered the
  static bundle too, so JS/CSS/favicon 401'd in a real browser (it never
  sends `?token=` for subresources). Fix: gate applies to `/api/*` only;
  the bundle is public (no secrets in it), all data stays behind the
  token. Verified the exact failing assets return 200 tokenless.
- **Terminal keyboard copy (2026-11-02):** `Ctrl+Shift+C` now copies the
  current selection (same wrap-aware text + toast as select-to-copy);
  plain `Ctrl+C` still sends SIGINT. Extended the existing keydown gate
  in `TerminalView.tsx` — no new listener. (`FORK.md` updated.)
- **Gateway upload false-error (2026-11-02):** `input.files` is a live
  list; the picker reset (`e.target.value = ""`) emptied it mid-upload, so
  the post-upload `files[0].name` read threw, the catch reported a false
  "Upload failed", and `refresh()` never ran — even though the file had
  landed. Fix: snapshot to a plain array before the first await.
- **camelCase arg normalization (2026-11-02):** the whole frontend sends
  camelCase keys (`sessionId`, `localPath`, `privateKey`) which desktop's
  bridge accepts but the gateway rejected (`MISSING_ARG: session_id` on a
  real server open). Fix: `normalize_args` at the single dispatch funnel
  maps top-level camelCase→snake_case (explicit snake wins ties; nested
  payloads already match). Smoke now uses SessionView's exact camelCase
  shapes so this class can't regress silently.
- **Stale-token UX (2026-11-02):** a tab restored from sessionStorage held
  the previous boot's token, so the app mounted against a dead token with
  only cryptic WS console errors. Fix: `GET /api/ping` (gated, 200 vs 401
  distinguishes dead-token from dead-server) + `forgetGateway()` /
  `checkGateway()` in `ipc.ts` (WS close probes; 401 clears storage and
  fires `submarine-gateway-invalid`, no more doomed-token retry loop) +
  `App.tsx` mount-time probe and a dedicated "Gateway token rejected —
  re-open the URL from the server console" card. Smoke covers ping
  200/401/missing-token.
- **Remainings DONE (2026-11-02, epic track):** interactive auth
  continuations (`verify_fingerprint_response`, `submit_kbi_response` —
  stale-nonce no-ops, unblock 2FA/host-key in browser) + `persist_vault`;
  tunnels full set (`start/stop/list/persist_session/restart_session`);
  vault CRUD batch (34: servers/folders/credentials/ssh_keys/commands/
  notes/history/reveal/generate/clone/reorder/set_*) + `create/delete_
  profile` (data_dir seam); mirrors (4), monitors (11, `+settings` in
  `ServeCtx`), SSH probes (5), docker (17, `+docker_streams` in `ServeCtx`),
  `parse_ssh_config`/`parse_client_import`; explicit `CLOUD_ONLY` arm
  (cloud/sync/share/identity) and extended `DESKTOP_ONLY`
  (export/import_profile, pickers). 112 dispatch arms, no dups.
  16-section WS smoke ✅ (CRUD fail-closed, CLOUD_ONLY/DESKTOP_ONLY/MOVED
  arms, tunnel no-session paths). `cargo check` clean (only pre-existing
  hlc warnings); desktop `tauri dev` boots ✅.
- **Phase 4 DONE (auth hardening):** bind consent (`--allow-remote`
  required off-loopback, loud no-TLS warning otherwise); token via
  `--token` / `SUBMARINE_SERVE_TOKEN` / generated; in-process TLS
  (`--tls-cert/--tls-key`, axum-server 0.7 + pinned aws-lc-rs provider);
  idle auto-lock (`--lock-after`, default 30m) reusing
  `close_profile_inner` + `vault-locked` broadcast; token stripped from
  the address bar on load (`history.replaceState`, sessionStorage keeps
  working). 7-assertion smoke incl. TLS roundtrip, lock event, Bearer
  parity, no-temp-leak checks. Reverse-proxy remains the documented
  choice for public hosts.

## Pre-push checklist (rules.md)

- [ ] Thermo review clean (no findings) on final diff
- [ ] `npm run typecheck`, `npm run build` clean
- [ ] Exercised in `npm run tauri dev` (hot-reload is not proof)
- [ ] Rust touched? → `cargo check`/`cargo build` in `src-tauri` clean
- [ ] `git diff --stat` shows only `src/` files, one logical batch

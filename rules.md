# Push cycle — thermo review to safe push

Binding for every change that leaves this machine. No exceptions.

## 1. Thermo review

Run the `thermo-nuclear-code-quality-review` skill against the working diff.
Be ambitious: hunt code-judo restructures, duplication, spaghetti branching,
boundary leaks — not nits.

## 2. Fix to no-findings

Address every finding. If a finding is rejected, write the justification
down (commit message or code comment) — "it works" is never a justification.

## 3. Re-review until clean

Run the thermo review again on the fixed diff. Repeat 1–2 until the review
ends with **no findings**. That verdict is the gate: nothing commits,
pushes, or PRs before it.

## 4. Safe checks (all must pass)

- Dev-server side works: the feature behaves correctly in `npm run tauri dev`
  (hot-reload is not proof — exercise the actual flow: rename, restart,
  reconnect, edge cases).
- `npm run typecheck` clean.
- `npm run build` (vite frontend) clean.
- Rust side untouched? Then no cargo check needed. Touched Rust? Then
  `cargo check` / `cargo build` in `src-tauri` must also pass.

## 5. Commit

- Upstream-eligible work (`src/` features) and fork-only work
  (`src-tauri/tauri.conf.json` identity, `FORK.md`, `rules.md`, local
  tooling) go in **separate commits**. Never mix them.
- One logical change per commit, imperative subject line.

## 6. Push + PR

- Push the branch to the fork (`origin`).
- Upstream PRs branch off `upstream/main` and contain **only**
  upstream-eligible files. Verify with `git diff --stat` before pushing:
  no identity, docs, or rules files in the PR diff.
- PR body documents behavior + test notes; link any follow-ups as issues,
  not extra commits on the PR.

Quick reference:

```powershell
npm run typecheck
npm run build
git status -sb; git diff --stat
git push origin <branch>
gh pr create --repo SinaXhpm/Submarine --head guntur-d:<branch> --base main --title "<title>" --body "<body>"
```

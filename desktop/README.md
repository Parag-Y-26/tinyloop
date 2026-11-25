# tinyloop desktop

Tauri 2 + React 19 + Vite. The Rust shell links `tinyloop-core` at the repo root.

## Run

```bash
# from repo root
export PATH="$HOME/.cargo/bin:$PATH"
cd desktop
pnpm install
pnpm tauri dev          # hot-reload dev app
```

## Build a release binary

```bash
pnpm tauri build --no-bundle
# -> src-tauri/target/release/tinyloop-tauri.exe
```

`bundle.active` is false for now; enable it and add a full icon set when you want an installer.

## Current status

- UI shell, layout, motion, dark mode, command palette: working.
- `context_budget` returns the real 8K result cap (9,828 chars) from `tinyloop-core`.
- `run_task` is wired: the Run button calls the Rust loop engine, which streams `run_event` steps back into the timeline.
- Core loop works in tests: plan → read → patch → verifier → Done, plus 3-strike escalation and text-only bounded repair.
- **Real model verified:** `llama serve` (build 12652) + `Spark-X2.6-4B-Q4_K_M.gguf` on port 9941. Probe passes, and a real read → patch → verify task finished `Done` in 4 steps.
- Approval flow is wired: in `Ask` mode a write/command raises an Allow/Deny card bound to the tool args + SHA-256. Deny leaves the file untouched. `cancel_run` stops at the next loop boundary.

Start the server:

```bash
cd ~/.llama/models
llama serve -m Spark-X2.6-4B-Q4_K_M.gguf -a spark-x26 -c 8192 -np 1 -ngl all -fa on --jinja --host 127.0.0.1 --port 9941
```

## Verification

```bash
pnpm build                 # tsc --noEmit + vite build — passes
pnpm test                  # Vitest + jsdom — 2/2 pass
cd src-tauri && cargo clippy --all-targets -- -D warnings   # clean
```

## Installers

Built with `pnpm tauri build`:

- `src-tauri/target/release/bundle/msi/tinyloop_0.1.0_x64_en-US.msi` — 4.59 MiB
- `src-tauri/target/release/bundle/nsis/tinyloop_0.1.0_x64-setup.exe` — 3.32 MiB

## Eval

`docs/EVAL_RESULTS.md` — 15/15 real-model tasks, 4,551 tok/s prefill, 40.9 tok/s decode, 88.9% cache hits, 0 tool errors.

Icon source: `icon-src.png` (generated, rounded blue tile with a white "t"). Regenerate with `pnpm tauri icon icon-src.png`.

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
- **Not runnable end-to-end yet:** `llama-server` is not installed and the GGUF is not present on this machine. A real run will fail at the startup probe until both exist.
- Approval flow is not wired: `Ask` mode blocks writes/commands by design; use `Auto` for a full run. `approve` / `deny` / `cancel_run` are the next wiring item.

## Verification

```bash
pnpm build                 # tsc --noEmit + vite build — passes
cd src-tauri && cargo check --release   # passes
```

Icon source: `icon-src.png` (generated, rounded blue tile with a white "t"). Regenerate with `pnpm tauri icon icon-src.png`.

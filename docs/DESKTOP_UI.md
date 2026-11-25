# Desktop UI decision and design (v1)

The interface changes from CLI to a native desktop app. The agent core does **not** change: every loop, budget, verifier, and sandbox rule in `ARCHITECTURE.md` and `FLOW_REASONING.md` stays valid. Only the surface changes.

## 1. Stack decision

| Layer | Choice | Why |
|---|---|---|
| Shell | **Tauri 2** (Rust) | Reuses the Rust core directly, ships a small binary, native window/menus/tray on Windows and macOS. Electron would duplicate a browser runtime and is much heavier. |
| Core | `tinyloop-core` library at repo root | The existing `src/` modules become a library consumed by both the desktop shell and an optional headless CLI. |
| UI | **React 19 + TypeScript + Vite** | Best ecosystem for smooth, interruptible motion and complex state (live runs, approvals, sessions). |
| Motion | Motion (`motion`/framer-motion) in the real app; CSS transitions in the standalone prototype | Spring transitions retarget when the user changes intent — exactly what `make-interfaces-feel-better` requires. |
| Styling | Hand-written CSS variables, no utility framework | Apple-like polish needs precise control of translucency, radii, shadows, and spring curves; a generic component kit would fight it. |
| Icons | SF Symbols on macOS via Tauri native APIs; inline SVG elsewhere | Avoids an icon-font dependency and keeps optical alignment. |

**Rejected:** Electron (too heavy), GPUI (excellent but steep, weak animation/tooling ecosystem for this scope), Slint/egui (native but hard to reach Apple-level polish), SwiftUI (macOS-only; this machine is Windows).

## 2. Surface archetype

This is an **Operate** surface with a secondary **Monitor** layer: the user drives tasks and makes approval decisions, while watching live step state. It is not a landing page — no hero, no feature cards, no marketing copy.

Composition (three columns, macOS app posture):

```
┌─────────────────────────────────────────────────────────────────────────┐
│ ● ● ●   tinyloop   [ Ask | Auto | Dry run ]  Spark X2.6 · 8K   ▶ Run    │
├──────────────┬──────────────────────────────────────┬───────────────────┤
│ Sessions     │ Task: find why calc_total uses 8%   │ Goal              │
│  • tax fix   │                                      │ • tests green     │
│  · report    │  PLAN      ✓ 3 steps                 │ Boundaries        │
│  · audit     │  ACT       ⟳ read_file src/tax.rs    │ • no test edits   │
│              │  ACT       · search_files "8%"       │ Verifier          │
│  New task +  │  VERIFY    · pending                 │ cargo test        │
│              │                                      │ ───────────────── │
│  Model ready │  [ composer: describe the task… ]    │ Approval required │
│  30 tok/s    │                                      │ [Allow] [Deny]    │
└──────────────┴──────────────────────────────────────┴───────────────────┘
```

- **Left — Sessions:** task list, new-task action, model status (measured tokens/s, cache hits).
- **Center — Run:** task title, live activity timeline with exact status, token counts in tabular numerals, and the composer pinned at the bottom.
- **Right — Inspector:** frozen goal + boundaries + verifier (the Clarify contract), plus the active approval card. The UI cannot edit acceptance criteria — they come from the core.

## 3. Design tokens (Apple-like, not a gradient soup)

- Type: `-apple-system, BlinkMacSystemFont, "SF Pro Text", "Segoe UI", sans-serif`; mono for paths/commands.
- Neutrals: macOS system grays; one accent (`#0a84ff` light / `#0a84ff` dark), status green/amber/red.
- Surfaces: layered translucency (`backdrop-filter`) with **real** depth — window > sidebar/toolbar > card > popover. Glass is used only where there is actual layering; no blanket blur on every element.
- Radii: concentric — outer radius = inner radius + padding.
- Motion: enter = opacity + 8px translateY + slight blur, ~260ms `cubic-bezier(.32,.72,0,1)`; exit = 150ms; press = `scale(.96)`; icon swaps cross-fade. `prefers-reduced-motion` disables non-essential motion.
- Hit areas ≥ 40px, ideally 44px. Focus rings from the border token, not a colored glow.
- Text: `text-wrap: pretty` for descriptions, `tabular-nums` for token/time counters, antialiased font smoothing.

## 4. Core ↔ UI contract

Tauri commands (UI → core): `run_task`, `approve`, `deny`, `cancel_run`, `new_session`, `set_mode`, `list_sessions`.

Tauri events (core → UI): `run_started`, `step_planned`, `tool_call`, `tool_result`, `approval_required`, `verification_passed`, `verification_failed`, `run_finished`, `run_escalated`, `metrics_updated`.

Rules:

1. The UI is a **view and an input surface**, never a second controller. All state transitions still happen in the Rust state machine.
2. Approval cards bind the exact args/path/hash from the core. The UI only forwards the user's decision; it cannot widen an allowlist.
3. The Clarify contract is displayed read-only in the inspector after confirmation.
4. Events are append-only and replayable into `audit.jsonl`, so `tinyloop trace` stays the source of truth.
5. The desktop app and headless CLI share `tinyloop-core`; no logic is duplicated in TypeScript.

## 5. Files

- `desktop/prototype.html` — self-contained, clickable UI prototype; openable directly in a browser, used for design review.
- `desktop/src/` — React app (wired to Tauri in Phase 1b).
- `desktop/src-tauri/` — Tauri shell and command bridge (requires Rust toolchain to build).
- `docs/FLOW_REASONING.md` — unchanged; still the reasoning for the core loop.

## 6. Migration impact on the plan

- CLI does not disappear: it becomes a thin binary over `tinyloop-core` for CI/headless use.
- Todo item `b3a` changes from "clap CLI + Clarify" to "Clarify in core + desktop task composer"; a small headless CLI flag parser remains for tests.
- New verification item: browser/UI smoke check (open `prototype.html`, check console, test run + approval flow) before wiring Tauri events.

## 7. Design self-audit

Slop score **1/10**: one borderline point for glass on layered surfaces (titlebar, sidebar, inspector, popovers). There is real depth behind each layer, so it is intentional; no gradients, no feature-tile grid, no accent rails, no monument stats, no center stack. Surface is Operate, matching the composition.

## 8. Verification status

The standalone prototype was exercised with jsdom: initial DOM (2 steps, 3 sessions, approval card), run flow (2 → 7 steps, all done), approval allow, theme toggle, tweaks, and command palette all worked with **zero JS errors**. Two real bugs found and fixed during verification: `localStorage` unavailable on opaque `file://` origins, and a null reference after mutating a running step's class.

The Rust toolchain was found at `~/.cargo/bin` (not on PATH). Final state: `cargo test --lib` passes **30/30**; core and Tauri `cargo clippy --all-targets -- -D warnings` are clean; `pnpm build` and `pnpm test` (**2/2**) pass. The app launches with window title `tinyloop`, responding, WebView2 active. Installers built: MSI 4.59 MiB and NSIS 3.32 MiB.

`run_task` is wired end-to-end: Tauri command → startup probe → `run_agent` → `run_event` stream → React timeline, with approval and cancel commands. Real-model eval: **15/15** tasks passed (`docs/EVAL_RESULTS.md`). The only unautomated surface is clicking Allow/Deny inside the native window; the approval gate itself is covered by core tests.

One contract fix found during integration: the Tauri dependency must use the root package name `tinyloop` even though the library crate is `tinyloop_core`.

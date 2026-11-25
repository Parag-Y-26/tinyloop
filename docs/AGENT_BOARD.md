# Agent board — tinyloop

Control pane for the current orchestration pass. One owner at a time; every card has an acceptance list and a merge gate. Evidence lives on disk, not in chat.

## Board

| Card | State | Owner | Scope | Merge gate |
|---|---|---|---|---|
| card-001 Real model capability probe | **Merged** | hermes | `src/model/`, `src/bin/probe.rs` | probe returns tools_visible + tool_call_ok |
| card-002 Real end-to-end loop task | **Merged** | hermes | `src/loop_engine/`, `src/tools/`, `src/bin/run.rs` | verifier exits 0 on final state |
| card-003 Approval flow (`approve`/`deny`/`cancel_run`) | **Merged** | hermes | `desktop/src-tauri/src/main.rs`, `desktop/src/App.tsx`, `src/loop_engine/` | Ask-mode patch blocks until approval; deny leaves file untouched |
| card-004 Audit JSONL | **Merged** | hermes | `src/audit.rs`, `src/loop_engine/` | every tool call + approval + token count appended and replayable |
| card-005 Context manager + compaction | **Merged** | hermes | `src/context.rs`, `src/loop_engine/` | 85% trigger, state.md written first, exact recent tail kept |
| card-006 Eval suite + metrics | **Merged** | hermes | `tests/eval/`, `src/bin/eval.rs` | 15/15 tasks, final-state grading, tool validity separate |
| card-007 Installer | **Merged** | hermes | `desktop/src-tauri/tauri.conf.json` | MSI + NSIS installers build |
| card-008 UI click-through harness | **Deferred** | hermes | `desktop/src/App.test.tsx` | Vitest covers clarify + theme; WebDriver click-through unavailable |

## card-001 — Real model capability probe

- **Acceptance**: `/health` OK; tools actually reach the prompt; one native tool call parsed.
- **Evidence**: `target/debug/probe` output — `tools_visible: true`, `tool_call_ok: true`, prompt tokens 23 → 130.
- **Server**: `llama serve` build 12652, `Spark-X2.6-4B-Q4_K_M.gguf`, port 9941, 8192 ctx, `-np 1`, `--jinja`.
- **Gate**: passed.

## card-002 — Real end-to-end loop task

- **Task**: read `Cargo.toml`, replace `TODO` in `answer.txt` with the package name, call `finish`.
- **Acceptance**: verifier `findstr /C:"tinyloop" answer.txt` exits 0; no other file changed.
- **Evidence**: run log — plan accepted, read ×2, patch applied, `VERIFY passed=true`, `STATUS: Done`, 4 steps, 3,924 prompt tokens, 139 completion tokens, 3,535 cache hits.
- **Gate**: passed.

## card-003 — Approval flow

- **Acceptance**: Ask-mode write blocks; Allow executes; Deny leaves the file untouched; cancel stops at the next loop boundary.
- **Evidence**: `cargo test --lib` — 19/19, including `ask_mode_denies_patch_without_an_approver` and `ask_mode_applies_patch_after_approval`.
- **Wiring**: core emits `ApprovalRequired`, `TauriApprover` blocks on a channel, UI `approve`/`deny` resolves by id, `cancel_run` flips an `AtomicBool` checked each loop turn.
- **Caveat**: the UI click-through has not been automated inside the WebView; core behavior is tested, window launches and responds.

## card-004 — Audit

- **Evidence**: `cargo test --lib` includes `audit_trail_has_tool_calls_and_a_report`; `.tinyloop/audit.jsonl` contains `plan`, `tool_call`, `tool_result`, `verify`, `report` entries with run id, tokens, cache hits, and tool errors.
- **Gate**: passed.

## card-005 — Context + compaction

- **Evidence**: `context::tests` (system + exact tail preserved, 85% threshold, bounded state) and `compaction_fires_and_writes_state` engine test.
- **Gate**: passed.

## card-006 — Eval suite

- **Evidence**: `docs/EVAL_RESULTS.md` — **15/15 passed**, average 3 steps, 4,551 tok/s prefill, 40.9 tok/s decode, 88.9% cache hit, 0 tool errors.
- **Gate**: passed.

## card-007 — Installer

- **Evidence**: `tinyloop_0.1.0_x64_en-US.msi` (4.59 MiB) and `tinyloop_0.1.0_x64-setup.exe` (3.32 MiB).
- **Gate**: passed.

## Blockers

| Blocker | Owner | Next action |
|---|---|---|
| Real-window Allow/Deny not click-tested | — | Deferred: needs `tauri-driver` + matching Edge WebDriver; Vitest covers UI logic and 30 core tests cover the gate |

## UI tests

`desktop/src/App.test.tsx` — Vitest + jsdom: clarify modal is required before a run; appearance toggle works. 2/2 pass.

## Notes

- Single-agent pass: branch isolation is used (`team/real-model-probe`) but no parallel writers yet. When card-003 and card-004 run in parallel, keep them on separate worktrees; both touch `main.rs`/`App.tsx`, so one integrator must land them in sequence.
- Candidate shared skill: "small local model → native tools probe → bounded loop" (probe-first harness). Promote after a second project uses it.

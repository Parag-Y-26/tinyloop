# Agent board — tinyloop

Control pane for the current orchestration pass. One owner at a time; every card has an acceptance list and a merge gate. Evidence lives on disk, not in chat.

## Board

| Card | State | Owner | Scope | Merge gate |
|---|---|---|---|---|
| card-001 Real model capability probe | **Merged** | hermes | `src/model/`, `src/bin/probe.rs` | probe returns tools_visible + tool_call_ok |
| card-002 Real end-to-end loop task | **Merged** | hermes | `src/loop_engine/`, `src/tools/`, `src/bin/run.rs` | verifier exits 0 on final state |
| card-003 Approval flow (`approve`/`deny`/`cancel_run`) | **Ready** | hermes | `desktop/src-tauri/src/main.rs`, `desktop/src/App.tsx`, `src/loop_engine/` | Ask-mode patch blocks until approval; deny leaves file untouched |
| card-004 Audit JSONL | **Backlog** | — | `src/audit.rs` | every tool call + approval + token count appended and replayable |
| card-005 Context manager + compaction | **Backlog** | — | `src/context.rs` | 85% trigger, state.md written first, exact recent tail kept |
| card-006 Eval suite + metrics | **Backlog** | — | `tests/eval/` | 5 smoke + 10 full tasks; final-state grading, tool validity separate |
| card-007 Installer | **Backlog** | — | `desktop/src-tauri/tauri.conf.json` | NSIS installer builds and launches |

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

## Blockers

| Blocker | Owner | Next action |
|---|---|---|
| Ask-mode approval not wired | hermes | card-003: add a channel + `approval_required` event; core waits for decision |
| Installer not built | — | card-007: enable `bundle.active`, run `pnpm tauri build` |

## Notes

- Single-agent pass: branch isolation is used (`team/real-model-probe`) but no parallel writers yet. When card-003 and card-004 run in parallel, keep them on separate worktrees; both touch `main.rs`/`App.tsx`, so one integrator must land them in sequence.
- Candidate shared skill: "small local model → native tools probe → bounded loop" (probe-first harness). Promote after a second project uses it.

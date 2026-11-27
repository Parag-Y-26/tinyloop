# WORKFLOW.md — tinyloop Engineering Workflow

This repository follows strict document-driven engineering for every task, regardless of size.

---

## 1. Per-Task Work Loop

1. **Read Context Docs First**: Read `docs/RULES.md`, `docs/ARCHITECTURE.md`, and `docs/TASKS.md` (plus `docs/SYSTEM_DESIGN.md` / `docs/DESIGN.md` if touching APIs, storage, or UI).
2. **Claim & Mark Active**: Pick the target task in `docs/TASKS.md` and mark it `[~]`. When running parallel workers/worktrees, enforce single-file/scope ownership (`docs/AGENT_BOARD.md` rule) so two writers never edit the same file simultaneously.
3. **Write a 3-Line Plan**: State what files change, what invariant is preserved, and how it will be verified before writing code.
4. **Implement Cleanly**: Keep changes small, single-purpose, and uniform; preserve existing docstrings/comments unrelated to the change.
5. **Run the Quality Check**:
   - Adheres to `docs/RULES.md` and `docs/ARCHITECTURE.md`
   - No duplicate logic, dead code, or hardcoded secrets
   - Clear symbol names, small functions, explicit error handling
   - Tests cover critical paths and edge cases (empty, malformed, boundary, large inputs)
   - Sandbox and protocol invariants hold (no path escapes, no symlink writes, no orphaned tool results, single `system` message)
6. **Run Tests, Lint, and Build**:
   - Rust core: `cargo test --lib` and `cargo clippy --all-targets -- -D warnings`
   - Desktop UI (when touched): `cd desktop && pnpm test && pnpm build`
   - Fix any failure before marking work complete.
7. **Update Living Documentation**:
   - Mark completed items `[x]` in `docs/TASKS.md` and add any newly discovered follow-up tasks (`[ ]`).
   - If module structure, data flow, thresholds, schemas, or UI tokens changed, update `docs/ARCHITECTURE.md`, `docs/SYSTEM_DESIGN.md`, or `docs/DESIGN.md` immediately.
   - Log significant architectural choices (what, why, date) in `docs/DECISIONS.md`.
8. **Report Concisely**: Share a brief, plain-language summary citing verified file paths, commit SHAs (if committed), and exact observed command/test results.

---

## 2. Direction & Scope Changes

- If requirements or architecture change, update `docs/DESIGN.md`, `docs/ARCHITECTURE.md`, `docs/TASKS.md`, and `docs/DECISIONS.md` **before** writing code.
- If code and documentation diverge, reconcile them in the same change set so `/docs` never drifts behind `src/` or `desktop/`.

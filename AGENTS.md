# AGENTS.md

Project context and working agreement for AI coding agents. Keep this file small — it loads into every agent's context on every task.

> Stack is Rust (Cargo workspace). Fill in the remaining project-specific blanks (host, CI, issue tracker) under **Project** below.

## Working Style

- **Operate autonomously.** Prefer making reasonable choices over stopping to ask.
- **Fix ALL failures, not just the first.** Tests, lint, type checks — fix everything. Iterate until green.
- **Stay focused** on the current component. Reference other components for context only.
- **Cross-component work**: only when explicitly asked.

## Operating Guidelines

| Situation | Approach |
| --------- | -------- |
| Normal | Answer first, explain after |
| Uncertain | Confidence → best guess → what resolves it |
| Stuck | State it → what tried → what needed |
| Disagree | Directly with alternatives |

**Avoid:** apologetic hedging, asking permission for trivial actions, silent failures.

**Autonomy boundaries:** Autonomy applies to local, reversible actions (reading code, running tests, editing files, pushing to your own branch). These actions **always require explicit user instruction**:

- Merging PRs
- Force-pushing or deleting shared branches
- Deploying, releasing, or promoting builds
- Any action that could reach users or break the team's workflow

**Prohibitions:** never commit secrets, bypass tests, force push to shared branches, hide failures.

**Refactoring bias:** deletion and compaction worth 2x additions (if no data loss). Prefer removing code over adding.

## Project

- **Host:** github.com (private)
- **CI gating:** all CI checks must pass before merge
- **Tool:** `gh` CLI
- **Default branch:** `main`
- **Issues:** GitHub Issues
- **Merge strategy:** squash — single commit per PR
- **Branch naming:** feature branches off `main`
- **Package manager:** Cargo (with `Cargo.lock` committed)
- **Do NOT** use nightly-only features without discussion

## Skills

- **Proactive routing.** Invoke skills on matching intent — the user doesn't have to type the slash command.
- **Design first.** Run `architect-and-design` before non-trivial feature work.

**Research first:** When debugging or implementing features that touch external dependencies, research their documentation BEFORE forming hypotheses.

**Execution:** Run independent ops in parallel; sequential only when output depends on prior.

## Repo Invariants

- `cargo clippy -- -D warnings` must pass (all warnings are errors)
- `cargo fmt --check` must pass (rustfmt enforced)
- No `unsafe` without explicit justification in a comment
- No `unwrap()` in library/production code — use proper error handling (`anyhow`, `thiserror`)
- Workspace layout: `services/` for binaries, `libs/` for shared crates

## Prerequisites

| Need | Why |
|------|-----|
| Rust stable toolchain | Build and test |
| `gh` CLI authenticated to your git host | PRs, issues |
| PostgreSQL 16 (local or container) | Integration tests |
| Docker | Worker runtime (local mode) |

Bootstrap: `cargo build && cargo test`

## PR Quality Checklist

```bash
# Run everything
cargo fmt --check && cargo clippy -- -D warnings && cargo test

# Individual gates
cargo fmt --check            # formatting
cargo clippy -- -D warnings  # lint
cargo test                   # tests
cargo build                  # compile check
```

Every PR must also pass:
- **Telemetry review:** every new function, decision, and error path has info-level logging.
- **Test coverage:** every behavior in the design has a test. New code without tests is incomplete code.

## Testing Guardrails

- Tests required for new code paths.
- No hardcoded ports — use port 0 or dynamic allocation.
- No global mutable state — tests run in parallel by default.
- Scoped test runs: `cargo test -p <crate-name>`
- **Every enum variant through every entry point.** If an API accepts an enum, every variant gets a test through every path. Not one variant that happens to work.
- **Test names are contracts.** If a test is named `create_with_owns_relationship`, it must exercise that relationship. A mismatch between test name and test body is a bug.
- **Exactly-once is always tested.** Any system that delivers events must assert the exact count, not "at least N."
- **No optimizations.** An optimization is a second codepath. A second codepath hides bugs. One path.
- **Design → tests → code.** Always in that order. Tests define the contract. Code fulfills it.

## Telemetry

**Every activity in the system is logged. Every event, every outcome, every decision. No exceptions.** If something happens and there's no log, that's a critical gap. If you can't diagnose a problem from the logs alone, the telemetry is insufficient.

## Keeping this file small

This file is loaded into every AI agent's context on every task. Every line is a tax.

- Invariants live inline (this file). Patterns and reference material live in `docs/*`.
- Component-specific context lives in `<component>/AGENTS.md`.
- No enumeration of components, design docs, or subsystem files.
- **Size budget:** root ≤ 200 lines (warn at 180).

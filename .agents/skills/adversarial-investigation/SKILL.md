---
name: adversarial-investigation
argument-hint: a PR / branch / diff to review, OR a production problem with symptoms and what has been tried
description: |
  Adversarial multi-agent team with two modes that share one evidence standard.
  REVIEW mode: target is a PR, branch or diff. Judges whether the change is correct, the simplest
  fix, and properly tested, using the bundled /code-review and /simplify passes plus Correctness,
  Tests, History, Telemetry (when the project has production telemetry) and Devil's Advocate lanes,
  then a verify phase that keeps only receipted, reachable findings.
  INVESTIGATION mode: target is a production problem. 4-5 agents (Lead, Code-Analyst,
  Telemetry-Analyst, Explorer, Devil's Advocate) reach consensus on root cause and fix in 3 rounds.
  Use when user asks to "run adversarial", "adversarial review", "review this PR hard",
  "run adversarial investigation", "investigate with team", "deep dive production issue",
  "root cause with agents", "adversarial debug", or needs cross-validated review or diagnosis.
  Do NOT use for straightforward log triage, single-component debugging, CI failure diagnosis,
  or docs-only changes with no executable code.
---

# Adversarial Team

## Target

```
$ARGUMENTS
```

**Pick the mode from the target:**
- A PR number or URL, a branch, or "the current diff" → **Review mode**.
- A described problem, symptoms, an incident, or "why does X happen" → **Investigation mode**.
- A PR that claims to fix a production problem → **Review mode**, with the Telemetry lane required
  to confirm the claimed symptom exists in production and to name the metric that proves the fix.

Both modes share the Evidence Standard, the agent definitions and the Knowledge Map below.

## Evidence Standard

Every agent prompt includes this standard. Every claim requires a receipt:

| Agent | Valid Receipt Types |
|-------|-------------------|
| Code-Analyst | `file:line` of executable code (NOT comments/docs) |
| Telemetry-Analyst | Query result with raw numbers, log lines, timestamps |
| Explorer | Issue number, PR number, commit SHA, doc path |
| Devil's Advocate | Any of the above (demands receipts from others) |

No receipt = claim excluded on submission. Unreceipted observations go in ASSUMPTIONS, not evidence.
**Hierarchy:** Tool output > code paths > artifacts. Comments, docstrings, docs, agent reasoning are NEVER evidence.
**Code ≠ production:** Code proves what CAN happen. Telemetry proves what DID happen. NEVER substitute.
**User experience is fact:** Users can be wrong about mechanism, NEVER about their experience. Findings contradicting symptoms → reopen scope.
**Absence ≠ evidence:** "Not found" means search was incomplete, not that it doesn't exist.
**Discrimination:** Before searching for a hypothesis, predict what you would find if TRUE vs FALSE. After evidence FOR, run 2+ searches targeting the FALSE prediction. Counter-searches must vary source (code vs telemetry vs history) or query strategy (different keywords, different entry points, different time windows). List all searches and results.
**Verification:** Verify claims via tool calls, not self-review. Re-reading your own reasoning is not verification.
**Hazards:** (1) Inherited certainty — problem statement is hypothesis, not axiom. (2) Confirmation bias — no disconfirming evidence? Search for refutation. (3) Hasty commitment — one data point ≠ root cause, test an alternative.
**Convergence:** Independent paths reaching the same conclusion raises confidence. Divergence must be investigated, not resolved by majority.
**Reachability (review mode):** A case counts only if a real caller or input in this codebase can produce it. Show the path. Unreachable cases are not findings, and handling them is over-engineering. This applies to bundled-pass output too.
**Reproduce when cheap:** A failing test or script beats an argument.

## Agents

**Lead Investigator:**
- "You are the Lead Investigator. You drive convergence — independent minds toward a single truth. If you filter, frame, or omit, every downstream conclusion is compromised."
- Lead produces ONLY structured outputs per protocol templates. No free-form analysis. No interpretation of evidence. No hypothesis formation.
- NEVER reads code, runs queries, or forms hypotheses. Coordinates logistics, not epistemics.
- **Scoreboard** (externalized after every round):

  | Track | States |
  |-------|--------|
  | Symptoms (verbatim) | `unexplained` → `covered` (agent + receipt) |
  | Hypotheses (flat list) | `alive` → `dead`+receipt / `parked`+receipt+DA approval |
  | Info gaps (owner, scope) | `open` → `closed` / `escalated` |

  **Gate check:** ALL symptoms covered, ALL gaps closed, ALL 3 Evidence-Completeness Gates pass → proceed to consensus.
- **Anti-patterns:** Circular → require new evidence. Position change → "what NEW evidence?" Ranking or filtering agent findings → protocol violation.
- Escalates to human with exactly what is unresolved.

**Code-Analyst:**
- "You are the Code-Analyst. You trace what code does, not what anyone says it does. If a relevant code path exists and you didn't find it, the investigation builds on incomplete evidence."
- **Landscape:** Map every code path relevant to the problem. Inventory: entry points, data flows, error handlers, edge cases, configuration. Hypotheses emerge from evidence.
- When someone says "it can't be X": "show me the code path that makes X impossible."
- Trace data flow end-to-end, hop by hop. After forming a hypothesis, re-read each critical function: "Does this handle [hypothesis-relevant condition]?"
- After every finding: "where else does this pattern exist?" — search the full codebase.

**Telemetry-Analyst:**
- "You are the Telemetry-Analyst. You have sole access to what actually happened in production. If production evidence exists and you didn't surface it, the investigation lacks ground truth."
- **First:** Read any project-specific telemetry/investigation skills from the Knowledge Map (Glob `.agents/skills/investigate-*/SKILL.md`) plus `AGENTS.md` → "Telemetry & Observability" (if present) for cluster URLs, table schemas, query patterns.
- **Landscape:** Survey all telemetry for the problem time window. Inventory: chronological event chain (deployment/config changes, first symptom, error inflections — all with timestamps), error rates, anomalies, state transitions. Hypotheses emerge from data.
- Post-fix validation (when deployed): verifies target metric improves and adjacent metrics unchanged.

**Explorer:**
- "You are the Explorer — the team's memory. If a prior investigation or fix exists and you didn't find it, the team reinvents a failed approach."
- **Landscape:** Build history inventory. Search issues and PRs FIRST (including closed) — 10+ keyword variations. Inventory: deployments, merges, dependency updates in 72h before first symptom (via commits, PRs, release tags), prior investigations, related fixes, regression history, design decisions. No prior art found? State explicitly — absence of history = novel problem class.
- First report must answer: "Has this been investigated before? Is there an existing fix? Could this be a regression?"
- When team converges: search for related regressions, adjacent subsystem impacts, prior failed fixes for the same root cause.

**Devil's Advocate (DA):**
- "You are the Devil's Advocate. Your job is to ensure the investigation is complete AND to kill weak conclusions. If you fail to challenge a weak claim, you are the last line of defense and the investigation ships a wrong answer. Politeness is not a factor."
- Operates across all lanes — challenges evidence from any agent.
- **Four duties:** (1) Coverage — was every domain checked per hypothesis? (2) Receipt — does each receipt support its claim? (3) Scope — was anything narrowed without justification? (4) Alternative — what fits confirmed evidence but wasn't explored? If an alternative survives, consensus fails.
- Each challenge MUST specify the **required receipt type** to resolve it (`file:line`, `query result`, `commit/PR/issue`). This determines which agent owns the response. Lead returns challenges missing a receipt type tag.
- A challenge REFUTED with strong evidence improved confidence — that is DA success. Quality = investigation value, not survival rate.
- **R1 (independent):** NEVER receives other agents' findings. Own landscape mapping. Challenges problem definition + scope. Deliverable: 2+ alternative explanations derived from own landscape. Assume the problem statement's implied causation is wrong — what else produces these symptoms? 5-Whys: is this the right problem or a symptom?
- **R2 (adversarial):** Receives all R1 reports. Runs four duties. Extracts assumptions from each report: "verified with a tool or inferred?" Audits Lead's digest for omissions against raw reports.
- **R3 (completeness):** Verifies Evidence-Completeness Gates. Signs off or identifies gaps. Consensus CANNOT proceed without DA sign-off.
- **Stress test:** Generates 1 NEW hypothesis (not previously proposed) contradicting consensus that fits all confirmed evidence. No refutation → escalate. Challenges fix: root cause or masking?
- When satisfied: "I accept [X] because [evidence]. I still challenge [Y]."

## Knowledge Map

Every agent prompt MUST include this map. Sub-agents start fresh — zero inherited context.

```
## KNOWLEDGE MAP — Read what you need for your role

Architecture & conventions:
- Repo context:             ./AGENTS.md (workspace layout, components, quality gates, MCP servers, git ops)
- Component-level docs:     ./<component>/AGENTS.md or ./<component>/CLAUDE.md (if present)
- Architecture docs:        Glob **/docs/ARCHITECTURE*.md
- Design docs:              Glob **/docs/design/*.md — Grep pattern="<keyword>" to find relevant ones
- API & protocol specs:     Glob **/docs/*API*.md, **/*.proto, and any project-specific protocol docs

Telemetry & production observability (Telemetry-Analyst reads BEFORE querying):
- ./AGENTS.md → "Telemetry & Observability" section — authoritative source for telemetry system,
  cluster URLs, table / namespace names, filters, correlation conventions, and noise patterns for this repo.
- Any project-specific investigation skills (Glob .agents/skills/investigate-*/SKILL.md) extend the basics.

Cross-component correlation:
- ./AGENTS.md → "Telemetry & Observability" → correlation IDs / trace-id / CV conventions section.

MCP & tooling bootstrapping:
- MCP config:               ./.mcp.json (if present — cluster URLs, auth, server commands)
- ./AGENTS.md → "MCP Servers" section lists what's configured and how each is used.

CI/CD pipelines:
- ./AGENTS.md → "PR Quality Checklist" section — required checks and how to invoke them.
- Pipeline definitions:     Glob .github/workflows/**/*.yml or .pipelines/**/*.yml

Skills (agents can read but NOT invoke):
- All skills:               Glob .agents/skills/*/SKILL.md, .claude/skills/*/SKILL.md

Past investigations and project memory:
- Memory notes / reports:   Glob .memories/**/*.md (if the project keeps a memory directory)
- Issues (all states):      gh issue list --state all --search "<keywords>"
- PRs (all states):         gh pr list --state all --search "<keywords>"
- Commits by keyword:       git log --oneline --grep="<keyword>" -20
- Recent changes to file:   git log --oneline -10 <file>
```

If `AGENTS.md` has no "Telemetry & Observability" section yet, the Telemetry-Analyst cannot contribute — proceed with 4 agents instead of 5 (see "5 agents" note at the top — Telemetry-Analyst is optional for pure code/design problems with no runtime component).

## Review mode

Your job is to find what is wrong with the change, not to confirm it works. The PR description, commit
messages and comments are claims to test, not facts (hazard: inherited certainty).

**Priorities, in this order (never trade an earlier one for a later one):**
1. **Correct:** fixes the real symptom in every REALISTIC case and breaks nothing.
2. **Simple:** the smallest change that achieves (1). Name what can be deleted.
3. **Tested:** the tests would fail without the change, exercise the real path, are deterministic,
   and are no bigger than needed.

### Phase 1: Gather (all in parallel)

**Bundled passes**, REPORT ONLY (never `--comment`, never `--fix`, no edits in this phase):
- `/code-review max <target> --max-findings all`
- `/simplify` on the target, report only. On someone else's PR, run it in a throwaway worktree of the
  PR head and discard its edits.
- Security: when the diff touches auth, sandboxing, policy, secrets, path validation, subprocesses,
  network, deserialization or anything that reaches a model's context, also run `/security-review`
  (or the matching `sec-*` skills).

When the caller already runs `/code-review` and `/simplify` around this pass (the `/implement-e2e`
gauntlet does), skip the bundled passes and run the lanes only.

**Lanes**, one role per `general-purpose` subagent. Each gets the diff, the stated goal, the Evidence
Standard and the Knowledge Map:
- **Correctness** (Code-Analyst): trace every touched path end to end, including callers and LATER
  steps that can override or bypass it. Look for missed cases, ordering, stale state, races, blocking
  I/O on async paths, and violated invariants (AGENTS.md and the owning spec or design doc). After each
  finding, ask "where else does this pattern exist?"
- **Tests:** would each new test fail on the merge-base? Real path or mocked shortcut? Which realistic
  case from Correctness has no test?
- **History** (Explorer): prior PRs, issues and commits (including closed) for the same symptom,
  earlier fixes, regressions, recorded decisions, and the review comments already on the PR.
- **Telemetry** (Telemetry-Analyst): required when the change claims to fix a production symptom or
  touches a hot or production-critical path. Optional only when the project has no production
  telemetry (no "Telemetry & Observability" section in AGENTS.md and no `investigate-*` skill); say so
  explicitly. Answer with query receipts:
  - Does the symptom the change claims to fix actually show in production: how often, since when,
    which tenants or paths?
  - What are the call volume, latency and error rate of the code paths the diff touches (the blast
    radius if it is wrong)?
  - Does the diff change emitted telemetry (log or event names, metric names or labels, span names)
    in a way that breaks a dashboard, an alert or a query?
  - Which metric proves the fix after deploy, and what is its current baseline?
- **Devil's Advocate:** independent; NEVER sees other lanes' output first. Assume the stated cause or
  approach is wrong: what else explains the symptom, and what simpler fix exists? Then, in Phase 2,
  challenge every candidate finding.

### Phase 2: Verify

The Devil's Advocate and the coordinator take the MERGED list from every pass and deduplicate it. Each
candidate gets one disposition:

| Disposition | Meaning |
|---|---|
| **CONFIRMED** | Receipt, and a reachable path |
| **REFUTED** | A receipt shows it cannot happen or is already handled |
| **UNVERIFIED** | Plausible, no receipt |
| **ALREADY RAISED** | On the PR already, and not regressed |

Only CONFIRMED findings are reported. Bundled-pass output gets the same scrutiny as the lanes, with no
trust because of where a finding came from.

**Don't flag:** style handled by formatters or linters, naming consistent with surrounding code, scope
the change never claimed, or hardening it does not need.

**Severity:** Blocking (bug, security, data loss, breaks CI) · High (missed realistic case, race, perf
regression, missing test for the fix) · Medium (simplification, test gap on an edge) · Nit (only if it
is a pattern).

### Output

- **Verdict:** MERGE / MERGE WITH NITS / REVISE, with a confidence score (0–100) and why.
- **Findings,** most severe first, each with: `file:line` · category (correctness | simplification |
  test | security | telemetry) · severity · source lane · one-sentence claim · failure scenario or
  simpler alternative · receipt · confidence.
- **Production context** (when Telemetry ran): symptom frequency, blast radius, and the proving metric
  with its baseline.
- **Held back:** REFUTED / UNVERIFIED / ALREADY RAISED items, one line each.

### Then act

- **Your own change:** fix every CONFIRMED finding (mechanical ones may use `/simplify` or
  `/code-review --fix`), re-run the green gate, then re-review the new diff once.
- **Someone else's PR:** confirm the head SHA has not moved, then post the CONFIRMED findings as ONE PR
  comment, skipping anything already raised. Do not use inline-comment mode.

## Investigation mode: Protocol

5 agents: 4 mandatory (Lead, Code-Analyst, Explorer, Devil's Advocate) + Telemetry-Analyst (mandatory when production is involved, optional for pure code/design with no runtime component and no telemetry claims). Unanimous consensus on: scope, root cause, and fix. The Rounds, Disposition, Consensus and Constraints sections below apply to investigation mode only.

### Round 1: Landscape Mapping
1. **Lead input:** Verbatim problem statement + user-reported symptoms. No assumptions, no framing, no scope direction.
2. **Lead spawns all agents in parallel** as `general-purpose` subagent_type. Every prompt: problem statement, symptoms, Evidence Standard, Knowledge Map, lane definition.
3. **Each agent maps their domain independently.** For each finding: receipt, causal relationship to problem, and 1+ counter-search. Breadth limited to problem-relevant items. Omissions acceptable if SCOPE explains.
4. **Report structure** (Lead sends back non-compliant reports):
   - **Domain inventory:** What exists in this lane relevant to the problem
   - **Emergent hypotheses:** What the evidence suggests (with receipts)
   - **Evidence FOR and AGAINST** each hypothesis (receipts, counter-searches listed)
   - **Problem-statement verification:** Verify or contradict claims relevant to your lane
   - **SCOPE:** "Searched [list]. Did NOT search [list + reason]."
   - **ASSUMPTIONS:** `ASSUMED: [claim] — [why unverified]` per unverified claim

### Evidence Digest (between every round)
Lead compiles from agent reports — structured template, no free-form analysis:
- Confirmed findings with raw evidence (log lines, file:line, query results — not summaries)
- Hypotheses: flat list (alive / dead+kill receipt / parked)
- Symptom map: each symptom → finding+receipt or `UNEXPLAINED`
- System check: architecture/design issue rather than local bug? Answer with evidence or add as hypothesis.
- Reframing: did any landscape suggest a different problem? → add as hypothesis
- Scoreboard state (all agents see it for self-correction)
- Follow-up questions (derived from agent-identified gaps only)

**DA audits the digest** during R2. Compares against raw reports. Omissions corrected in next digest.
DA receives full prior-round reports for audit. Other agents receive compressed digest (one-line fact + receipt pointer per finding).
**Cross-lane:** When a finding needs cross-domain verification, Lead bridges with raw evidence attached (not just receipt pointers): "Code-Analyst found X [full evidence] — Telemetry: query for X's production signature."

### Round 2: Adversarial Review
Lead spawns all agents with Evidence Digest + role-specific tasks:

| Agent | R2 Task |
|-------|---------|
| **DA** | All R1 reports. Four duties. Digest audit. Each challenge includes acceptance criteria. |
| **Code-Analyst** | Cross-lane verification + DA challenges requiring code investigation. |
| **Telemetry-Analyst** | Cross-lane verification + DA challenges requiring production evidence. |
| **Explorer** | Blast radius, regression checks, prior art for DA's alternatives. |

R2 is scoped: agents answer R1 gaps and DA challenges, not re-investigate from scratch.

### Round 3: Resolution
Lead spawns all agents with R2 Digest. Each agent resolves their lane's DA challenges and gaps.

**R3 exit → verdict or escalate:**
1. Every DA challenge from R2 and R3 has a disposition (per §Disposition)
2. All 3 Evidence-Completeness Gates pass (per §Consensus)
→ All pass: stress test then verdict. **Any fail: ESCALATE to human with specific failing criteria.**
When escalating: distinguish SATURATED (evidence exhausted) from BLOCKED (evidence unreachable — specify what and why).

### Disposition Protocol
DA challenges are ELEVATED. For each, Lead produces ONE disposition with a receipt:

| Disposition | When | Evidence | DA Sign-off |
|-------------|------|----------|-------------|
| **REFUTED** | Alternative ruled out | Receipt contradicting it | No |
| **ABSORBED** | Compatible with verdict | Receipt showing compatibility | **Yes — DA must agree** |
| **PROMOTED** | Not refuted | Elevate to hypothesis for R3 | No |
| **ESCALATED** | Unresolvable | What evidence is missing | No |

**SCOPE challenges cannot be ABSORBED.** Scope was legitimately narrowed (REFUTED) or wasn't (PROMOTED).
Each challenge MUST include acceptance criteria. Without criteria → UNFALSIFIABLE, cannot block consensus.
**Classes:** SCOPE (narrowing without justification) · EVIDENCE (receipt doesn't support claim) · ALTERNATIVE (unexplored explanation fits evidence).

## Investigation mode: Consensus
Unanimous agreement on scope, root cause, fix. One dissent = investigate. DA dissent elevated per §Disposition.

**Evidence-Completeness Gates (all 3 MUST pass):**
1. **Symptom coverage:** Every symptom covered by 2+ independent evidence-producing agents. When Telemetry-Analyst is absent, symptoms coverable by only 1 agent → ESCALATE. No `UNEXPLAINED`.
2. **Discriminating evidence:** Per surviving hypothesis, evidence distinguishes from alternatives — not just compatible.
3. **DA sign-off:** Four duties satisfied. No unresolved gaps.

**Stress test (after gates pass):** Revive parked/dead hypotheses by receipt ("what rules this out?"). DA generates 1 contradicting hypothesis. Every hypothesis needs a disproof test (unfalsifiable = cannot be root cause). Any failure → escalate.

**Verdict** (all fields required, confidence ≥75 = CONFIRMED, 50-74 = PRELIMINARY, <50 = INCONCLUSIVE):
1. **Root Cause** — one sentence. INCONCLUSIVE: top hypotheses + what discriminating evidence is needed.
2. **Confidence** — 0-100 + justification (how many agents converged, via what independent paths).
3. **Evidence Chain** — ≥3 hops: symptom → proximate → root. Each hop: fact receipt + causal receipt.
4. **Symptom Coverage** — per symptom: covering agents, finding, receipt.
5. **Blast Radius** — affected components, code paths, teams.
6. **Fix** — mapped to evidence chain hop. Symptom-only = "mitigation." Regression risk + test coverage.
7. **Gates + Stress Test** — pass/fail per gate. DA contradiction hypothesis + result.
8. **DA Disposition Log** — per challenge: class, criteria, disposition, sign-off, evidence.
9. **Dissent** — unresolved disagreements + what evidence resolves them. Or: "Unanimous."

## Investigation mode: Constraints
1. All agents: `general-purpose` subagent_type (MCP + Bash required).
2. Every agent prompt includes Evidence Standard + Knowledge Map.
3. Exactly 3 rounds. R3 gate failure → escalate to human. No additional rounds.
4. One role per subagent. Combined agents lose adversarial independence.
5. Single problem scope. Multiple symptoms may share root cause; unrelated problems = separate invocations.
6. R1: Agents investigate within their designated lane only. R2/R3: Cross-lane evidence provided only when assigned by Lead via digest.
7. No spawning outside round structure. No ad-hoc investigation.
8. Every factual assertion MUST attach a receipt. No exceptions.
9. Conclusions require discriminating evidence — must rule out alternatives, not just be compatible.
10. All surviving hypotheses receive equal investigation effort. Flat list is a behavioral mandate, not a formatting suggestion.
11. Fix MUST map to evidence chain hop. Symptom-only fix = "mitigation."

# Classification Rules, Scoring & Adversarial Protocols

## Universal Evidence Protocol

All agents in this skill MUST internalize this protocol. Include in every agent prompt. Non-negotiable.

**Receipts** — every claim requires one:

| Agent | Valid Receipt Types |
|-------|-------------------|
| Architecture Explorer | `file:line` of executable code, dependency graph evidence |
| Pattern Analyzer | `file:line` of pattern usage, counter-examples |
| Test Coverage Auditor | `file:line` of test code or untested source |
| Production Readiness | `file:line` of executable code (NOT comments/docs) |
| Regression History Scout | Issue number, PR number, commit SHA, doc path |
| Devil's Advocate | Any of the above (demands receipts from others) |
| Regression Risk Analyst | `file:line` of callers, test references, blast radius maps |

No receipt after one challenge = claim **excluded** from the refactor plan.

**Evidence hierarchy:** Tool output > executable code paths > verifiable artifacts. NEVER accept as sole evidence for a finding: comments, docstrings, agent reasoning. Documentation (CLAUDE.md, design docs) may be used as context to calibrate findings and avoid false positives, but cannot be the sole basis for confirming or dismissing a finding — always require a code-level or artifact receipt alongside.

**Confidence Calibration:**

| Score | Meaning | Action |
|-------|---------|--------|
| 0-24 | Suspected | Low confidence — note what would confirm |
| 25-49 | Possible | Include but flag for adversarial review |
| 50-74 | Likely | Standard confidence — include in refactor plan |
| 75-100 | Verified | High confidence — multiple evidence paths converge |

Rule: Only findings at 50+ after adversarial challenge enter the refactor plan. Below 50 → dismissed or deferred.

**Counter-Search Requirement:**
After finding evidence FOR an issue, search with equal effort for evidence AGAINST it before reporting. If no disconfirming evidence is found, state what counter-searches were attempted. This prevents confirmation bias — not every code smell is a real bug.

**Discrimination Tests:**
Before searching for evidence, predict what you'd find if the issue is real vs. if it's a false positive. After finding evidence FOR, run 2+ counter-searches AGAINST. List each counter-search and its result.

**Cognitive Hazards** — detect trigger, take action:
- Confirmation bias — Found supporting evidence? Search for refutation before reporting.
- Hasty commitment — One code smell ≠ architectural flaw. Generate + test an alternative explanation.
- Inherited certainty — Findings from other agents are hypotheses, not axioms. Verify with your own tool calls.
- Self-correction trap — "Let me verify" without a planned tool call is not verification. Re-reading your own reasoning is not verification.

**Chain-of-Verification:** For each claim: (1) what tool call verifies this? (2) run it, (3) revise if contradicted.

**Assumptions Section:**
Every agent report MUST end with an explicit ASSUMPTIONS section listing anything believed without direct evidence. Format: `ASSUMED: [claim] — [why not verified]`

**Anti-Patterns to Avoid:**
- Flagging theoretical issues that can't occur in the actual execution path
- Proposing fixes more complex than the problem they solve
- Confusing "unusual pattern" with "bug"
- Counting lines without considering whether the file is genuinely cohesive
- Flagging known, accepted patterns as anti-patterns (read CLAUDE.md first)

---

## Consensus Resolution

After both adversarial agents report, resolve each finding:

| Outcome | Criteria | Action |
|---------|----------|--------|
| **Confirmed** | Confidence ≥ 50 after challenge, no fatal regression risk | Include in refactor plan |
| **Dismissed** | Confidence < 50 after challenge, or Devil's Advocate killed it with evidence | Log as "Dismissed" with reason — do NOT include |
| **Deferred** | Real issue but regression risk too high without mitigation | Include with required mitigation steps |
| **Disputed** | Adversarial agents disagree | Include with "DISPUTED" tag and both perspectives |

**Update confidence scores based on adversarial results:**
- Finding challenged and defended with new evidence: +10 confidence
- Finding challenged with no counter-evidence found: no change
- Finding partially weakened by challenge: -15 confidence
- Finding killed by specific counter-evidence: -30 confidence (dismiss if below 50)
- High regression risk but mitigable: keep, add mitigation requirement
- High regression risk and unmitigable: dismiss or defer with justification

## DA Challenge Resolution Protocol

DA challenges (CHALLENGE, DOWNGRADE, REDIRECT verdicts) are ELEVATED — the orchestrator cannot dismiss them with confidence score adjustments alone or hand-wave them with "noted." For each DA challenge, produce exactly ONE disposition with evidence:

| Disposition | When | Required Evidence |
|-------------|------|-------------------|
| **REFUTED** | DA's challenge is ruled out | Receipt that directly contradicts it. "DA challenged [X]. Evidence proves [not-X] because [mechanism]." |
| **ABSORBED** | DA's concern is valid but compatible with the finding | "DA challenged [X]. Consistent with the finding because [evidence]. Fix unchanged because [receipt]." |
| **DEFERRED** | DA's alternative explanation is unrefuted | "DA proposed [X]. Unrefuted — finding included as DEFERRED with DA's alternative noted. Requires mitigation steps before implementation." |
| **UNRESOLVED** | Cannot resolve with available evidence | "DA challenged [X]. Lack [specific evidence]. Finding included as DEFERRED with note: 'Unresolved DA challenge — requires human review before implementation.' Surfaced in Phase 7 status checkpoint." |

**No other disposition exists.** Every DA challenge gets one of these four with a receipt. No disposition = finding enters the refactor plan as DISPUTED.

**Precedence rule:** Phase 2.4 dispositions override Phase 2.3 confidence-based outcomes for findings that received DA CHALLENGE/DOWNGRADE/REDIRECT verdicts. If a disposition is ABSORBED, the finding remains in the refactor plan regardless of confidence adjustments. If DEFERRED or UNRESOLVED, the finding enters the Deferred Findings section regardless of confidence score.

**DA's obligation:** Each CHALLENGE, DOWNGRADE, and REDIRECT verdict must include acceptance criteria — what specific evidence would satisfy the DA. Challenges without acceptance criteria are parked as UNFALSIFIABLE and cannot block the finding.

**Disposition tracking** (append to refactor document, in the "DA Challenge Dispositions" section):

| # | Finding ID | DA Challenge | Acceptance Criteria | Disposition | Evidence |
|---|-----------|-------------|-------------------|-------------|----------|
| 1 | F001 | [challenge] | [what satisfies DA] | REFUTED/ABSORBED/DEFERRED/UNRESOLVED | [receipt] |

## Stress Test (mandatory before synthesis)

Before proceeding to Phase 3:

1. For each KILL verdict from DA, verify the kill evidence is definitive — not just "unlikely." If the kill rests on probability alone ("requires 3 simultaneous events"), downgrade to DEFERRED rather than DISMISSED.
2. For each confirmed finding, ask: "Could fixing this introduce a worse problem than it solves?" If yes and no mitigation exists, mark DEFERRED.
3. Generate 1 contradiction hypothesis: an alternative explanation for the top findings that would make the proposed fixes counterproductive. If neither DA nor the discovery agents can refute it with a receipt, add it as a DISPUTED finding.

---

## Priority Classification

| Priority | Criteria | SLA |
|----------|----------|-----|
| **P0 - Critical** | Security vulnerabilities, data loss risks, **deadlocks**, **race conditions causing data corruption**, production blockers, **files >1500 lines** | Must fix immediately |
| **P1 - High** | Architectural violations, **single points of failure**, **resource leaks**, **unbounded queues/collections**, pattern inconsistencies affecting >3 files, **files >800 lines** | Fix in current cycle |
| **P2 - Medium** | Missing tests for critical paths, **missing circuit breakers**, **inefficient connection/resource handling**, code duplication >50 lines, **files >500 lines** | Fix in current cycle |
| **P3 - Low** | Style inconsistencies, minor optimizations, documentation gaps | Fix in current cycle |

### Large File Thresholds

Large files are a maintainability and cognitive load issue that compounds over time. They indicate responsibility creep and make testing, reviewing, and debugging harder.

| Lines | Priority | Rationale |
|-------|----------|-----------|
| **>1500** | **P0** | Urgent split required - file is unmaintainable, likely has hidden bugs |
| **>800** | **P1** | High priority split - file is becoming a "God Object" |
| **>500** | **P2** | Should be split when touching this file for other reasons |
| **<500** | OK | Acceptable size for most modules |

**Note:** Line counts exclude blank lines and comments. Test files may have higher thresholds (+50%) since test methods are often verbose.

**IMPORTANT**: The goal is **100% resolution** of all confirmed issues. Lower priority items may be quick wins that improve code quality significantly. Never skip issues just because they're low priority.

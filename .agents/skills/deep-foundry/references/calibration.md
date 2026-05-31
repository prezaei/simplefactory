# Fundamental Calibration

Convert subjective instructions to objective criteria. Models lack temporal sense, calibrated importance, effort gradients, and persistent memory.

---

## Why Calibration Matters

| Limitation | Technical Basis | Implication |
|------------|-----------------|-------------|
| No temporal sense | Positional encodings encode sequence, not duration | "Take your time" is null |
| No calibrated importance | Attention weights are correlations, not value judgments | "Focus on what matters" needs criteria |
| No effort gradients | Computation is fixed per token | "Be careful" doesn't increase compute |
| No persistent state | Autoregressive generation is stateless | Must externalize working memory |
| No intrinsic thresholds | "Enough", "brief", "detailed" have no default | Must specify counts or ranges |

**Research:** LLMs operate as "reactive post-hoc reasoners" rather than proactive planners with persistent internal states (Johns Hopkins/Renmin, 2025).

---

## Core Principle

**Convert vibes into enumerated checks, explicit counts, defined taxonomies, or measurable properties.**

If a human would need clarification to execute the instruction, the model needs specification.

---

## Translation Table

| Subjective (Fails) | Why It Fails | Objective (Works) |
|--------------------|--------------|-------------------|
| "Be thorough" | No metric | "Include: parameters, edge cases, errors, return values" |
| "Be brief" | No length calibration | "Maximum 3 sentences" or "Under 150 tokens" |
| "Be careful" | No safety checklist | "Check for: null derefs, bounds, races, leaks" |
| "Focus on important" | No ranking | "Prioritize: 1) correctness 2) performance 3) maintainability" |
| "Take your time" | No time sense | "Use step-by-step reasoning in `<thinking>` blocks before answering" |
| "Be creative" | Unbounded | "Generate 5 distinct approaches using different algorithms" |
| "When appropriate" | No threshold | "When: input >1000 elements OR latency <10ms" |
| "Enough sources" | No count | "15-20+ sources" |
| "Recent sources" | No timeframe | ">1 year for fast-moving, >5 years for stable" |
| "Significant" | No criteria | "Changes approach, reveals blocker, or >3 attempts" |
| "Complex task" | No definition | ">3 subtasks or unclear requirements" |

---

## Ineffective Instructions

| Instruction | Why It's Wasted | Working Alternative |
|-------------|-----------------|---------------------|
| "Take your time" | No time sense; computation is fixed | Use step-by-step reasoning in `<thinking>` blocks |
| "Think hard" | Cannot allocate additional compute | "Analyze against: [enumerated criteria]" |
| "Be extra careful" | No variable attention mechanism | "Check for: [specific list]" |
| "This is important" | No importance weighting *alone* | **IMPORTANT:** + specific prohibition + consequence (see below) |
| "Double-check" | Must explicitly request verification step | "After completing, verify: [enumerated checks]" |
| "Use best judgment" | Judgment requires criteria | "When [condition], choose [option A]; otherwise [option B]" |
| "Remember that..." | Memory is in-context only | State the fact directly as a constraint |
| "Obviously/Clearly" | Assertion words, not instructions | Remove — or state the thing being asserted |

**The IMPORTANT/CRITICAL formula:** These keywords work when paired with a specific, falsifiable instruction and a consequence. `"IMPORTANT: Never use git commands with the -i flag"` works because the constraint is binary. `"This is important"` alone fails because there's nothing specific to comply with.

---

## Structural Patterns

### Enumerated Checklist

```
Review against these criteria:
1. [Category]: [specific checks]
2. [Category]: [specific checks]

Report: [criterion]: [PASS | ISSUE: description]
```

### Decision Tree

```
Classification rules:
- If [condition A] → [action A], stop
- If [condition B] → [action B], continue
- Otherwise → [default action]
```

### State Externalization

For multi-step tasks in skill **body** content (not frontmatter — XML is prohibited in `name`/`description`):

```
After each step, write:
<state>
  <known>established facts</known>
  <unknown>remaining questions</unknown>
  <next>next action</next>
</state>
```

### Approved XML Tags for Body Content

XML tags are prohibited in `name`/`description` fields but encouraged in body content for structured reasoning and examples.

| Tag | Purpose | Example Usage |
|-----|---------|---------------|
| `<analysis>` | Pre-output thinking | `<analysis>[thought process]</analysis>` |
| `<reasoning>` | Explain why actions taken | `<reasoning>Used X because...</reasoning>` |
| `<commentary>` | In-example annotations | `<commentary>Now use agent Y</commentary>` |
| `<example>` | Good examples | `<example>Correct approach...</example>` |
| `<bad-example>` | Anti-patterns | `<bad-example>Wrong: ...</bad-example>` |
| `<policy_spec>` | Rule definitions | `<policy_spec>If X then Y</policy_spec>` |
| `<state>` | Externalized working memory | See above |

**Pattern:** Use `<analysis>` to enforce structured thinking before output:
```markdown
Before responding, analyze in:
<analysis>
[Ensure all criteria met]
</analysis>
```

### Contract Pattern

```
You are: [role - one line]

Goal: [measurable success criteria]

Constraints:
- [specific constraint 1]
- [specific constraint 2]

If unsure: Say so explicitly and ask 1 clarifying question.

Output format: [exact schema]
```

### Output Control Patterns

For strict output requirements:

| Pattern | Use Case | Example |
|---------|----------|---------|
| Exclusive output | Structured data only | `Return ONLY valid JSON. No markdown, no explanation.` |
| Constrained length | Summaries | `Maximum 3 sentences` or `Under 100 words` |
| Format enforcement | Programmatic consumption | `Your output MUST start with {` |
| Turn ending | Workflow control | `Your turn should only end by: [list of valid endings]` |

**Example:**
```markdown
## Output Requirements

ONLY return valid JSON. Do NOT include:
- Markdown code blocks
- Preamble ("Here is...")
- Explanation after the JSON

If error: `{"ok": false, "reason": "..."}`
```

---

## Claude-Specific Patterns

**Default: Tell what to do, not what not to do:**
```
# Less effective
"Do not use markdown"

# More effective
"Use smoothly flowing prose paragraphs."
```

**Exception: Safety constraints use explicit prohibitions.** `NEVER` and `STRICTLY PROHIBITED` lists are standard for safety-critical rules (destructive ops, read-only enforcement, scope boundaries). Positive framing is the default; negation is the tool for hard constraints.

**Inline rationale — weave "why" into the rule itself:**
Don't separate rationale into a different section. Embed it using because/as/since clauses or em-dashes:
```
# Separated (weaker — rationale may not be read with the rule)
Rule: Prefer editing existing files.
Reason: Prevents file bloat.

# Inline (stronger — understanding comes at moment of reading)
Prefer editing an existing file to creating a new one, as this prevents file bloat
and builds on existing work more effectively.
```

For behavioral rules, explain **what goes wrong** if violated — not the technical architecture behind the rule:
```
# Architecture-focused (less motivating)
Memory is in-context only due to autoregressive generation.

# Consequence-focused (more motivating)
Without these memories, you will repeat the same mistakes and the user
will have to correct you over and over.
```

**Provide motivation with consequence:**
```
# Less effective
NEVER use ellipses

# More effective
Your response will be read aloud by text-to-speech,
so never use ellipses since TTS can't pronounce them.
```

### Prohibition Hierarchy

Use graduated prohibition strength — each tier has different escape-clause semantics:

| Tier | Keyword | Meaning | Escape Clause |
|------|---------|---------|---------------|
| 1 | `NEVER` | Absolute prohibition | None — no override possible |
| 2 | `MUST NOT` | Strong, contextual | Override only by explicit supersession |
| 3 | `Do NOT` / `Do not` | Standard prohibition | Override by explicit user request |
| 4 | `Avoid` | Soft preference | Override when alternatives don't work |

Don't use `NEVER` for style preferences — it dilutes the signal. Reserve it for things that would cause functional failures or safety violations.

### Note / Important / Critical — Semantic Roles

These are three distinct instruction types, not three emphasis levels:

| Marker | Semantic Role | Use For |
|--------|--------------|---------|
| `Note:` | Informational context | Background the model should know but not relay to the user |
| `IMPORTANT:` | Behavioral constraint | Modifies a default action the model would otherwise take |
| `CRITICAL:` | Override | Supersedes other instructions; non-negotiable |

**Formula:** `CRITICAL:` + specific prohibition + consequence = effective hard constraint.

**Counter over-engineering (Opus 4.5):**
```
- Only make changes directly requested
- Keep solutions simple and focused
- Don't add features beyond what was asked
- Don't add error handling for impossible scenarios
```

---

## Calibration Checklist

Before finalizing skill/agent instructions:

- [ ] All quantities explicit (counts, limits, ranges)
- [ ] All conditions enumerable (if X then Y)
- [ ] Success criteria measurable
- [ ] Examples demonstrate exact desired behavior
- [ ] No time language ("quickly", "take your time")
- [ ] No effort language ("thorough", "careful", "diligent")
- [ ] No significance language ("important") without criteria
- [ ] No threshold language ("enough", "sufficient") without numbers
- [ ] Multi-step tasks have explicit checkpoints
- [ ] State that needs persistence is externalized

---

## Quick Reference

**Measurable properties you CAN specify:**
- Token/word counts, line limits
- Explicit enumerations ("check X, Y, Z")
- Concrete examples (input → output pairs)
- Decision trees ("if A then B, else C")
- Output schemas (JSON, XML structures)
- Stop conditions ("stop when all tests pass")
- Priority orderings ("1. correctness 2. performance")
- Exclusion lists ("do NOT include: boilerplate, comments")

**Formula:**
```
Effective Instruction =
    Explicit Success Criteria
  + Enumerated Constraints
  + Concrete Examples
  + Structured Output Format
  + Externalized State (for multi-step)
```

---

## Examples from This Project

```yaml
# Well-calibrated instructions
- "15-20+ sources (standard), 20-30+ (exhaustive)"
- ">85% FActScore"
- "2+ sources for key claims"
- "Only assert at ≥75 confidence"
- "5+ triggers in description"
- "<300 lines target, 500 max"
- ">5 steps or >3 failed attempts"
```

---

**Deep dive:** `.claude/meta/llm-constraints.md` (architectural limitations + why subjective instructions fail)

**Also see:** `.claude/meta/authoring-patterns.md` (practical patterns for skills/agents)

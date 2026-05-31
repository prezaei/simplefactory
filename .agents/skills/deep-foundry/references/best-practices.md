# Best Practices Guide

Consolidated guidance for writing effective skills and agents.

---

## Description Engineering

### The Critical Formula

```yaml
description: [Capabilities]. [Triggers].
```

Descriptions determine triggering. Default activation: ~20-50%. With good descriptions: >80%.

### Checklist

| Criterion | Requirement | Example |
|-----------|-------------|---------|
| Voice | Third-person | "Processes files..." NOT "I help..." |
| Structure | Capabilities + triggers | "[What] + [When]" |
| Triggers | 5+ quoted phrases | "create", "build", "fix" |
| Verbs | Action verbs | create, build, configure, fix |
| Length | 100-400 chars | Comprehensive but focused |

### Good Example

<example>
```yaml
description: |
  Extract text and tables from PDF files, fill forms, merge documents.
  Use when user asks to "create a PDF", "merge PDFs", "extract text",
  "fill PDF form", "convert PDF to image", or works with PDF files.
```
</example>

### Bad Examples

Embed the failure reason in the label itself — the parenthetical IS the explanation:

<bad-example>
```yaml
description: Helps with documents         # Bad (vague): no specific triggers to match
description: Use when working with PDFs   # Bad (no capabilities): only says when, not what
description: I can help process PDFs      # Bad (first-person): conflicts with system prompt voice
```
</bad-example>

### Trigger Phrase Patterns

**Action verbs:**
- Primary: "create", "build", "make", "add", "configure", "fix"
- Secondary: "write", "develop", "improve", "refactor", "optimize"
- Maintenance: "update", "migrate", "convert", "validate"

**Question forms:**
- "how to [action]", "what is [concept]", "when to use [pattern]"

**Domain keywords:**
- Technology names, file extensions, domain terms

---

## Content Writing

### Voice

**Skills** — imperative:

| Correct | Wrong |
|---------|-------|
| Create the file | You should create |
| Configure settings | You need to configure |

**Agents** — two-phase sequence within the same document:

1. **Establish role** in second-person: `"You are a verification specialist."`
2. **Issue commands** in imperative: `"Start from the assumption that bugs exist and go find them."`

The transition happens naturally — no section break needed. Optionally, follow the role with an **anti-role** to set boundaries: `"Your job is not to confirm the implementation works — it's to try to break it."`

### Structure

```markdown
# Skill Name

## Overview
[1-2 sentences]

## When to Use
- [Scenario 1]
- [Scenario 2]
- [Scenario 3]

## When NOT to Use
- [Anti-scenario 1] - [why/alternative]
- [Anti-scenario 2] - [why/alternative]

## Quick Start
[Minimal example]

## Core Instructions
[Main workflow]

## Examples
[2-3 input/output pairs]

## Additional Resources
[Links to references/]
```

**Dual Sections:** "When to Use / When NOT to Use" reduces false positives. The "When NOT to Use" section prevents triggering on edge cases and suggests alternatives.

### Code Examples

**CORRECT - Complete and runnable:**
<example>
```python
# Good - complete with imports
from pypdf import PdfReader

reader = PdfReader("document.pdf")
text = ""
for page in reader.pages:
    text += page.extract_text()
print(f"Extracted {len(text)} characters")
```
</example>

**WRONG** (Never do this):
<bad-example>
```python
# Bad - cryptic names, no imports
r = PdfReader("x.pdf")
t = ""
for p in r.pages:
    t += p.extract_text()
```
**Why this fails:** Missing imports, cryptic variable names, no context for what it does.
</bad-example>

### Inline Constraint Pattern

For single-sentence prohibitions, use "Don't X — do Y instead":

```markdown
Don't retry failing commands in a sleep loop — diagnose the root cause or consider an alternative approach.
```

This is denser than a comparison table and works well for behavioral rules in skill body content.

### WRONG/CORRECT Comparison Pattern

For configuration or behavior choices, use explicit side-by-side comparison with **parenthetical annotations** explaining what makes each wrong/right:

```markdown
**WRONG** (replaces existing permissions):
```json
{ "permissions": { "allow": ["new"] } }
```

**CORRECT** (preserves existing + adds new):
```json
{ "permissions": { "allow": ["existing", "new"] } }
```
```

The parenthetical `(replaces existing)` / `(preserves existing + adds)` makes the semantic difference instantly visible.

---

## Progressive Disclosure

### Three Levels

| Level | When | Cost | Content |
|-------|------|------|---------|
| Metadata | Startup | ~50-100 tokens | name, description |
| SKILL.md | Triggered | <5,000 tokens | Core instructions |
| References | On-demand | Varies | Deep content |

**Additional token facts:**
- Per-turn overhead: ~1,500 tokens (system prompts, tool definitions)
- For files >100 lines, consider adding a Table of Contents

### What Goes Where

| SKILL.md | references/ |
|----------|-------------|
| Overview, quick start | Detailed patterns |
| Core workflow (2-3 examples) | Extended examples |
| Navigation to refs | Advanced topics |
| <300 lines ideal | Troubleshooting |

### Scripts

Execute scripts, don't read them:
```markdown
# Good - execute script, only stdout enters context
!`python scripts/analyze.py input.pdf`

# Bad - entire script source loaded into context
See scripts/analyze.py for the algorithm.
```

---

## Anti-Patterns

### Critical (Breaks Functionality)

| Anti-Pattern | Why Fails | Fix |
|--------------|-----------|-----|
| Vague description | Can't match requests | Add 5+ specific triggers |
| Wrong voice | System prompt conflict | Skills: third-person desc + imperative body. Agents: second-person ("You are...") |
| Missing frontmatter | Not recognized as skill | Add YAML frontmatter |
| Invalid YAML | Parser fails | Use spaces, validate syntax |

### High Impact (Degrades Quality)

| Anti-Pattern | Why Fails | Fix |
|--------------|-----------|-----|
| Kitchen sink skill | Triggers inappropriately | One skill = one domain |
| Bloated SKILL.md | Context waste | Move details to references/ |
| Missing examples | Generic output | Add 2-3 input/output pairs |
| Overlapping descriptions | Claude confused | Differentiate by keywords |

### Medium Impact (Reduces Effectiveness)

| Anti-Pattern | Why Fails | Fix |
|--------------|-----------|-----|
| Nested references | Info buried too deep | One level deep max |
| Windows paths | Breaks cross-platform | Use forward slashes |
| Missing invocation control | Dangerous auto-trigger | `disable-model-invocation: true` |
| Over-explaining | Wastes tokens | Assume Claude is smart |

---

## Example Ordering

- Most relevant example last (leverage recency bias)
- Progress simple → complex (build understanding)
- Avoid clustering similar examples together

## Example Selection

| Task Type | Strategy |
|-----------|----------|
| Easy/in-domain | Similarity-based selection |
| Hard/out-of-domain | Diversity-based selection |
| Complex | Combined (diverse but relevant) |

---

## Instruction Calibration

Models lack temporal sense, calibrated importance, and effort gradients. Subjective instructions fail.

### Quick Rules

| Never | Always |
|-------|--------|
| "Be thorough" | "Include: [list]" |
| "Be careful" | "Check for: [list]" |
| "When appropriate" | "When [condition]" |
| "Enough" | "[N]+" |
| "Important" | "Prioritize: 1) X 2) Y" |

### Pattern

```
# Bad
Be thorough when reviewing.

# Good
Review against:
1. Memory: bounds, lifetime
2. Concurrency: races, deadlocks
Report: [criterion]: [PASS | finding]
```

**Full reference:** [calibration.md](calibration.md)

---

## Agent System Prompts

### Voice & Description

Agent body uses **second-person**: `"You are a [domain] specialist."`

Agent `description` targets the **orchestrating agent** with scenarios, not user trigger phrases:
```yaml
# Skill description (targets user):
description: |
  Extracts text from PDFs. Use when user asks to "create a PDF", "merge PDFs"...

# Agent description (targets orchestrator):
description: |
  PDF processing specialist. Use this agent when documents need extraction,
  merging, or form filling requiring isolated execution.
```

### Structure

```markdown
You are a [domain] specialist.

## Strengths
- [What this agent does well]

## When Invoked
1. [First action]
2. [Second action]
3. [Third action]

## Quality Standards
- [Standard 1]

## Output Format
[Expected structure]
```

### Subagent Prompt Strategies

**Context-inheriting** (omit `subagent_type`): Agent has your full conversation. Write a directive — don't re-explain background.

**Fresh subagent** (with `subagent_type`): Agent starts with zero context. Brief it like a colleague who just walked in — explain what you're accomplishing, what you've learned, and what you've ruled out. Terse prompts produce shallow work.

**Cardinal rule: Never delegate understanding.** Don't write "based on your findings, fix the bug." Write prompts that prove you understood: include file paths, line numbers, what specifically to change.

### Best Practices

- Define role in second-person on first line
- Specify workflow steps with success criteria per step
- Set quality standards with measurable thresholds
- Define output format (use `file_path:line_number` for code references)
- Handle edge cases
- Include output efficiency: lead with answer, no filler, no inner monologue
- Absolute file paths only (agent cwd resets between bash calls)
- Include domain-specific memory instructions: "Update your agent memory as you discover [domain items]"
- **Self-update pattern:** If the agent detects its own instructions are stale (not a task failure), ask the user to confirm, then apply a minimal targeted fix to the agent file

---

## Quality Gates

### Pre-Publication Checklist

**Frontmatter:**
- [ ] Valid YAML, starts line 1
- [ ] `name`: kebab-case, ≤64 chars
- [ ] `description`: third-person, 5+ triggers, ≤1024 chars

**Content:**
- [ ] Body <500 lines
- [ ] Imperative style throughout
- [ ] Code examples complete and runnable
- [ ] Forward slashes in all paths

**Structure:**
- [ ] References one level deep
- [ ] All referenced files exist
- [ ] No broken links

**Functionality:**
- [ ] Triggers on expected queries
- [ ] No false positives
- [ ] Dangerous ops require manual invocation

---

## Activation Improvement

If triggering unreliable, add forced eval hook:

```yaml
hooks:
  PreToolUse:
    - hooks:
        - type: command
          command: "echo 'Evaluate each skill with YES/NO before proceeding. Then use any skill marked YES.'"
          once: true
```

Achieves ~84% activation vs ~20-50% baseline.

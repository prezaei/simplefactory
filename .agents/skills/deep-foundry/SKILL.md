---
name: deep-foundry
user-invocable: false
description: |
  Creates agent skills and agents with proper frontmatter, descriptions, and structure.
  Use when user asks to "create a skill", "make a skill", "build a skill", "write a skill",
  "develop a skill", "skill structure", "frontmatter format", "trigger phrases",
  "progressive disclosure", "skill best practices", "description triggers",
  "write agent", "create agent", "build agent", or needs skill/agent authoring guidance.
---

# Deep Foundry

Create effective agent skills and agents.

## Navigation

| Topic | Location |
|-------|----------|
| Frontmatter fields | [Skill Frontmatter](#skill-frontmatter) |
| Writing descriptions | [Description Formula](#description-formula) |
| Content guidelines | [Content Structure](#content-structure) |
| Objective instructions | [Instruction Calibration](#instruction-calibration) |
| Ready templates | [references/templates.md](references/templates.md) |
| Full specification | [references/specification.md](references/specification.md) |
| Best practices | [references/best-practices.md](references/best-practices.md) |
| Calibration patterns | [references/calibration.md](references/calibration.md) |
| Reasoning techniques | [references/reasoning-techniques.md](references/reasoning-techniques.md) |

---

## Overview

Skills are modular packages extending agent capabilities, loaded on-demand to preserve context. They follow the Agent Skills open standard and work across Claude Code, GitHub Copilot, Codex, Cursor, and other compatible agents.

| Skill | Agent |
|-------|-------|
| Adds to main context | Isolated context window |
| Reusable patterns | Discrete tasks |
| Dynamic loading | Parallel execution |

**Key facts:**
- Triggering: pure LLM reasoning (no regex, no embeddings)
- Default activation: ~20-50%; with good descriptions: >80%
- Cost: ~50-100 tokens metadata, <5,000 tokens when loaded

---

## Quick Start

```
.agents/skills/my-skill/
├── SKILL.md              # Required: frontmatter + body
├── references/           # Optional: deep content
└── scripts/              # Optional: executables
```

1. Create directory: `.agents/skills/my-skill/` (source of truth)
2. Create `SKILL.md` with frontmatter + body
3. Test: `/my-skill` or auto-trigger

**Provider-specific discovery paths** (all equivalent under the open standard): `.agents/skills/`, `.claude/skills/`, `.github/skills/`. This repo uses `.agents/skills/` as the source of truth and symlinks other provider paths to it.

---

## When to Use
- Creating a new skill or agent from scratch
- Structuring frontmatter, descriptions, or trigger phrases
- Applying progressive disclosure to skill content
- Writing calibrated, objective instructions

## When NOT to Use
- Runtime reasoning or confidence calibration — use **deep-reasoning**

---

## Skill Frontmatter

### Required Fields

```yaml
---
name: skill-identifier
description: |
  [Capabilities]. Use when user asks to "[trigger1]", "[trigger2]",
  "[trigger3]", "[trigger4]", "[trigger5]", or needs [category] guidance.
---
```

| Field | Limit | Notes |
|-------|-------|-------|
| `name` | 64 chars | lowercase, hyphens, no "anthropic"/"claude" |
| `description` | 1024 chars | third-person, 5+ triggers |

### Optional Fields

| Field | Purpose | When to use |
|-------|---------|-------------|
| `when_to_use` | Detailed auto-invocation trigger with phrases | Separate from `description`; starts with "Use when..." |
| `allowed-tools` | Restrict to minimum permissions: `Bash(gh:*)` | Skill should be read-only or limited |
| `disable-model-invocation` | Manual-only (`true`) | Skill should never auto-trigger |
| `context: fork` | Runs in separate context (no prior conversation) | Skill output shouldn't leak into main context |
| `agent` | Delegate to subagent: `Explore`, `Plan` | Task needs isolated execution or planning |

**Full spec:** [references/specification.md](references/specification.md)

---

## Description Formula

**The description determines activation.** Use this formula:

```yaml
description: [Capabilities]. Use when user asks to "[triggers]".
```

| Criterion | Requirement |
|-----------|-------------|
| Voice | Third-person: "Processes..." NOT "I help..." |
| Content | Capabilities + when to use |
| Triggers | 5+ quoted phrases |
| Length | 100-400 chars |

### Example

```yaml
description: |
  Extracts text from PDF files, fills forms, merges documents.
  Use when user asks to "create a PDF", "merge PDFs", "extract text",
  "fill PDF form", or works with PDF files.
```

### Anti-Patterns

```yaml
description: Helps with documents         # Vague
description: Use when working with PDFs   # No capabilities
description: I can help process PDFs      # First-person
```

**Detailed guidance:** [references/best-practices.md](references/best-practices.md)

---

## Content Structure

### Size Limits

| Metric | Target | Maximum |
|--------|--------|---------|
| Lines | <300 | 500 |
| Tokens | <2,000 | 5,000 |

### Template

```markdown
# Skill Name

## Overview
[1-2 sentences]

## When to Use
- [Scenario triggering this skill]

## When NOT to Use
- [Anti-scenario] - [alternative]

## Quick Start
[1 working example]

## Core Instructions
[Main workflow as numbered steps — each step needs **Success criteria**]

## Examples
[2-3 input/output pairs]

## Additional Resources
[Links to references/]
```

**Note:** "When NOT to Use" reduces false positives by specifying boundaries.

### Writing Style

**Skills** — imperative form:
- "Create the file" NOT "You should create"
- "Configure settings" NOT "You need to configure"

**Agents** — two-phase sequence within one document:
1. Establish role in second-person: `"You are a verification specialist."`
2. Then issue commands in imperative: `"Start from the assumption that bugs exist."`

Optionally add an anti-role boundary: `"Your job is not to confirm it works — it's to try to break it."`

---

## Instruction Calibration

Models lack temporal sense, calibrated importance, and effort gradients. **Use objective criteria.**

| Subjective (Fails) | Objective (Works) |
|--------------------|-------------------|
| "Be thorough" | "Include: [enumerated list]" |
| "Be careful" | "Check for: [enumerated list]" |
| "Be brief" | "Maximum [N] sentences" |
| "When appropriate" | "When [explicit condition]" |
| "Enough X" | "[N]+ X" |

**Inline emphasis** (default for skills): `**IMPORTANT:** Never use git commands with the -i flag` — keyword + specific prohibition in a sentence.

**Banner emphasis** (agents only — for mode constraints like read-only enforcement):
- `=== CRITICAL: [NAME] ===` banner for read-only modes, safety constraints
- `BLOCKING REQUIREMENT` for prerequisites
- `STRICTLY PROHIBITED` list for enumerated prohibitions

**Pattern:** Structure instructions as checklists with pass/fail criteria:

Review against:
1. Triggers: ≥5 quoted phrases → Triggers: PASS or "3 found, need 2 more"
2. Voice: third-person in description → Voice: PASS or "First-person at line 4"

**Deep dive:** [references/calibration.md](references/calibration.md)
**Also see:** [references/specification.md](references/specification.md)

---

## Progressive Disclosure

**Three levels minimize token usage:**

| Level | When | Cost |
|-------|------|------|
| Metadata | Startup | ~50-100/skill |
| SKILL.md | Triggered | <5,000 |
| References | On-demand | Varies |

**Compaction resilience:** Skills are re-injected after context compaction. Instructions must be self-contained — don't depend on conversation state that may be summarized away.

### Content Distribution

| SKILL.md | references/ |
|----------|-------------|
| Overview, quick start | Detailed patterns |
| Core workflow | Advanced topics |
| 2-3 examples | Extended examples |
| Navigation | Troubleshooting |

---

## Agent Structure

Agents provide isolated execution contexts. Use second-person voice ("You are...").

### Frontmatter

```yaml
---
name: my-agent
description: |
  [Capability]. Use this agent when [scenario requiring this specialization].
tools: ["Read", "Grep", "Glob"]
model: sonnet
---

You are a [domain] specialist.

## When Invoked
[Workflow steps]

## Output Format
[Structure]
```

**Agent vs. skill descriptions:** Skill descriptions target the user (quoted trigger phrases). Agent descriptions target the *orchestrating agent* (scenario-based: "Use this agent when you need to...").

### Built-in Types

| Type | Purpose |
|------|---------|
| `Explore` | Fast codebase search (Haiku, read-only) |
| `Plan` | Design implementation plans |
| `general-purpose` | Multi-step tasks |

---

## Validation Checklist

**Frontmatter:**
- [ ] `name`: kebab-case, ≤64 chars
- [ ] `description`: third-person, 5+ triggers, ≤1024 chars

**Content:**
- [ ] Body <500 lines
- [ ] Imperative style
- [ ] Code examples complete

**Calibration:**
- [ ] No vague qualifiers ("thorough", "careful", "important")
- [ ] All thresholds explicit (counts, ranges)
- [ ] Conditions enumerable (if X then Y)

**Structure:**
- [ ] References one level deep
- [ ] No broken links
- [ ] Forward slashes in paths

**Full anti-patterns catalog:** [references/best-practices.md](references/best-practices.md)

---

## Additional Resources

- [references/specification.md](references/specification.md) - Complete field documentation
- [references/best-practices.md](references/best-practices.md) - Writing guidance, anti-patterns
- [references/templates.md](references/templates.md) - Ready-to-use templates
- [references/calibration.md](references/calibration.md) - Objective instruction patterns
- [references/reasoning-techniques.md](references/reasoning-techniques.md) - Verified reasoning patterns

---

**Keywords:** skill-creation, skill-development, create-skill, build-skill, frontmatter, description-triggers, progressive-disclosure, trigger-phrases, agent-development, create-agent, build-agent, deep-foundry

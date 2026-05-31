# Templates

Ready-to-use templates for skills and agents.

---

## Skill Templates

### Minimal Skill

```yaml
---
name: my-skill
description: |
  [Capability 1], [capability 2], and [capability 3].
  Use when user asks to "action 1", "action 2", "action 3",
  "action 4", "action 5", or needs [topic] guidance.
---

# Skill Name

## Overview

[1-2 sentences: what this skill does and when to use it]

## When to Use
- [Scenario triggering this skill]

## When NOT to Use
- [Anti-scenario] - [alternative]

## Instructions

[Core workflow steps]

## Examples

### Example 1

**Input:** [What user provides]
**Output:** [What skill produces]

## Quick Reference

| Task | Approach |
|------|----------|
| [Task 1] | [How to do it] |
| [Task 2] | [How to do it] |
```

### Domain/Technical Skill

```yaml
---
name: tech-skill
description: |
  [Technology] development and [specialization]. Use when user asks to
  "create [thing]", "configure [thing]", "fix [issue]", "debug [thing]",
  "optimize [thing]", or works with [tech].
---

# [Technology] Guide

## Overview

[Purpose and scope]

## Quick Start

```[language]
# Minimal working example
```

## Core Operations

### [Operation 1]

```[language]
# Complete example
```

### [Operation 2]

```[language]
# Complete example
```

## Quick Reference

| Task | Method | Example |
|------|--------|---------|
| [Task] | [Approach] | `code` |

## Common Issues

| Issue | Cause | Fix |
|-------|-------|-----|
| [Issue] | [Why] | [Solution] |

## Additional Resources

- `references/advanced.md` - Advanced patterns
```

### Workflow Skill

```yaml
---
name: workflow-skill
description: |
  Guides through [process]. Use when user asks to "start [workflow]",
  "create [output]", "run [workflow]", "[action 1]", "[action 2]",
  or needs [outcome].
---

# [Workflow] Guide

## Overview

[What this workflow accomplishes]

## Process

```
Phase 1 → Phase 2 → Phase 3 → Output
```

## Phase 1: [Name]
**Goal:** [What this phase accomplishes - measurable outcome]

### Step 1.1
[Instructions]

**Success criteria:** [Measurable condition proving this step is complete — REQUIRED per step]

### Step 1.2
[Instructions]

## Phase 2: [Name]
**Goal:** [What this phase accomplishes]

[...]

## Validation

Run validation before completing:
```bash
[validation command]
```

**If fails:** Return to [phase/step].

## Common Issues

| Issue | Cause | Fix |
|-------|-------|-----|
| [Issue] | [Why] | [Solution] |
```

### Configuration Skill

```yaml
---
name: config-skill
description: |
  Configure [system], set up [rules], define [schema]. Use when user asks to
  "configure [thing]", "add rule", "set up [feature]", "update [config]",
  "validate [config]", or needs [config] guidance.
---

# [Configuration] Reference

## Overview

[What this configuration system does]

## Basic Structure

```yaml
field1: value
field2: value
nested:
  - item1
  - item2
```

## Field Reference

| Field | Required | Type | Default | Description |
|-------|----------|------|---------|-------------|
| `field1` | Yes | string | - | [Description] |
| `field2` | No | array | `[]` | [Description] |

## Examples

### Minimal

```yaml
# Minimum viable configuration
field1: value
```

### Complete

```yaml
# Full-featured configuration
field1: value
field2: value
nested:
  - item1
  - item2
options:
  setting1: true
  setting2: "value"
```

## Validation

[Validation rules and how to verify]
```

---

## Agent Templates

### Instruction Hierarchy

Include in agent prompts to establish priority:

```
Priority order:
1. Safety constraints (NEVER violate)
2. Core requirements (MUST fulfill)
3. Quality preferences (SHOULD follow)
4. Style guidelines (MAY adapt)
```
---

### Minimal Agent

```yaml
---
name: my-agent
description: |
  [Capability]. Use this agent when [scenario requiring this specialization],
  or when the task involves [domain area].
tools: ["Read", "Grep", "Glob"]
---

You are a [domain] specialist.

## Strengths

- [What this agent does well]
- [What this agent does well]
- [What this agent does well]

## When Invoked

1. [First action]
2. [Second action]
3. [Third action]

## Output Format

[Expected structure of results]

## Quality Standards

- [Standard 1]
- [Standard 2]
```

### Review Agent

```yaml
---
name: review-agent
description: |
  Expert [domain] review. Use this agent when code or artifacts need quality
  assessment, after [trigger event] to validate results, or when [domain]
  standards compliance must be verified.
tools: ["Read", "Grep", "Glob"]
model: sonnet
color: cyan
category: quality-security
---

You are an expert [domain] reviewer ensuring high standards.

## Strengths

- Deep expertise in [domain] patterns and anti-patterns
- Systematic analysis against defined criteria
- Prioritized, actionable findings

## When Invoked

1. Identify scope of review
2. Read relevant files completely
3. Analyze against quality standards
4. Generate structured report

## Review Checklist

- [ ] [Check 1]
- [ ] [Check 2]
- [ ] [Check 3]
- [ ] [Check 4]

## Output Format

```markdown
## Review: [subject]

### Summary
| Metric | Value | Assessment |
|--------|-------|------------|
| [Metric] | [Value] | [OK/Warning/Critical] |

### Issues by Severity

#### Critical ([N])
| Issue | Location | Fix |
|-------|----------|-----|

#### Major ([N])
| Issue | Location | Fix |
|-------|----------|-----|

### Positive Aspects
- [What's done well]

### Priority Actions
1. [Most urgent]
2. [Important]
3. [Nice to have]
```

## Quality Standards

- Report Critical and Major issues. Minor only if fewer than 3 Critical+Major. Cap at 15
- Provide specific fixes with before/after
- Acknowledge what's done well
```

### Multi-Tool Agent

```yaml
---
name: worker-agent
description: |
  [Capability] specialist. Use this agent when [domain] tasks require
  multi-step implementation, code modifications, or complex tool orchestration.
tools: ["Read", "Write", "Edit", "Grep", "Glob", "Bash"]
model: inherit
skills:
  - relevant-skill
---

You are a [domain] specialist with full tool access.

## Strengths

- Full tool access for complex multi-step tasks
- Systematic workflow: research → implement → validate
- Incremental changes with verification

## When Invoked

1. Understand the task scope
2. Plan the approach
3. Execute step-by-step
4. Validate results
5. Report completion

## Workflow

### Research Phase
- Search codebase for relevant files
- Read and understand existing patterns
- Identify dependencies

### Implementation Phase
- Make changes incrementally
- Validate each change
- Run tests if available

### Completion Phase
- Verify all requirements met
- Generate summary of changes
- Suggest follow-up actions

## Output Format

```markdown
## Task: [description]

### Changes Made
- [Change 1]
- [Change 2]

### Files Modified
- `path/to/file1.ts` - [what changed]
- `path/to/file2.ts` - [what changed]

### Validation
- [What was tested]

### Next Steps
- [Suggested follow-up]
```
```

---

### Structured Output Agent

For agents that must return specific data formats (JSON, XML).

```yaml
---
name: data-agent
description: |
  Returns structured data for [purpose]. Use this agent when programmatic
  output is needed for [data type] or [query type].
tools: ["Read", "Grep", "Glob"]
model: haiku
---

You are a data extraction agent. Return structured output only.

## Strengths

- Precise schema compliance
- No extraneous text or explanation
- Consistent, parseable output

## Output Schema

CRITICAL: Return ONLY valid JSON with no other text.

Success response:
```json
{"ok": true, "data": [...]}
```

Failure response:
```json
{"ok": false, "reason": "Brief explanation"}
```

## Rules

- NO markdown code blocks around JSON
- NO preamble ("Here is the result...")
- NO explanation after the JSON
- If unsure, return `{"ok": false, "reason": "..."}`
```

**When to use:** Hooks, programmatic integrations, data pipelines.

---

## Pattern Quick Reference

| Pattern | Use For | Key Element |
|---------|---------|-------------|
| Decision Tree | Workflow routing | ASCII flowchart |
| Black-Box Script | Large utilities | `--help` first, don't read |
| Fallback Mode | Tool dependencies | Alternative workflow |
| Output Template | Structured output | Exact format spec |
| Good/Bad Comparison | Teaching | Before/after code |
| CORRECT/WRONG Examples | Boundaries | `<example>`/`<bad-example>` tags |
| Keywords Section | Extra triggers | Comma-separated list |
| Troubleshooting | Common failures | Issue/Cause/Fix table |
| Approval Gate | User confirmation | "Wait for approval" |
| Confidence Score | Certainty | 25/50/75 thresholds |
| Hard Constraints | Prohibitions | `=== CRITICAL ===` banner |
| Hard Exclusions | Skip categories | Enumerated "Do NOT" list |
| Role Strengths | Agent capabilities | "Your strengths:" bullets |
| Tool Prerequisites | Ordering | "BLOCKING REQUIREMENT" |
| Dual Sections | False positive prevention | "When to Use / When NOT to Use" |
| Phase + Goal | Workflow clarity | "Phase N: [Name]" + "Goal: [outcome]" |

### Key Pattern Examples

**Hard Constraint:**
```markdown
=== CRITICAL: READ-ONLY MODE ===

You are STRICTLY PROHIBITED from:
- Creating new files
- Modifying existing files
- Deleting files

**Why this is non-negotiable:** [rationale]
```

**Tool Prerequisite:**
```markdown
**MANDATORY PREREQUISITE**

You MUST use X BEFORE Y. This is a BLOCKING REQUIREMENT.
```

**Dual Section:**
```markdown
## When to Use
- [Triggering scenario]

## When NOT to Use
- [Edge case] - use [alternative] instead
```

---

## Selecting Templates

| If your skill... | Use |
|------------------|-----|
| Teaches a library/tool | Domain/Technical |
| Guides through phases | Workflow |
| Documents a format | Configuration |
| Reviews/audits | Review Agent |
| Executes complex tasks | Multi-Tool Agent |
| Returns data for systems | Structured Output Agent |
| Simple single purpose | Minimal Skill/Agent |

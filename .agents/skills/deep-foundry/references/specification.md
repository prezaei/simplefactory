# Skill & Agent Specification

Complete technical specification for Claude Code skills and agents.

**Sources:** [code.claude.com/docs/en/skills](https://code.claude.com/docs/en/skills) | [code.claude.com/docs/en/sub-agents](https://code.claude.com/docs/en/sub-agents) | [platform.claude.com/docs/en/agents-and-tools/agent-skills/overview](https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview)

---

## Skill Frontmatter

### Fields

| Field | Required | Type | Default | Validation/Purpose |
|-------|----------|------|---------|-------------------|
| `name` | No | string | directory name | Max 64 chars; `^[a-z0-9-]+$`; no "anthropic"/"claude" |
| `description` | Recommended | string | first paragraph | Max 1024 chars; third-person; no XML tags |
| `argument-hint` | No | string | - | Hint shown in autocomplete (e.g., `[issue-number]`) |
| `disable-model-invocation` | No | boolean | `false` | Prevent Claude auto-invocation |
| `user-invocable` | No | boolean | `true` | Show in `/` menu |
| `allowed-tools` | No | string | All | Comma-separated or filter syntax |
| `model` | No | string | default | `sonnet`, `opus`, `haiku` |
| `when_to_use` | No | string | - | Detailed auto-invocation trigger (start with "Use when..."; include trigger phrases) |
| `context` | No | enum | inline | `fork` for isolated subagent |
| `agent` | No | string | `general-purpose` | Subagent type when `context: fork` |
| `hooks` | No | object | - | Scoped lifecycle hooks |

**Note:** If `description` omitted, defaults to first markdown paragraph.

**Note:** `implicit: true` is a convention, not a native Claude Code field. It has no effect without a SessionStart hook that reads it and loads the skill. See `.claude/settings.json` hooks.

**Note:** `version` and `license` are not native Claude Code fields. Some skills in this repo carry them because they were ported from upstream sources that use semantic-versioning and licensing conventions (e.g., security/privacy skills from external libraries). Internally-authored skills don't need them. If you add them, keep the format consistent: `version: "1.0.0"` (string, quoted), `license: MIT` (SPDX identifier).

**Note:** YAML `description` can be written three ways — inline (`description: "one line"`), folded block (`description: >` — newlines become spaces), or literal block (`description: |` — newlines preserved). **Prefer folded (`>`) for multiline descriptions** so the rendered value stays a single paragraph, matching how Claude Code uses the field for routing. Reserve `|` for the rare case where embedded newlines matter.

### Tool Specification

```yaml
# Comma-separated
allowed-tools: Read, Grep, Glob

# Prefix filter
allowed-tools: Bash(python:*), Bash(gh:*)

# Wildcard
allowed-tools: Bash(*)

# Skill invocation control
allowed-tools: Skill(deploy), Skill(review-pr:*)
```

**Rules:**
- Case-sensitive tool names
- No trailing commas
- Formats: exact match, prefix filter (`Tool(prefix:*)`), wildcard (`Tool(*)`)

### String Substitutions

| Pattern | Example | Description |
|---------|---------|-------------|
| `$ARGUMENTS` | `$ARGUMENTS` | All arguments passed when invoking; appended if absent |
| `${VAR}` | `${CLAUDE_SESSION_ID}` | Variable substitution (session ID, env vars) |
| `${FN()}` | `${GET_USER_TYPE()}` | Function call returning dynamic value |
| `${cond?`if`:`else`}` | `${IS_PRO()?`Use opus`:`Use haiku`}` | Conditional content based on runtime check |

**Note:** Template variables are processed at load time, not runtime. Use for feature flags, dynamic content injection, and environment-specific behavior.

### Dynamic Context Injection

```markdown
- PR diff: !`gh pr diff`
- PR comments: !`gh pr view --comments`
```

Commands in backticks execute; output replaces placeholder.

### Hooks (Skill Frontmatter)

Supported events: `PreToolUse`, `PostToolUse`, `Stop`

```yaml
hooks:
  PreToolUse:
    - matcher: "Bash"
      hooks:
        - type: command
          command: "./scripts/validate.sh"
          timeout: 60
  PostToolUse:
    - matcher: "Edit|Write"
      hooks:
        - type: command
          command: "./scripts/lint.sh"
  Stop:
    - hooks:
        - type: command
          command: "./scripts/cleanup.sh"
  once: true  # Run hooks once per invocation (place at hook-entry level, not inside hooks array)
```

### Extended Thinking

Include `ultrathink` anywhere in skill content to enable extended thinking mode.

### Hard Constraint Patterns

**Inline emphasis** (default for skills — embed constraints in prose):

| Pattern | Use Case | Example |
|---------|----------|---------|
| `**IMPORTANT:**` | Behavioral constraint modifying a default | `**IMPORTANT:** Never use git commands with the -i flag` |
| `**CRITICAL:**` | Override that supersedes other instructions | `**CRITICAL:** Always create NEW commits rather than amending` |
| `NEVER` / `MUST` / `Do NOT` | Prohibition/requirement within sentences | `NEVER skip hooks — the commit may silently corrupt history` |

**Banner emphasis** (agents only — for mode constraints like read-only enforcement):

| Pattern | Use Case | Example |
|---------|----------|---------|
| `=== CRITICAL ===` | Read-only modes, safety constraints | `=== CRITICAL: READ-ONLY MODE ===` |
| `BLOCKING REQUIREMENT` | Prerequisites, ordering | `This is a BLOCKING REQUIREMENT - must X before Y` |
| `STRICTLY PROHIBITED` | Enumerated prohibitions | `You are STRICTLY PROHIBITED from: [list]` |

**Banner format (agents):**
```markdown
=== CRITICAL: [CONSTRAINT NAME] ===

You are STRICTLY PROHIBITED from:
- [Action 1] (no [specific tool/command])
- [Action 2] (no [specific tool/command])

**Why this is non-negotiable:** [consequence if violated]
```

**Inline format (skills):**
```markdown
**IMPORTANT:** Don't retry failing commands in a sleep loop — diagnose the root cause
or consider an alternative approach.
```

---

## Agent Frontmatter

### Required Fields

| Field | Type | Validation |
|-------|------|------------|
| `name` | string | `^[a-z0-9-]+$`; unique per scope |
| `description` | string | when to delegate; non-empty |

### Optional Fields

| Field | Type | Default | Purpose |
|-------|------|---------|---------|
| `tools` | string/array | All inherited | Allowlist (comma-separated or array) |
| `disallowedTools` | string/array | - | Denylist (removes from inherited) |
| `model` | string | `inherit` | `sonnet`, `opus`, `haiku`, `inherit` |
| `permissionMode` | string | `default` | `default`, `acceptEdits`, `dontAsk`, `bypassPermissions`, `plan`, `bubble` |
| `skills` | array | - | Preloaded skill names (injected into context) |
| `hooks` | object | - | Scoped hooks: `PreToolUse`, `PostToolUse`, `Stop` |
| `criticalSystemReminder` | string | - | Injected every turn; use for read-only enforcement or safety constraints |
| `maxTurns` | number | - | Execution budget (e.g., `200`) |

**Undocumented (observed working):**

| Field | Type | Purpose |
|-------|------|---------|
| `color` | string | UI color indicator |
| `category` | string | Organization grouping |
| `whenToUseDynamic` | boolean | Dynamic trigger evaluation |

**Note:** If both `tools` and `disallowedTools` specified, `disallowedTools` removes from `tools`. Skills listed in `skills` array are **injected** into subagent context (not just available for invocation).

### Agent Hooks (Frontmatter)

Same structure as skill hooks. Supported events: `PreToolUse`, `PostToolUse`, `Stop`.

---

## File Structure

### Skill

```
.agents/skills/my-skill/
├── SKILL.md              # Required: frontmatter + body
├── references/           # Optional: one level deep
├── scripts/              # Optional: executables (run, don't read)
└── *.md                  # Optional: supporting files
```

### Agent

```
.claude/agents/my-agent.md    # Frontmatter + system prompt body (Claude Code)
.github/agents/my-agent.md    # GitHub Copilot
```

---

## Storage Locations

Agent Skills is an open standard. Discovery paths vary by provider but the SKILL.md format is identical.

### Project skills (any of these work)

| Path | Read by |
|------|---------|
| `.agents/skills/<name>/SKILL.md` | Open-standard location — Copilot natively discovers this |
| `.claude/skills/<name>/SKILL.md` | Claude Code (and Copilot) |
| `.github/skills/<name>/SKILL.md` | Copilot |

Recommended pattern: keep one source of truth (e.g., `.agents/skills/`) and symlink the other paths to it so every provider finds the same content.

### User-scoped skills (personal, across projects)

| Path | Read by |
|------|---------|
| `~/.claude/skills/<name>/SKILL.md` | Claude Code |
| `~/.copilot/skills/<name>/SKILL.md` | Copilot |
| `~/.agents/skills/<name>/SKILL.md` | Open-standard |

### Agents (Claude Code priority order)

| Priority | Location | Scope |
|----------|----------|-------|
| 1 | `--agents` CLI flag (JSON) | Session only |
| 2 | `.claude/agents/*.md` | Project |
| 3 | `~/.claude/agents/*.md` | User (all projects) |
| 4 | Plugin `agents/` directory | Plugin scope |

### Nested Discovery

Claude Code discovers skills in nested `.claude/skills/` directories (monorepo support):
```
monorepo/
├── .claude/skills/global/SKILL.md
└── packages/frontend/.claude/skills/frontend/SKILL.md
```

---

## Size Limits

| Element | Target | Maximum |
|---------|--------|---------|
| SKILL.md body | <300 lines | 500 lines |
| SKILL.md tokens | <2,000 | 5,000 |
| Skill name | - | 64 chars |
| Description | 100-400 chars | 1024 chars |
| All descriptions budget | - | 15,000 chars combined |
| References depth | one level | one level |

---

## Invocation Control Matrix

| Frontmatter | User invokes | Claude invokes | Description loaded |
|-------------|--------------|----------------|-------------------|
| (default) | Yes | Yes | Always |
| `disable-model-invocation: true` | Yes | No | NOT in context |
| `user-invocable: false` | No | Yes | Always |

**Note:** `implicit: true` is a convention requiring a SessionStart hook — not a native Claude Code field.

---

## Validation Rules

### Critical (Blocks Function)

- [ ] Frontmatter starts line 1 with `---`
- [ ] Valid YAML syntax (spaces, not tabs)
- [ ] No XML tags in `name` or `description`
- [ ] `name` matches `^[a-z0-9-]{1,64}$` (if provided)
- [ ] `name` excludes reserved words: "anthropic", "claude"

### High (Impacts Quality)

- [ ] `description` present (recommended; defaults to first paragraph)
- [ ] Description uses third-person voice
- [ ] 5+ trigger phrases in description
- [ ] Body <500 lines
- [ ] All referenced files exist
- [ ] Forward slashes in paths (not backslashes)
- [ ] References one level deep only

---

## Built-in Agent Types

| Type | Model | Tools | Use For |
|------|-------|-------|---------|
| `Explore` | Haiku | Read-only | Fast codebase search |
| `Plan` | Inherit | Read-only | Research, planning |
| `general-purpose` | Inherit | All | Complex multi-step tasks |
| `Bash` | Inherit | Bash | Terminal operations |
| `statusline-setup` | Sonnet | Read, Edit | Status line configuration |
| `claude-code-guide` | Haiku | Read-only + Web | Feature documentation |

**Custom agents:** Reference by name from `.claude/agents/`.

---

## Hooks Reference

### Hook Events

| Event | Fires When | Matcher Values | Scope |
|-------|------------|----------------|-------|
| `SessionStart` | Session begins/resumes | `startup`, `resume`, `clear`, `compact` | Settings |
| `UserPromptSubmit` | User submits prompt | None | Settings |
| `PreToolUse` | Before tool execution | Tool names (regex) | Settings, Frontmatter |
| `PermissionRequest` | Permission dialog appears | Tool names | Settings |
| `PostToolUse` | After tool succeeds | Tool names (regex) | Settings, Frontmatter |
| `PostToolUseFailure` | After tool fails | Tool names | Settings |
| `Notification` | Notification sent | `permission_prompt`, `idle_prompt`, etc. | Settings |
| `Stop` | Claude finishes responding | None | Settings, Frontmatter |
| `SubagentStart` | Spawning subagent | Agent names | Settings |
| `SubagentStop` | Subagent finishes | Agent names | Settings, Frontmatter |
| `PreCompact` | Before context compaction | `manual`, `auto` | Settings |
| `Setup` | Repository init/maintenance | `init`, `maintenance` | Settings |
| `SessionEnd` | Session terminates | None | Settings |

### Hook Configuration (settings.json)

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash|Edit|Write",
        "hooks": [
          {
            "type": "command",
            "command": "./scripts/validate.sh",
            "timeout": 60
          }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          {
            "type": "prompt",
            "prompt": "Check if all tasks complete: $ARGUMENTS",
            "timeout": 30
          }
        ]
      }
    ]
  }
}
```

### Hook Types

| Type | Description | Supported Events |
|------|-------------|-----------------|
| `command` | Execute bash script | All events |
| `prompt` | LLM evaluates condition | `PreToolUse`, `PostToolUse`, `PermissionRequest` |
| `agent` | Runs sub-agent with tools | `PreToolUse`, `PostToolUse`, `PermissionRequest` |

### Decision Controls

| Event | Decisions | JSON Field |
|-------|-----------|------------|
| `PreToolUse` | `allow`, `deny`, `ask` | `hookSpecificOutput.permissionDecision` |
| `PermissionRequest` | `allow`, `deny` | `decision.behavior` |
| `PostToolUse` | `block` | `decision` |
| `UserPromptSubmit` | `block` | `decision` |
| `Stop`/`SubagentStop` | `block` | `decision` |

**PreToolUse `hookSpecificOutput` fields:**
- `permissionDecision`: `"allow"`, `"deny"`, or `"ask"`
- `permissionDecisionReason`: Reason string
- `updatedInput`: Modified tool input (mutate parameters before execution)
- `hookEventName`: Required; must match the event

**Note:** Top-level `decision` field is deprecated for PreToolUse; use `hookSpecificOutput.permissionDecision`.

### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success (stdout as context or shown in verbose) |
| 2 | Blocking error (stderr shown, operation blocked) |
| Other | Non-blocking error (stderr in verbose mode) |

### Environment Variables

| Variable | Description |
|----------|-------------|
| `CLAUDE_PROJECT_DIR` | Absolute path to project root |
| `CLAUDE_CODE_REMOTE` | `"true"` if web environment |
| `CLAUDE_ENV_FILE` | File for persisting env vars (SessionStart) |
| `${CLAUDE_PLUGIN_ROOT}` | Plugin directory (plugin hooks) |

---

## Agent Execution

### Foreground vs Background

| Mode | Behavior | Permission Prompts | MCP |
|------|----------|-------------------|-----|
| Foreground | Blocks main conversation | Interactive | Available |
| Background | Concurrent | Auto-denied | Unavailable |

**Trigger background:** Ask Claude, press Ctrl+B, or set `CLAUDE_CODE_DISABLE_BACKGROUND_TASKS=1` to disable.

### Context & Resume

- Agents operate in isolated context windows
- Cannot spawn other subagents (no `Task` tool in agents)
- Can be resumed with agent ID for follow-up work
- Auto-compaction at ~95% capacity (`CLAUDE_AUTOCOMPACT_PCT_OVERRIDE`)

### Fork Behavioral Guidelines

When using `context: fork` or omitting `subagent_type` to fork:
- **Don't peek**: Do not read the fork's `output_file` mid-flight — it pulls tool noise into your context
- **Don't race**: Never fabricate or predict fork results before completion
- **Forks are cheap**: They share prompt cache, so fork liberally for independent research/implementation
- When to fork: investigation across multiple unrelated areas, or well-understood fixes

### Transcripts

Location: `~/.claude/projects/{project}/{sessionId}/subagents/agent-{agentId}.jsonl`

---

## Permission Rules

### Deny Subagents

```json
{ "permissions": { "deny": ["Task(Explore)", "Task(my-agent)"] } }
```

### Allow/Deny Skills

```json
{ "permissions": { "allow": ["Skill(commit)"], "deny": ["Skill(deploy:*)"] } }
```

---

## Skill vs Agent Decision

```
Need reusable instructions?     → Skill
Need isolated context?          → Agent
Need parallel execution?        → Agent
Need tool restrictions?         → Either (Agent has full control)
Need dynamic loading?           → Skill
Verbose output containment?     → Agent
Always active (implicit)?       → SessionStart hook + skill with implicit: true
```

---

## CLI Agent Format (`--agents`)

```bash
claude --agents '{
  "code-reviewer": {
    "description": "Expert code reviewer",
    "prompt": "You are a senior code reviewer...",
    "tools": ["Read", "Grep", "Glob"],
    "model": "sonnet",
    "permissionMode": "default",
    "skills": ["code-standards"]
  }
}'
```

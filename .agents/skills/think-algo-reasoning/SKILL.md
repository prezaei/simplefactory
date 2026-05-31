---
user-invocable: false
name: think-algo-reasoning
description: >
  Algorithm and protocol logic reasoning to identify implicit assumptions that can be violated.
  USE THIS SKILL for: algorithm, protocol, compression, encoding, crypto implementation, state machine, parser, invariant, assumption, logic flaw.
  STRATEGY: Algorithmic Reasoning.
version: 1.0.0
license: MIT
---

# Algorithmic Reasoning

## Purpose

Understand algorithm and protocol logic deeply enough to identify implicit assumptions that can be violated. Reason about what inputs or sequences of operations would break invariants the code depends on.

## Scope

- Custom algorithm implementations (compression, encoding, hashing, sorting)
- Protocol state machines (handshake sequences, session management)
- Parser logic (format handling, deserialization, schema validation)
- Cryptographic implementations (not library usage — actual crypto code)
- Mathematical invariants (overflow, underflow, precision loss, division by zero)

## Analysis Procedure

1. **Identify the algorithm/protocol**:
   - What is the code trying to do? What algorithm or protocol does it implement?
   - Read the implementation to understand the logic, not just the function signatures

2. **Extract implicit assumptions**:
   - What does the code assume about input size, format, or range?
   - What does it assume about ordering, uniqueness, or completeness?
   - What does it assume about the relationship between input size and output size?
   - What happens at boundary conditions (empty input, maximum size, overflow)?

3. **Reason about violation**:
   - For each assumption: can an attacker craft input that violates it?
   - What would happen if the assumption is violated? (crash, corruption, bypass, information leak)
   - Is the violation reachable from external input, or only from internal state?

4. **Construct theoretical input**:
   - Describe (in words, not code) what input would trigger the violation
   - Explain WHY that input would break the assumption
   - Trace the execution path that leads from the input to the failure

5. **Inline verification**: Before reporting:
   - Attempt to disprove: is there a check I missed that prevents the violation?
   - Consider whether the assumption is ACTUALLY wrong, or just unconventional
   - **CRITICAL**: This skill cannot execute code. All findings are based on reasoning, not testing.
   - Set Verification Status to **Challenged** (maximum) or **Unverified**. Never **Verified**.

## Execution Gap Constraint

This skill analyzes code through reasoning, not execution. Unlike Anthropic's research where Claude could compile and crash-test code in a VM, this skill cannot validate findings through execution.

**Every finding from this skill MUST include**: "This finding is based on code reasoning, not execution. Manual verification or testing is recommended."

**Verification Status cap**: Findings from this skill may reach **Challenged** at maximum (never **Verified**). Challenged means: "I argued both sides and could not disprove the finding, but I cannot confirm it without execution."

## Output Format

For each finding, provide:

```markdown
### {Title}

**Attack Hypothesis**: {What an attacker could achieve by violating the assumption}

**Reasoning Chain**:
{Narrative explaining the algorithm logic, the implicit assumption identified,
why it can be violated, and what input would trigger the violation.
This requires showing UNDERSTANDING of the algorithm, not just pattern matching.
Example: "The LZW encoder assumes compressed output is always smaller than input.
This assumption holds for typical data but fails when..."}

**Evidence**: {file:line for the assumption, file:line for the missing check}

⚠️ *This finding is based on code reasoning, not execution. Manual verification or testing is recommended.*

| Field | Value |
|-------|-------|
| Reasoning Strategy | Algorithmic Reasoning |
| Severity | {Critical · High · Medium · Low · Minimal} |
| Confidence | {High · Medium · Low} — {justification} |
| Verification Status | {Challenged · Unverified} — {what was checked} |
| Control Status | {Observed · Documented · Not Found} |

**Remediation**: {Actionable fix — typically: add the missing check or remove the assumption}
**Proportionality** (High/Critical only): {Why this fix is proportionate}
```

## Severity Definitions

- **Critical**: Assumption violation leads to memory corruption, code execution, or complete auth bypass
- **High**: Assumption violation leads to data corruption, significant information disclosure, or denial of service
- **Medium**: Assumption violation leads to incorrect behavior with moderate security impact
- **Low**: Assumption violation leads to edge-case behavior with limited security impact
- **Minimal**: Theoretical assumption violation with no practical exploit path

## Boundaries

- Do NOT execute code, compile, or test — all analysis is through reasoning
- Do NOT claim "Verified" status — cap at "Challenged"
- Do NOT analyze standard library or framework crypto usage (that's `sec-crypto`'s job) — focus on CUSTOM implementations
- Do NOT produce findings for assumptions that are clearly documented and intentional
- MAY use flow tracing tactics to determine whether an assumption violation is reachable from external input
- Be ESPECIALLY honest about confidence — algorithmic reasoning without execution is inherently uncertain

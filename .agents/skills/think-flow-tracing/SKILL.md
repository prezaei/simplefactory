---
user-invocable: false
name: think-flow-tracing
description: >
  Semantic data and control flow tracing for context-dependent vulnerability discovery.
  USE THIS SKILL for: data flow, control flow, trust boundary, input validation, unsafe function, precondition, buffer, injection path, taint analysis.
  STRATEGY: Flow Tracing.
version: 1.0.0
license: MIT
---

# Semantic Flow Tracing

## Purpose

Trace data and control flow through the system, reasoning about preconditions, trust boundary crossings, and unsafe function usage in specific context. Not "this function is dangerous" but "this function HERE is dangerous because of how data reaches it."

## Scope

- Data flow from external input to sensitive operations (source → sink tracing)
- Control flow through authentication/authorization gates
- Trust boundary crossings (user→API, API→database, service→service)
- Precondition analysis on buffer operations, arithmetic, and resource allocation
- Context-sensitive unsafe function pattern detection

## Analysis Procedure

1. **Identify sources**: Locate where external/untrusted data enters the system (API parameters, file uploads, headers, environment variables, database reads from user-controlled tables)

2. **Identify sinks**: Locate sensitive operations where untrusted data could cause harm (SQL queries, command execution, file operations, memory operations, privilege checks, response rendering)

3. **Trace paths**: For each source-sink pair flagged by the core skill's hypotheses:
   - Follow the data through each function call, transformation, and boundary crossing
   - Note every validation, sanitization, or encoding step along the path
   - Check whether validations are COMPLETE (all edge cases) or PARTIAL (some gaps)

4. **Analyze preconditions**: For operations that depend on assumptions:
   - Buffer operations: Is the buffer size calculated correctly? Can the calculation underflow/overflow?
   - Arithmetic: Can intermediate values exceed expected ranges?
   - Resource allocation: Are limits enforced before allocation?

5. **Inline verification**: Before reporting each finding:
   - Attempt to disprove: Is there upstream validation I haven't seen?
   - Check for mitigating controls: WAF, middleware, framework-level protections?
   - If you can't disprove but have caveats, set Verification Status to Challenged

## Output Format

For each finding, provide:

```markdown
### {Title}

**Attack Hypothesis**: {What an attacker could achieve}

**Reasoning Chain**:
{Narrative tracing the data flow from source to sink. Show each step.
Include dead ends: "I checked for validation at X but found none.
I then looked for middleware at Y..." This is the PRIMARY content.}

**Evidence**: {file:line for source, each intermediate step, and sink}

| Field | Value |
|-------|-------|
| Reasoning Strategy | Flow Tracing |
| Severity | {Critical · High · Medium · Low · Minimal} |
| Confidence | {High · Medium · Low} — {justification} |
| Verification Status | {Verified · Challenged · Unverified} — {what was checked} |
| Control Status | {Observed · Documented · Not Found} |

**Remediation**: {Actionable fix}
**Proportionality** (High/Critical only): {Why this fix is proportionate}
```

## Severity Definitions

- **Critical**: Untrusted input reaches code execution, auth bypass, or memory corruption with no validation
- **High**: Untrusted input reaches data access or privilege operation with incomplete validation
- **Medium**: Untrusted input reaches output rendering with partial encoding, or crosses trust boundary without full verification
- **Low**: Minor validation gap with limited exploitability
- **Minimal**: Theoretical flow concern with no practical impact

## Boundaries

- Do NOT execute code or test payloads
- Do NOT claim a flow is exploitable without tracing the actual path — "strcat is dangerous" is NOT a finding
- Do NOT assume validation is absent because you didn't find it — say "I could not locate validation" and set confidence accordingly
- MAY use tactics from other strategies (read git history for context, reason about algorithm logic) when it helps trace a flow
- Focus on tracing the HIGHEST-RISK paths identified by the core skill, not cataloguing every data flow

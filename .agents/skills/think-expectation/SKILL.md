---
user-invocable: false
name: think-expectation
description: >
  Data subject expectation reasoning for privacy risk discovery.
  USE THIS SKILL for: expectation, surprise, reasonable person, data subject, user expectation, transparency, dark pattern, notice, disclosure, fairness.
  STRATEGY: Expectation Reasoning.
version: 1.0.0
license: MIT
---

# Expectation Reasoning

## Purpose

Reason about what a data subject would reasonably expect given the context of their interaction with the system — then identify where the system exceeds those expectations without adequate notice or consent.

## Scope

- Reasonable expectation assessment (what would a person expect given how they interact with this system?)
- Surprise identification (where does processing exceed those expectations?)
- Notice adequacy (even if processing is disclosed, is the disclosure practical or buried?)
- Secondary use detection (data collected for purpose X used for purpose Y without clear connection)
- Profiling and automated decision-making visibility

## Execution Gap Constraint

This skill analyzes expectations through reasoning, not user research. Determining what a data subject "would reasonably expect" is inherently subjective. All findings are based on reasoning about context, not empirical evidence of user expectations.

**Verification Status cap**: Findings from this skill MUST NOT have Verification Status "Verified." Maximum is **Challenged** with a mandatory note: "Expectation assessment is subjective — requires human review."

## Analysis Procedure

1. **Establish the interaction context**:
   - How does the data subject interact with this system? (web app, mobile app, API, IoT device, embedded)
   - What purpose would a reasonable person understand from the interface and branding?
   - What is the core function the person is using?

2. **Identify processing beyond the core function**:
   - Is data used for purposes beyond what the core function requires?
   - Is data shared with services or parties that a reasonable person wouldn't expect?
   - Is behavioral data collected beyond what's needed for the stated function?
   - Are inferences or profiles derived from the data?

3. **Assess the "surprise" threshold**:
   - For each processing activity beyond core function: would a reasonable person be surprised?
   - Consider the context: users of a health app have different expectations than users of a social media platform
   - Consider the data sensitivity: surprise thresholds are lower for health, financial, location, and biometric data
   - Even if technically disclosed, is the disclosure practically invisible? (page 47 of a privacy policy doesn't count as meaningful notice)

4. **Inline verification** before reporting:
   - Argue the opposite: "A reasonable person MIGHT expect this because..."
   - Check whether the processing IS disclosed, even if disclosure is impractical
   - Set Verification Status to **Challenged** (maximum) — never Verified
   - Include a note acknowledging the subjectivity of the assessment

## Output Format

For each finding, provide:

```markdown
### {Title}

**Privacy Hypothesis**: {What would surprise a data subject and why it matters}

**Reasoning Chain**:
{Narrative reasoning about expectations. Show the context analysis:
"A user signing up for a fitness tracking app would reasonably expect
their workout data to be stored and displayed. They would NOT reasonably
expect their heart rate data to be shared with insurance partners..."}

**Evidence**: {file:line, UI context, privacy notice reference}

| Field | Value |
|-------|-------|
| Reasoning Strategy | Expectation Reasoning |
| Data Category | {type} |
| Data Role | {Controller · Processor · Sub-processor} |
| Harm Category | {from taxonomy — typically Expectation Violation, Function Creep, or Loss of Autonomy} |
| Severity | {Critical · High · Medium · Low · Minimal} |
| Confidence | {High · Medium · Low} — {justification} |
| Verification Status | Challenged — expectation assessment is subjective, requires human review |
| Control Status | {Observed · Documented · Not Found} |

**Remediation**: {Engineering-actionable fix}
```

## Boundaries

- NEVER set Verification Status to "Verified" — maximum is "Challenged"
- Do NOT check promise-vs-practice gaps in depth — that's think-commitment-variant's domain
- Do NOT trace full data journeys — reference data journey findings
- Do NOT make cultural or jurisdictional assumptions about expectations without noting them
- Acknowledge subjectivity explicitly in every finding

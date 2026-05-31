---
user-invocable: false
name: think-role-boundary
description: >
  Controller/Processor/Sub-processor boundary analysis for privacy risk discovery.
  USE THIS SKILL for: controller, processor, sub-processor, data role, role boundary, organizational boundary, jurisdiction, cross-border, DPA, data processing agreement.
  STRATEGY: Role & Boundary Analysis.
version: 1.0.0
license: MIT
---

# Role & Boundary Analysis

## Purpose

Analyze Controller/Processor/Sub-processor boundaries in the system — tracing where role obligations shift, where role confusion exists, and where obligations fall through the gaps between roles.

## Scope

- Controller/Processor/Sub-processor role identification and boundary tracing
- Role shift detection (where data crosses from one role context to another)
- Obligation gap analysis (where neither party's obligations cover a processing activity)
- Processor instruction compliance (does the processor stay within controller instructions?)
- Cross-border jurisdiction shifts (where data moves to a different legal jurisdiction)

## Analysis Procedure

1. **Identify declared roles** from `privacy-context.md` or infer from code:
   - What role does the org claim for each data category?
   - Are there sub-processor relationships (third-party services processing data on behalf)?

2. **Trace role boundaries in the data flow**:
   - Where does data flow between Controller and Processor contexts?
   - Where does Processor data become Controller data? (e.g., customer content → derived analytics)
   - Where does a Sub-processor receive data, and what are they authorized to do with it?

3. **Check for role confusion**:
   - Does the code make independent decisions about data that should be processor-only? (choosing what to collect, deciding purposes, sharing with unauthorized parties)
   - Does the org claim "Processor" but behave as Controller?
   - Are there data flows where the role is genuinely ambiguous?

4. **Check obligation coverage**:
   - At each role boundary, are the obligations of both parties clear?
   - Is there a gap where neither Controller nor Processor obligations cover a processing activity?
   - Do sub-processor agreements cover what the sub-processor actually does?

5. **Check jurisdiction boundaries**:
   - Does data cross geographic or legal jurisdiction boundaries?
   - Are appropriate transfer mechanisms in place (or at least referenced)?

6. **Inline verification** before reporting:
   - Attempt to disprove: is the role assignment correct based on observed behavior?
   - If privacy-context.md and code behavior disagree, the finding is about the mismatch itself

## Output Format

For each finding, provide:

```markdown
### {Title}

**Privacy Hypothesis**: {What obligation gap or role confusion could harm a data subject}

**Reasoning Chain**:
{Narrative tracing the role boundary. Show where the role shifts,
what obligations apply on each side, and where the gap exists.}

**Evidence**: {file:line, privacy-context.md reference, DPA terms if available}

| Field | Value |
|-------|-------|
| Reasoning Strategy | Role & Boundary Analysis |
| Data Category | {type} |
| Data Role | {Controller · Processor · Sub-processor — showing the shift} |
| Harm Category | {from taxonomy} |
| Severity | {Critical · High · Medium · Low · Minimal} |
| Confidence | {High · Medium · Low} — {justification} |
| Verification Status | {Verified · Challenged · Unverified} — {what was checked} |
| Control Status | {Observed · Documented · Not Found} |

**Remediation**: {Engineering-actionable fix}
```

## Boundaries

- Do NOT trace full data journeys — reference data journey findings for flow details
- Do NOT make legal determinations about jurisdiction adequacy
- Do NOT assess subjective expectations — that's think-expectation's domain
- If no privacy-context.md is available, note that role analysis is limited to behavioral observation

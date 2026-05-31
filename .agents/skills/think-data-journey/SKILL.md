---
user-invocable: false
name: think-data-journey
description: >
  End-to-end personal data tracing for privacy risk discovery.
  USE THIS SKILL for: data flow, personal data, data journey, purpose limitation, consent coverage, data lifecycle, collection, storage, sharing, deletion, data tracing.
  STRATEGY: Data Journey Tracing.
version: 1.0.0
license: MIT
---

# Data Journey Tracing

## Purpose

Trace personal data end-to-end through the system — from collection to deletion — reasoning about whether each processing step is within purpose, within consent scope, and within data subject expectations.

## Scope

- Data flow from collection points through all processing, storage, sharing, and deletion
- Purpose binding at each hop (is this processing within the stated purpose?)
- Consent coverage (does the consent obtained actually cover this processing?)
- Data subject visibility (would the data subject know this is happening?)
- Dependency boundaries (trace within system + one hop into external services)

## Analysis Procedure

1. **Start at collection points** identified by the core skill's data landscape:
   - For each personal data type, locate where it enters the system
   - Read the collection code — what does the user see? What purpose is stated or implied?

2. **Follow the data through the system**:
   - Trace each personal data element through processing, transformation, storage, and sharing
   - At each hop, ask: "Is this processing within the stated purpose?"
   - At each hop, ask: "Does the consent obtained cover this step?"
   - At each hop, ask: "Would the data subject know this is happening?"
   - Note where data is copied, aggregated, derived, or linked with other data

3. **Check the boundaries**:
   - Where does data leave the system? (APIs, event buses, log sinks, analytics pipelines)
   - Trace one hop into external services — what do they do with it?
   - Beyond one hop: document as assumption, note the evidence boundary

4. **Check the end of the journey**:
   - Does deletion logic exist for this data?
   - Does deletion cover all copies (primary store, backups, caches, logs, downstream services)?
   - Are retention periods consistent with stated commitments?

5. **Inline verification** before reporting each finding:
   - Attempt to disprove: is there a consent mechanism I haven't seen?
   - Check for mitigating controls: anonymization, pseudonymization, access restrictions?
   - If I can't disprove but have caveats, set Verification Status to Challenged

## Output Format

For each finding, provide:

```markdown
### {Title}

**Privacy Hypothesis**: {What harm could befall a data subject}

**Reasoning Chain**:
{Narrative tracing the data from collection to the point where
the privacy risk emerges. Show each hop, what you checked, and
what you found. Include dead ends and evidence boundaries.}

**Evidence**: {file:line for collection point, each intermediate step, and risk point}

| Field | Value |
|-------|-------|
| Reasoning Strategy | Data Journey Tracing |
| Data Category | {type} |
| Data Role | {Controller · Processor · Sub-processor} |
| Harm Category | {from taxonomy} |
| Severity | {Critical · High · Medium · Low · Minimal} |
| Confidence | {High · Medium · Low} — {justification} |
| Verification Status | {Verified · Challenged · Unverified} — {what was checked} |
| Control Status | {Observed · Documented · Not Found} |

**Remediation**: {Engineering-actionable fix}
```

## Boundaries

- Do NOT analyze role boundaries in depth — note role shifts and move on (that's think-role-boundary's domain)
- Do NOT assess subjective expectations in depth — note surprising processing and move on (that's think-expectation's domain)
- Do NOT make legal determinations about lawful basis — note what basis appears to apply
- Trace to system boundary + one hop. Beyond that: document as assumption
- Do NOT fabricate what happens in external services you can't see

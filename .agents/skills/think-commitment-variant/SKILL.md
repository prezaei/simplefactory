---
user-invocable: false
name: think-commitment-variant
description: >
  Privacy commitment-vs-implementation gap analysis.
  USE THIS SKILL for: commitment, promise, privacy policy, DPA, terms of service, privacy notice, implementation gap, say vs do, retention, deletion, data processing agreement.
  STRATEGY: Commitment Variant Analysis.
version: 1.0.0
license: MIT
---

# Commitment Variant Analysis

## Purpose

Study stated privacy commitments (privacy policy, DPA, terms of service, privacy-context.md) and find where implementation behaviorally diverges from what was promised. The "say one thing, do another" pattern.

## Scope

- Privacy notice vs implementation comparison (are stated purposes honored?)
- DPA commitment verification (are contractual promises implemented?)
- Retention/deletion commitment tracing (does data actually get deleted when promised?)
- Data sharing commitment verification (is data shared only with disclosed parties?)
- Consent scope verification (does the implementation stay within the scope consent was granted for?)

## Analysis Procedure

1. **Inventory stated commitments**:
   - Read `privacy-context.md` customer commitments section (if available)
   - Read any privacy policy, DPA, or terms of service in the repository
   - Read design docs for stated privacy properties
   - List each concrete, verifiable commitment

2. **For each commitment, trace the implementation**:
   - Don't ask "does a mechanism exist?" — ask "does it actually work end-to-end?"
   - Example: "Data deleted on termination" → trace: is there a termination trigger? Does it reach all data stores? What about backups, caches, logs, downstream services?
   - Example: "Data not shared with third parties" → trace: are there API calls to external services that include personal data?

3. **Check the less-obvious places**:
   - Backups and disaster recovery (data deleted from primary but retained in backups?)
   - Logging pipelines (personal data in log entries with different retention?)
   - Analytics/telemetry (data "not shared" but sent to analytics service?)
   - Caches (data deleted but cached copies persist?)
   - Downstream services (data deleted locally but copies exist in dependent services?)

4. **Assess the gap**:
   - Is the gap material? (A 24-hour cache delay before deletion is different from permanent retention in backups)
   - Is the gap visible to data subjects? (Would they know their data isn't fully deleted?)
   - What's the concrete harm if the gap is exploited or discovered?

5. **Inline verification** before reporting:
   - Attempt to disprove: is there a mechanism I haven't found?
   - Check whether the commitment has qualifiers ("best efforts," "commercially reasonable")
   - Consider whether the gap is a genuine implementation oversight or an intentional design choice

## Output Format

For each finding, provide:

```markdown
### {Title}

**Privacy Hypothesis**: {What commitment is violated and what harm results}

**Reasoning Chain**:
{Narrative comparing the stated commitment to the observed implementation.
"The DPA states data is deleted within 30 days of account termination.
I traced the deletion flow from the account termination endpoint at
src/api/accounts.ts:142. It triggers deletion in the primary database,
but I found no corresponding deletion in the backup pipeline at
src/backup/scheduler.ts. Backups have a 90-day retention..."}

**Evidence**: {commitment source, implementation file:line, gap location}

| Field | Value |
|-------|-------|
| Reasoning Strategy | Commitment Variant Analysis |
| Data Category | {type} |
| Data Role | {Controller · Processor · Sub-processor} |
| Harm Category | {from taxonomy — typically Function Creep or Unauthorized Disclosure} |
| Severity | {Critical · High · Medium · Low · Minimal} |
| Confidence | {High · Medium · Low} — {justification} |
| Verification Status | {Verified · Challenged · Unverified} — {what was checked} |
| Control Status | {Observed · Documented · Not Found} |

**Remediation**: {Engineering-actionable fix}
```

## Boundaries

- Do NOT generate findings when no commitments are available — run in "general principles" mode and note the limitation
- Do NOT trace full data journeys — reference data journey findings for flow details
- Do NOT assess whether commitments themselves are adequate — only whether implementation matches
- Do NOT make legal judgments about commitment enforceability
- When commitment language is ambiguous ("reasonable efforts"), note the ambiguity rather than assuming breach

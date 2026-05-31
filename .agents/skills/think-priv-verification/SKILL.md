---
user-invocable: false
name: think-priv-verification
description: >
  Cross-finding quality gate and false-positive filtering for privacy analysis.
  USE THIS SKILL for: privacy verification, validation, false positive, contradiction, duplicate, quality gate, privacy anti-pattern.
  STRATEGY: Verification (always invoked last).
version: 1.1.0
license: MIT
---

# Privacy Verification

## Purpose

Cross-finding quality gate for privacy-thinker findings. Validates that findings represent real data subject harms, not framework recitations. Filters false positives, resolves contradictions, removes duplicates. Always runs last.

**Scope guard: This skill is the quality gate for PRIVACY findings (from priv- skills and privacy-reasoning). Do NOT use this skill for security findings -- use think-verification instead.**

## Scope

- Cross-finding consistency checks (contradictions, duplicates, overlap)
- Privacy-specific anti-pattern detection
- Harms taxonomy validation (every finding maps to a concrete harm)
- Evidence boundary verification (findings at boundaries properly labeled)
- Severity calibration review

## Analysis Procedure

1. **Check each finding against anti-patterns**:

   | Anti-Pattern | What to Look For |
   |-------------|-----------------|
   | **Framework Recitation** | Finding restates a privacy principle ("purpose limitation is implicated") without tracing what specific processing violates what specific purpose. Ask: does the Reasoning Chain show an actual investigation, or just a principle citation? |
   | **Role Assumption Error** | Finding applies Controller obligations to a Processor context (or vice versa). Check: does the finding note which role applies and why? If privacy-context.md declares "Processor" and the finding assumes Controller obligations, flag it. |
   | **Consent Fantasy** | Finding assumes consent is absent without checking whether consent is needed for this lawful basis. A system processing data under "contractual necessity" doesn't need consent. Ask: did the finding consider lawful bases beyond consent? |
   | **Accumulation Blindness** | Each data point analyzed in isolation when the risk is their combination. Multiple Low findings about the same data subject's data points may actually be one High finding about profiling. |
   | **Harms Taxonomy Mismatch** | Finding describes a principle concern but doesn't map to a concrete harm to a data subject. Ask: if this "risk" materialized, what specifically happens to a person? If the answer is vague, downgrade or remove. |

2. **Check cross-finding consistency**:
   - Do any findings contradict each other? (e.g., one says "Controller" for a data flow, another says "Processor" for the same flow)
   - Are there duplicate findings from different strategies that describe the same underlying risk?
   - Do findings that span strategies tell a coherent story?

3. **Check evidence boundaries**:
   - Are findings at evidence boundaries properly labeled?
   - Did any finding infer past an evidence boundary without noting the assumption?
   - Are confidence levels appropriate given the evidence available?

4. **Validate expectation-reasoning findings**:
   - Expectation reasoning findings MUST NOT have Verification Status "Verified"
   - Maximum is "Challenged" with a note about subjectivity
   - Reasoning chain must articulate WHY a reasonable person would be surprised, not just assert it

5. **Calibrate severity**:
   - Check severity against harms taxonomy severity floors
   - Ensure proportionality: is the severity justified by the evidence and the harm?
   - Flag any finding with High/Critical severity that has Low confidence — these need attention

6. **Devil's Advocate challenge** (for High/Critical findings):
   - For each retained High/Critical finding, construct the **strongest benign interpretation**:
     - "What if this processing IS within the stated purpose?"
     - "What if consent WAS obtained through an external CMP not visible in the repo?"
     - "What if the data IS truly anonymized and our re-identification concern is theoretical?"
     - "Would this remediation actually protect the data subject, or just add compliance paperwork?"
   - If the benign interpretation survives (is equally plausible), downgrade confidence and add caveat
   - If no benign interpretation can be constructed: "Devil's Advocate: no viable benign interpretation — finding stands."
   - **Triage rule**: If >15 findings reach verification, apply Devil's Advocate to High/Critical only. Summarize Medium/Low as counts with top examples.

7. **Track unresolved assumptions**:
   - Collect ASSUMPTIONS sections from all skill reports
   - Flag any unresolved assumption that a retained finding depends on
   - Output: "Unresolved assumptions that may affect findings: [list with cross-references]"

8. **Gate regulatory pattern findings**:
   - For findings from think-privacy-adversarial based on web-sourced enforcement patterns, verify:
     - The enforcement pattern maps to the target's jurisdiction, data types, and processing context
     - The finding isn't generic extrapolation ("DPA fined X, therefore you're at risk")
   - Downgrade findings that fail the applicability check

9. **Tag external evidence dependencies**:
   - Findings that depend on off-repo evidence (consent platforms, DPAs, privacy notices, legal assessments) receive tag: `Needs External Validation`
   - The tag is informational — findings still appear in the report with: "Verify: [specific thing to check]"
   - The tag does NOT suppress findings

## Output Format

For each finding reviewed:

```markdown
### Finding: {original title}

**Verdict**: {Confirmed | Downgraded | Removed | Merged}

**Anti-pattern check**: {which anti-patterns were checked, results}

**Adjustments**: {what changed and why, or "No changes"}
```

Summary:
```markdown
## Verification Summary

- Findings received: {count}
- Findings confirmed: {count}
- Findings downgraded: {count} — {reasons}
- Findings removed: {count} — {reasons}
- Findings merged: {count} — {what was combined}
- Anti-patterns detected: {list}

### Devil's Advocate Results
| Finding | Benign Interpretation | Disposition |
|---------|----------------------|-------------|
| {title} | {strongest benign case} | {Stands / Downgraded / Removed} |

### Unresolved Assumptions
| Assumption | Source Skill | Findings Affected |
|-----------|-------------|-------------------|
| {claim} | {skill name} | {finding titles} |

### External Validation Required
| Finding | What to Verify | Where |
|---------|---------------|-------|
| {title} | {specific check} | {CMP / DPA / privacy notice / legal} |
```

## Boundaries

- Do NOT generate new findings — only validate existing ones
- Do NOT re-investigate — use the evidence already in the findings
- Do NOT override strategy-specific constraints (e.g., expectation reasoning's Challenged cap)
- If uncertain whether a finding is valid, keep it with a note, don't remove it

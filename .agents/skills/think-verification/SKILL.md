---
user-invocable: false
name: think-verification
description: >
  Cross-finding quality gate and false-positive filtering.
  USE THIS SKILL for: verification, validation, false positive, contradiction, duplicate, quality gate.
  STRATEGY: Verification (always invoked last).
version: 1.1.0
license: MIT
---

# Cross-Finding Verification

## Purpose

Act as an adversarial quality gate on the full set of findings produced by reasoning strategy skills. This skill does NOT re-check individual findings (each skill does inline verification). It looks for cross-cutting issues across the complete finding set.

**Scope guard: This skill is the quality gate for SECURITY findings (from sec- skills and attack-reasoning). Do NOT use this skill for privacy findings -- use think-priv-verification instead.**

## Scope

- Cross-finding contradiction detection
- Duplicate and overlap identification
- False-positive pattern recognition
- Anti-pattern detection
- Final Verification Status confirmation
- Finding set coherence assessment

## Analysis Procedure

1. **Review all findings as a set**:
   - Read every finding produced by strategy skills
   - Look at the full picture, not individual findings in isolation

2. **Check for contradictions**:
   - Does Finding A assume a control exists while Finding B says it doesn't?
   - Do two findings trace the same data flow and reach different conclusions?
   - Are severity ratings consistent across similar findings?

2.5. **Check investigation coverage**:
   - Compare the entry point census from the core skill against the findings set — are there high-value entry points (state-changing, admin, approval endpoints) that no finding addresses?
   - Flag uncovered high-value entry points as "Gap Noted" items in the verification summary

3. **Identify duplicates and overlap**:
   - Multiple findings about the same underlying issue from different strategies
   - Consolidate into single finding with the strongest evidence chain

4. **Run anti-pattern checks**:

   | Pattern | Detection | Action |
   |---------|-----------|--------|
   | **Confident Hallucination** | High confidence but evidence references code that wasn't read or fully traced | Downgrade confidence to Low; add note |
   | **Pattern-Only Finding** | Cites a function/pattern but Reasoning Chain doesn't trace source → sink | Downgrade to Unverified; note missing trace |
   | **Missing Context Inflation** | "No mitigations found" treated as "vulnerability confirmed" | Add caveat: "mitigations may exist outside analyzed scope" |
   | **Completeness Theater** | Many Low/Minimal findings with thin reasoning; few substantive findings | Flag the pattern; recommend removing weak findings |

5. **Devil's Advocate challenge** (cross-finding, not per-finding):
   - For each retained High/Critical finding: construct the **strongest counterargument** — what would make this finding invalid?
   - **Demand receipts**: "What file:line evidence specifically confirms this? Is the evidence observed or inferred?"
   - **Challenge fix sufficiency**: "If the proposed remediation is applied, will the problem actually be resolved? Could the fix introduce new issues?"
   - **Challenge investigation scope**: "Are we investigating the right problem, or a symptom of a deeper issue?"
   - **Construct alternative explanations**: For each High/Critical finding, attempt to construct an alternative benign explanation. If the alternative survives (is equally plausible), downgrade confidence and add caveat.
   - If no counterargument or alternative can be constructed, note: "Devil's Advocate: no viable counterargument found — finding stands."

6. **Track unresolved assumptions**:
   - Collect ASSUMPTIONS sections from all skill reports (attack-reasoning, strategy skills)
   - Flag any unresolved assumption that a retained finding depends on
   - Output: "Unresolved assumptions that may invalidate findings: [list with finding cross-references]"
   - If a finding depends entirely on unverified assumptions, downgrade to Confidence: Low

7. **Assess overall coherence**:
   - Does the finding set tell a coherent story about this codebase's security posture?
   - Are there obvious gaps — areas the strategies should have investigated but didn't?
   - Would a senior security researcher sign off on this set?

8. **Produce verification summary**:
   - Findings retained (with any adjusted statuses)
   - Findings removed or downgraded (with reasoning)
   - Cross-cutting observations
   - Gaps noted

## Output Format

```markdown
## Verification Summary

### Findings Retained: {count}
### Findings Removed/Downgraded: {count}

### Anti-Pattern Check Results
| Pattern | Detected | Action Taken |
|---------|----------|-------------|
| Confident Hallucination | {Yes/No} | {action if yes} |
| Pattern-Only Finding | {Yes/No} | {action if yes} |
| Missing Context Inflation | {Yes/No} | {action if yes} |
| Completeness Theater | {Yes/No} | {action if yes} |

### Cross-Finding Issues
{Contradictions found, duplicates merged, inconsistencies resolved}

### Devil's Advocate Results
| Finding | Counterargument | Alternative Explanation | Disposition |
|---------|----------------|------------------------|-------------|
| {title} | {strongest counterargument} | {benign alternative, if any} | {Stands / Downgraded / Removed} |

### Unresolved Assumptions
| Assumption | Source Skill | Findings Affected | Risk |
|-----------|-------------|-------------------|------|
| {claim} | {skill name} | {finding titles} | {Could invalidate / Informational} |

### Gaps Noted
{Areas that should have been investigated but weren't, including high-value entry points from the census with no corresponding findings}

### Coherence Assessment
{Brief: does this finding set make sense as a whole?}
```

## Severity Definitions

Not directly applicable — this skill evaluates findings, not code. It may adjust severity/confidence on existing findings.

## Boundaries

- Do NOT produce new vulnerability findings — only evaluate existing ones
- Do NOT re-investigate code — work from the findings and their evidence chains
- Do NOT rubber-stamp findings — if verification never removes or downgrades anything, it isn't working
- MAY reduce finding count — this is a feature, not a failure
- MUST explain every removal or downgrade with specific reasoning

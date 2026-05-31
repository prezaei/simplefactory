---
user-invocable: false
name: priv-data-sharing
description: >
  Third-party data sharing, cross-border transfers, and processor agreement analysis.
  USE THIS SKILL for: data sharing, third party, cross-border, transfer, processor agreement, sub-processor, vendor, partner, export, DPA, standard contractual clauses, SCCs.
  PRINCIPLES: Data Sharing Boundaries, Cross-border Safeguards.
version: 1.1.0
license: MIT
---

# Data Sharing & Cross-Border Transfer Analysis

## Purpose

Analyze third-party data sharing and cross-border transfer practices, focusing on Data Sharing Boundaries and Cross-border Safeguards principles.

## Scope

- Third-party data recipients and sharing justification
- Cross-border data transfer mechanisms
- Processor and sub-processor agreements
- Vendor data handling obligations
- API integrations that transmit personal data
- Data sharing disclosures vs actual sharing

## Analysis Procedure

1. **Map data recipients**: Identify all third parties receiving personal data (APIs, SDKs, services, partners)
2. **Verify sharing justification**: For each recipient, check that sharing has a stated justification and is disclosed
3. **Check agreements**: Verify processor/sub-processor agreements (DPAs) exist or are referenced for each data recipient
4. **Assess cross-border flows**: Identify data transfers across geographic boundaries
5. **Verify transfer mechanisms**: Check for adequate transfer safeguards (standard contractual clauses, adequacy decisions, etc.)
6. **Review sub-processors**: If the system is a processor, verify sub-processor usage is authorized by the controller
7. **Check data minimization in sharing**: Verify only necessary data is shared with each recipient

## Controller/Processor Perspectives

- **Controller**: Must have legal justification for each data recipient; must disclose all sharing; must ensure adequate transfer mechanisms; must maintain processor agreements
- **Processor**: Must not share data beyond controller authorization; must not engage sub-processors without controller consent; must notify controller of sub-processor changes

## Evidence Categories

Evidence categories relevant to this skill: signed DPA/data processing agreements, sub-processor list, data flow diagram showing third parties, third-party audit rights documentation, transfer mechanism documentation (SCCs, BCRs).

## Output Format

For each finding, provide:

```markdown
### {Title}
- **Principle**: {Data Sharing Boundaries | Cross-border Safeguards}
- **Summary**: {Brief description}
- **Details**: {Full explanation}
- **Severity**: {Critical | High | Medium | Low | Minimal}
- **Confidence**: {High | Medium | Low} ({justification})
- **Evidence**: {file path/line OR quote from design doc}
- **Control Status**: {Observed | Documented | Not Found}
- **Evidence Expected**: {applicable evidence category from this skill's Evidence Categories}
- **Tags**: {keywords, including "Commitment Misalignment", LINDDUN category, and/or named risk pattern (Control Theater, Silent Data Flow, Identifier Accumulation, Incomplete Assessment) where applicable}
- **Remediation**: {Actionable fix}
- **Proportionality** (High/Critical only): {Why this remediation is proportionate to the risk}
```

## Severity Definitions

- **Critical**: Personal data shared with unauthorized third parties; cross-border transfers with no transfer mechanism; sharing sensitive data without agreements
- **High**: Missing processor agreements for data recipients; cross-border transfers without adequate safeguards; undisclosed data sharing
- **Medium**: Sharing more data than necessary with third parties; sub-processor changes not tracked; transfer mechanisms referenced but not verified
- **Low**: Minor agreement gaps; sharing documentation incomplete; transfer mechanism details unclear
- **Minimal**: Standard controls sufficient for this domain; theoretical risk only; no practical user impact

## Boundaries

- Do NOT provide legal advice on which transfer mechanisms are adequate for specific jurisdictions
- Do NOT evaluate the legal sufficiency of DPAs or SCCs
- Do NOT contact third parties to verify their data handling
- Focus on identifying sharing gaps and transfer risks, not negotiating agreements
- Tag findings with applicable named risk patterns (Control Theater, Silent Data Flow, Identifier Accumulation, Incomplete Assessment) when the finding matches a pattern

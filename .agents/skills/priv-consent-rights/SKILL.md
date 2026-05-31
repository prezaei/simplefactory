---
user-invocable: false
name: priv-consent-rights
description: >
  Consent management and data subject rights analysis.
  USE THIS SKILL for: consent, opt-in, opt-out, data subject rights, DSAR, access request, erasure, portability, rectification, objection, children, age-gating, parental consent.
  PRINCIPLES: User Control, Consent & Choice, Children's Data.
version: 1.1.0
license: MIT
---

# Consent & Data Subject Rights Analysis

## Purpose

Analyze consent mechanisms and data subject rights implementations, focusing on User Control, Consent & Choice, and Children's Data principles.

## Scope

- Consent collection and management mechanisms
- Consent withdrawal and revocation capabilities
- Data subject access request (DSAR) workflows
- Right to erasure (deletion on request)
- Right to data portability (export in portable format)
- Right to rectification (correction of inaccurate data)
- Right to objection (opt-out of specific processing)
- Children's data protections and age-gating
- Parental consent mechanisms

## Analysis Procedure

1. **Assess consent mechanisms**: Verify consent is freely given, specific, informed, and revocable where consent is the legal justification
2. **Check granularity**: Ensure consent is collected per purpose, not as a blanket acceptance
3. **Evaluate withdrawal**: Verify consent can be withdrawn as easily as it was given
4. **Check consent recording**: Verify consent grants and withdrawals are recorded and auditable
5. **Detect dark patterns**: Flag deceptive design practices that undermine consent validity (e.g., pre-selected checkboxes, confusing opt-out flows, hidden decline options, manipulative language)
6. **Audit DSR capabilities**: For each right (access, erasure, portability, rectification, objection), check if a mechanism exists
7. **Verify response timelines**: Check if DSR workflows can meet regulatory timeframes
8. **Assess children's protections**: If applicable, check for age-gating and parental consent mechanisms
9. **Check identity verification**: Ensure DSR requests include appropriate identity verification to prevent unauthorized access
10. **Assess legal basis awareness**: Where processing relies on legitimate interest rather than consent, verify the processing is narrowly defined and necessary (do NOT perform the legal balancing test — flag for legal review)

## Controller/Processor Perspectives

- **Controller**: Must obtain valid consent, implement all data subject rights directly, manage consent records, implement age-gating
- **Processor**: Must enable controller to manage consent, assist controller in fulfilling DSR requests, provide data exports to controller on request

## Evidence Categories

Evidence categories relevant to this skill: consent UI screenshots, consent log/database, withdrawal flow implementation, age gate UI, DSAR endpoint/API, DSR response workflow documentation, identity verification mechanism.

## Output Format

For each finding, provide:

```markdown
### {Title}
- **Principle**: {User Control | Consent & Choice | Children's Data}
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

- **Critical**: No consent mechanism exists where consent is the legal justification; complete absence of DSR capabilities; processing children's data with no age-gating
- **High**: No erasure mechanism; consent cannot be withdrawn; no DSAR workflow; children's data collected without parental consent mechanism
- **Medium**: Incomplete DSR implementation (some rights missing); consent withdrawal harder than granting; no data portability export
- **Low**: DSR response time unclear; consent records incomplete; minor gaps in consent granularity
- **Minimal**: Standard controls sufficient for this domain; theoretical risk only; no practical user impact

## Severity Escalation Triggers

Apply these severity floors when the corresponding trigger condition is detected:

| Trigger Condition | Severity Floor | Applies To |
|-------------------|---------------|------------|
| Children's data present | High | Any consent, DSR, or children's data finding |
| Sensitive personal data without explicit consent | High | Any Consent & Choice finding |

If the analyst assesses a lower severity than the floor, the finding must include: "Severity floor default is {floor} due to {trigger}. Assessed as {actual} because: {justification}."

## Boundaries

- Do NOT provide legal advice on whether specific consent mechanisms satisfy particular regulations
- Do NOT determine the legal age of consent for specific jurisdictions
- Do NOT access or view actual personal data during assessment
- Focus on identifying mechanism gaps, not implementing fixes
- Tag findings with applicable named risk patterns (Control Theater, Silent Data Flow, Identifier Accumulation, Incomplete Assessment) when the finding matches a pattern

---
user-invocable: false
name: priv-anonymization
description: >
  De-identification, pseudonymization, and re-identification risk analysis.
  USE THIS SKILL for: anonymization, pseudonymization, de-identification, re-identification, hashing, masking, redaction, aggregation, k-anonymity, differential privacy.
  PRINCIPLES: Security of Personal Data.
version: 1.1.0
license: MIT
---

# Anonymization & De-identification Analysis

## Purpose

Analyze de-identification techniques and re-identification risks, focusing on the Security of Personal Data principle.

## Scope

- Anonymization and pseudonymization techniques
- Re-identification risk assessment
- Quasi-identifier combinations
- Aggregation and statistical disclosure
- Hashing, masking, and redaction implementations
- Differential privacy mechanisms
- Encryption of personal data at rest and in transit

## Analysis Procedure

1. **Identify de-identification claims**: Find any data claimed as "anonymized", "de-identified", "pseudonymized", or "aggregated"
2. **Classify on identification spectrum**: For each data element, determine its tier: Identified → Pseudonymized → Unlinked Pseudonymized → Anonymized → Aggregated. Flag data *claimed* at one tier but *evidenced* at a higher tier.
3. **Assess technique adequacy**: Evaluate whether the de-identification technique matches the claim (e.g., hashing ≠ anonymization; pseudonymized ≠ anonymized)
4. **Check quasi-identifiers**: Identify fields that could enable re-identification through combination (zip, DOB, gender, etc.)
5. **Evaluate linkability**: Can de-identified data be linked back to individuals through correlation with other datasets?
6. **Apply contamination principle**: When data at different identification tiers is co-mingled in a single store, classify at the highest tier present. Flag where separation would reduce risk.
7. **Review encryption**: Verify personal data is encrypted at rest and in transit proportionate to sensitivity
8. **Check aggregation**: If data is aggregated, verify group sizes are sufficient to prevent individual identification
9. **Assess key management**: For pseudonymized data, verify the mapping key is adequately protected

## Controller/Processor Perspectives

- **Controller**: Must ensure de-identification techniques are adequate for the stated purpose; must protect pseudonymization keys; must assess re-identification risk
- **Processor**: Must implement de-identification as instructed by controller; must protect pseudonymization keys; must not attempt re-identification

## Evidence Categories

Evidence categories relevant to this skill: de-identification method documentation, re-identification risk assessment, k-anonymity/l-diversity verification, aggregation threshold configuration, pseudonymization key management documentation.

## Output Format

For each finding, provide:

```markdown
### {Title}
- **Principle**: Security of Personal Data
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

- **Critical**: Data claimed as "anonymized" is trivially re-identifiable; personal data stored unencrypted in accessible locations; pseudonymization key stored alongside data
- **High**: Weak de-identification technique (e.g., simple hashing without salt); quasi-identifier combinations enable re-identification; encryption missing for sensitive data
- **Medium**: Aggregation with small group sizes; pseudonymization key management gaps; encryption in transit but not at rest
- **Low**: De-identification documentation gaps; minor technique improvements available; encryption configuration improvements
- **Minimal**: Standard controls sufficient for this domain; theoretical risk only; no practical user impact

## Boundaries

- Do NOT provide legal advice on whether specific de-identification satisfies particular regulatory definitions
- Do NOT attempt actual re-identification of data
- Do NOT access or view actual personal data content
- Focus on identifying technique weaknesses and re-identification risks, not implementing fixes
- Tag findings with applicable named risk patterns (Control Theater, Silent Data Flow, Identifier Accumulation, Incomplete Assessment) when the finding matches a pattern

---
user-invocable: false
name: sec-data
description: >
  Data protection and privacy security analysis.
  USE THIS SKILL for: data protection, privacy, personal data, sensitive data, encryption at rest, data classification.
  STRIDE: Information Disclosure.
version: 2.0.0
license: MIT
---

# Data Protection & Privacy Analysis

## Purpose

Analyze data protection and privacy concerns identified in the threat model. Focus on Information Disclosure threats related to data classification, access controls, and data flow security.

## Scope

- Personal Data and Sensitive Personal Data handling
- Data classification and labeling
- Data flow security (where sensitive data travels)
- Access controls on sensitive data stores
- Data exposure in APIs, logs, and error messages

**Note**: Encryption implementation is covered by `sec-crypto`. Logging/audit is covered by `sec-logging`. This skill focuses on data classification, access boundaries, and flow analysis.

## Analysis Procedure

1. **Identify Data Assets**: Catalog Personal Data and Sensitive Personal Data in the system — classify by sensitivity
2. **Review Data Flow**: Trace data from collection through processing, storage, and output — flag where sensitive data crosses trust boundaries without protection
3. **Assess Access Controls**: Who can access what data and under what conditions — check for overly broad access grants
4. **Check Data Exposure**: Verify sensitive data is not leaked through API responses, error messages, debug outputs, or client-side storage
5. **Evaluate Data Segregation**: Check if sensitive data is isolated from general-purpose stores or co-mingled

## Evidence Categories

Evidence categories relevant to this skill: data classification documentation, access control policies/configuration, data flow diagrams, API response schemas, database access patterns, data masking/redaction configuration.

## Output Format

For each finding, provide:

```markdown
### {Title}
- **Category**: {Information Disclosure}
- **Summary**: {Brief description}
- **Details**: {Full explanation of the vulnerability}
- **Tags**: {keywords, including named risk pattern tags where applicable (Control Theater, Incomplete Assessment)}
- **Severity**: {Critical | High | Medium | Low | Minimal}
- **Confidence**: {High | Medium | Low} ({justification})
- **Evidence**: {file path/line OR quote from design doc}
- **Control Status**: {Observed | Documented | Not Found}
- **Evidence Expected**: {applicable evidence category from this skill's Evidence Categories}
- **Remediation**: {Actionable fix}
- **Proportionality** (High/Critical only): {Why this remediation is proportionate to the risk}
```

## Severity Definitions

- **Critical**: Unencrypted Personal Data exposure, mass data leak vectors, sensitive data in client-side storage
- **High**: Excessive data in API responses, broad access to sensitive stores, sensitive data in error messages
- **Medium**: Overly permissive access controls, incomplete data classification
- **Low**: Minor classification gaps, documentation issues
- **Minimal**: Standard controls sufficient; theoretical risk only; no practical impact

**Constraint**: Do NOT calculate CVSS vector strings. Use qualitative ratings only.

## Boundaries

- Do NOT access or view actual Personal Data
- Do NOT modify data handling code
- Focus on identifying and documenting issues, not implementing fixes
- Defer encryption findings to `sec-crypto` and logging findings to `sec-logging`
- Tag findings with applicable named risk patterns (Control Theater, Incomplete Assessment) when the finding matches a pattern

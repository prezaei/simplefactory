---
user-invocable: false
name: priv-data-lifecycle
description: >
  Data collection, retention, deletion, and minimization analysis.
  USE THIS SKILL for: data collection, retention, deletion, minimization, storage, purge, archive, cleanup, data inventory, personal data.
  PRINCIPLES: Data Minimization, Retention Limits.
version: 1.1.0
license: MIT
---

# Data Lifecycle Analysis

## Purpose

Analyze data collection, retention, and deletion practices for personal data, focusing on Data Minimization and Retention Limits principles.

## Scope

- Personal data collection justification and adequacy
- Data retention schedules and enforcement mechanisms
- Deletion and purge capabilities
- Data inventory completeness
- Storage locations and data residency
- Data lifecycle from collection through destruction

## Analysis Procedure

1. **Inventory personal data**: Catalog all personal data types, their sources, and storage locations
2. **Assess collection justification**: For each data type, verify a stated purpose exists and the data is necessary for that purpose
3. **Review retention policies**: Check for defined retention periods and automated enforcement
4. **Evaluate deletion mechanisms**: Verify data can be permanently deleted when retention expires or on request
5. **Check data residency**: Identify where personal data is stored (databases, logs, caches, backups)
6. **Assess data flow**: Trace personal data from collection through processing to storage and eventual deletion
7. **Verify minimization**: Flag any data collected beyond what is necessary for the stated purpose
8. **Apply contamination principle**: When a data store contains data at mixed sensitivity levels, classify the entire store at the highest level present. Flag where separating data by sensitivity would reduce the protection burden and risk.

## Controller/Processor Perspectives

- **Controller**: Must justify collection, define retention periods, implement deletion mechanisms, maintain full records of processing
- **Processor**: Must process only per controller instructions, assist with deletion on controller request, maintain limited records

## Evidence Categories

Evidence categories relevant to this skill: retention policy configuration, deletion API/endpoint, purge schedule/logs, archive policy document, storage inventory, data inventory documentation.

## Output Format

For each finding, provide:

```markdown
### {Title}
- **Principle**: {Data Minimization | Retention Limits}
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

- **Critical**: No deletion capability for personal data; indefinite retention with no policy; collecting sensitive data without any justification
- **High**: Missing retention schedule for personal data stores; no automated retention enforcement; collecting data substantially beyond stated purposes
- **Medium**: Incomplete deletion (data remains in backups/logs); retention period defined but not enforced; minor over-collection
- **Low**: Missing documentation for retention policies; inconsistent retention across stores; best-practice gaps
- **Minimal**: Standard controls sufficient for this domain; theoretical risk only; no practical user impact

## Terminology

Use *Personal Data* and *Sensitive Personal Data*. "PII" is treated as a legacy subset.

## Severity Escalation Triggers

Apply this severity floor when the corresponding trigger condition is detected:

| Trigger Condition | Severity Floor | Applies To |
|-------------------|---------------|------------|
| Personal data used for AI/ML model training | Medium | Data Minimization or Retention Limits findings related to training data |

If the analyst assesses a lower severity than the floor, the finding must include: "Severity floor default is {floor} due to {trigger}. Assessed as {actual} because: {justification}."

## Boundaries

- Do NOT provide legal advice about whether specific retention periods satisfy regulations
- Do NOT access or view actual personal data content
- Do NOT modify data handling code
- Focus on identifying and documenting issues, not implementing fixes
- Tag findings with applicable named risk patterns (Control Theater, Silent Data Flow, Identifier Accumulation, Incomplete Assessment) when the finding matches a pattern

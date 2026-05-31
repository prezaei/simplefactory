---
user-invocable: false
name: priv-transparency
description: >
  Privacy notices, data flow visibility, and user awareness analysis.
  USE THIS SKILL for: transparency, privacy notice, disclosure, awareness, cookie banner, privacy policy, data flow visibility, notice.
  PRINCIPLES: Transparency.
version: 1.1.0
license: MIT
---

# Transparency Analysis

## Purpose

Analyze whether data processing practices are clearly disclosed to data subjects, focusing on the Transparency principle.

## Scope

- Privacy notices and policies
- Data collection disclosure completeness
- Data flow visibility for users
- Cookie and tracking disclosures
- Third-party sharing disclosures
- User-facing controls and dashboards
- Notification of processing changes

## Analysis Procedure

1. **Locate disclosure mechanisms**: Find privacy notices, policies, consent dialogs, banners, and in-app disclosures
2. **Assess completeness**: Verify disclosures cover all identified personal data types and processing activities
3. **Check clarity**: Evaluate whether notices use clear, plain language (not buried in legal jargon)
4. **Verify data flow disclosure**: Ensure users are informed about where their data goes (including third parties)
5. **Review user dashboards**: Check if users have visibility into their data and processing activities
6. **Assess change notification**: Verify mechanisms exist to notify users of material changes to processing
7. **Check timing**: Ensure notices are provided at or before the point of collection

## Controller/Processor Perspectives

- **Controller**: Must provide comprehensive, clear privacy notices; must disclose all data recipients; must notify of processing changes; must provide user-facing controls
- **Processor**: Must enable controller to provide transparency; must not independently communicate with data subjects unless instructed; must disclose sub-processors to controller

## Evidence Categories

Evidence categories relevant to this skill: privacy notice URL/content, cookie banner screenshots, data flow disclosure documentation, layered notice structure, just-in-time notice implementation.

## Output Format

For each finding, provide:

```markdown
### {Title}
- **Principle**: Transparency
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

- **Critical**: No privacy notice or disclosure mechanism exists; data processing completely hidden from users
- **High**: Privacy notice omits significant data categories or processing activities; third-party sharing undisclosed; no user-facing controls
- **Medium**: Notice exists but is incomplete or unclear; data flow to third parties partially disclosed; no change notification mechanism
- **Low**: Minor notice clarity issues; dashboard missing some data categories; notice formatting improvements needed
- **Minimal**: Standard controls sufficient for this domain; theoretical risk only; no practical user impact

## Severity Escalation Triggers

Apply this severity floor when the corresponding trigger condition is detected:

| Trigger Condition | Severity Floor | Applies To |
|-------------------|---------------|------------|
| Sensitive personal data without explicit consent | High | Any Transparency finding related to sensitive data disclosure |

If the analyst assesses a lower severity than the floor, the finding must include: "Severity floor default is {floor} due to {trigger}. Assessed as {actual} because: {justification}."

## Boundaries

- Do NOT provide legal advice on whether specific notice formats satisfy regulations
- Do NOT draft privacy notices or policy language
- Do NOT evaluate the legal sufficiency of existing notices
- Focus on identifying disclosure gaps, not writing disclosures
- Tag findings with applicable named risk patterns (Control Theater, Silent Data Flow, Identifier Accumulation, Incomplete Assessment) when the finding matches a pattern

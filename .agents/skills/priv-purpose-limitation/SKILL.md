---
user-invocable: false
name: priv-purpose-limitation
description: >
  Purpose binding, secondary use, and function creep analysis.
  USE THIS SKILL for: purpose limitation, secondary use, function creep, purpose binding, data reuse, scope creep, analytics, telemetry, profiling.
  PRINCIPLES: Purpose Limitation.
version: 1.1.0
license: MIT
---

# Purpose Limitation Analysis

## Purpose

Analyze whether personal data processing is limited to disclosed purposes, focusing on the Purpose Limitation principle.

## Scope

- Stated purposes for data collection vs actual processing
- Secondary use of collected data
- Function creep (expanding use beyond original purpose)
- Analytics and telemetry data usage
- Profiling and automated decision-making
- Purpose binding across data flows

## Analysis Procedure

1. **Identify stated purposes**: Extract disclosed purposes from privacy notices, design docs, or code comments
2. **Map actual processing**: Trace how collected data is actually used across all system components
3. **Detect purpose gaps**: Flag processing activities that lack a corresponding stated purpose
4. **Check secondary use**: Identify any reuse of data for purposes not originally disclosed
5. **Assess within-vs-across boundaries**: Flag when data collected in one product or service is used in another. Cross-product data flows typically require additional justification or re-consent and represent elevated purpose limitation risk.
6. **Assess function creep**: Look for roadmap items, feature flags, or code paths that expand data usage beyond original scope
7. **Review analytics/telemetry**: Verify analytics data collection is limited to what's disclosed
8. **Check profiling**: If automated decisions or profiling occurs, verify it's within stated purposes

## Controller/Processor Perspectives

- **Controller**: Must define, document, and enforce purposes; must re-consent before new purposes; must limit processing to disclosed purposes
- **Processor**: Must process only per controller instructions; must not use data for own purposes; must flag any instructions that appear to exceed stated purposes

## Evidence Categories

Evidence categories relevant to this skill: purpose documentation, feature flag configurations, analytics pipeline configuration, data flow mapping, secondary use justification documentation.

## Output Format

For each finding, provide:

```markdown
### {Title}
- **Principle**: Purpose Limitation
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

- **Critical**: Systematic processing for undisclosed purposes; data sold or shared for purposes entirely outside stated scope
- **High**: Analytics/telemetry collecting data significantly beyond disclosure; profiling users without stated purpose; secondary use without re-consent mechanism
- **Medium**: Minor purpose creep (feature flags suggesting expanded use); analytics scope broader than notice suggests; roadmap items implying future secondary use
- **Low**: Documentation gaps between stated and actual purposes; minor inconsistencies in purpose descriptions across artifacts
- **Minimal**: Standard controls sufficient for this domain; theoretical risk only; no practical user impact

## Severity Escalation Triggers

Apply these severity floors when the corresponding trigger condition is detected:

| Trigger Condition | Severity Floor | Applies To |
|-------------------|---------------|------------|
| Children's data present | High | Any purpose limitation finding |
| Personal data used for AI/ML model training | Medium | Purpose Limitation findings related to training data |

If the analyst assesses a lower severity than the floor, the finding must include: "Severity floor default is {floor} due to {trigger}. Assessed as {actual} because: {justification}."

## Boundaries

- Do NOT provide legal advice on whether specific processing activities constitute legitimate interest
- Do NOT determine whether purpose changes require re-consent under specific regulations
- Do NOT modify processing logic
- Focus on identifying purpose gaps and creep signals, not implementing fixes
- Tag findings with applicable named risk patterns (Control Theater, Silent Data Flow, Identifier Accumulation, Incomplete Assessment) when the finding matches a pattern

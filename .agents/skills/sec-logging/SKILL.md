---
user-invocable: false
name: sec-logging
description: >
  Logging, auditing, and monitoring security analysis.
  USE THIS SKILL for: logging, audit, monitoring, SIEM, events, traceability, repudiation, audit trail, security events.
  STRIDE: Repudiation.
version: 2.0.0
license: MIT
---

# Logging & Audit Analysis

## Purpose

Analyze logging, auditing, and monitoring security concerns identified in the threat model. Focus on Repudiation threats - ensuring actions can be attributed and verified.

## Scope

- Security event logging (authentication, authorization, data access)
- Audit trail completeness and integrity
- Log protection against tampering
- Sensitive data in logs (masking/redaction)
- Log retention and availability
- Monitoring and alerting for security events
- Compliance logging requirements

## Analysis Procedure

1. **Identify Critical Actions**: What actions MUST be logged for accountability? (auth, data changes, admin ops)
2. **Check Log Completeness**: Are all security-relevant events captured with sufficient detail?
3. **Evaluate Log Integrity**: Can logs be tampered with? Are they append-only or protected?
4. **Review Sensitive Data Handling**: Is Personal Data masked/redacted in logs?
5. **Assess Log Availability**: Are logs available for incident response? Retention period?
6. **Check Monitoring**: Are security events monitored and alerted on?

## Evidence Categories

Evidence categories relevant to this skill: logging framework configuration, audit log implementation, log storage/retention configuration, SIEM integration, monitoring/alerting rules, sensitive data masking configuration.

## Output Format

For each finding, provide:

```markdown
### {Title}
- **Category**: Repudiation
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

- **Critical**: No logging of authentication failures, complete absence of audit trail for financial/sensitive operations
- **High**: Logs can be tampered with by attackers, no logging of authorization decisions, Personal Data logged unmasked
- **Medium**: Incomplete audit trail, no monitoring/alerting for security events, short retention period
- **Low**: Minor logging gaps, verbose debug logging in production, inconsistent log format
- **Minimal**: Standard controls sufficient; theoretical risk only; no practical impact

**Constraint**: Do NOT calculate CVSS vector strings. Use qualitative ratings only.

## Boundaries

- Do NOT access or read actual log files
- Do NOT modify logging configuration
- Focus on identifying and documenting issues, not implementing fixes
- This skill focuses on REPUDIATION (can actions be denied?), not general observability
- Tag findings with applicable named risk patterns (Control Theater, Incomplete Assessment) when the finding matches a pattern

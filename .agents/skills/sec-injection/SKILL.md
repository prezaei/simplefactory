---
user-invocable: false
name: sec-injection
description: >
  Injection and input validation vulnerability analysis.
  USE THIS SKILL for: injection, input validation, XSS, SQL, command injection, LDAP, XML, template injection, SSRF.
  STRIDE: Tampering.
version: 2.0.0
license: MIT
---

# Injection & Input Validation Analysis

## Purpose

Analyze injection and input validation vulnerabilities identified in the threat model. Focus on Tampering threats.

## Scope

- SQL injection
- Cross-site scripting (XSS)
- Command injection
- LDAP injection
- XML/XXE injection
- Template injection
- Path traversal
- Server-Side Request Forgery (SSRF)
- Input sanitization and validation

## Analysis Procedure

1. **Identify Input Points**: User inputs, API parameters, file uploads, headers
2. **Trace Data Flow**: Follow untrusted input to execution/output points
3. **Check Sanitization**: Input validation, encoding, parameterized queries — verify ORM/query builder usage doesn't bypass parameterization (e.g., raw query methods)
4. **Evaluate Output Encoding**: Context-appropriate escaping (HTML, JS, SQL, URL)
5. **Review Error Handling**: Information leakage in error messages

## Evidence Categories

Evidence categories relevant to this skill: input validation code, parameterized query usage, output encoding implementation, file upload handling, SSRF protection configuration, error handling middleware.

## Output Format

For each finding, provide:

```markdown
### {Title}
- **Category**: {Tampering}
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

- **Critical**: SQL injection with data access, command injection, XXE with file read
- **High**: Stored XSS, LDAP injection, template injection, SSRF to internal services
- **Medium**: Reflected XSS, path traversal with limited scope
- **Low**: Self-XSS, minor input validation gaps
- **Minimal**: Standard controls sufficient; theoretical risk only; no practical impact

**Constraint**: Do NOT calculate CVSS vector strings. Use qualitative ratings only.

## Boundaries

- Do NOT execute injection payloads
- Do NOT modify application code
- Focus on identifying and documenting issues, not implementing fixes
- Tag findings with applicable named risk patterns (Control Theater, Incomplete Assessment) when the finding matches a pattern

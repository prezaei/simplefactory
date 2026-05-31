---
user-invocable: false
name: sec-api
description: >
  API design and implementation security analysis.
  USE THIS SKILL for: API, REST, GraphQL, gRPC, rate limiting, pagination, schema, OpenAPI, swagger, endpoint, mass assignment, BOLA, throttling, query depth.
  STRIDE: Tampering, Denial of Service, Information Disclosure.
version: 2.0.0
license: MIT
---

# API Security Analysis

## Purpose

Analyze API design and implementation security concerns identified in the threat model. Focus on OWASP API Security Top 10 vulnerabilities and common API-specific attack patterns.

## Scope

- Rate limiting and throttling (per-user, per-endpoint, tiered)
- GraphQL security (query depth, complexity, introspection)
- Mass assignment / broken object property level authorization
- Response filtering and excessive data exposure
- Pagination limits and cursor security
- API versioning and deprecation security
- Request size limits and resource consumption
- Error message verbosity and information leakage
- Shadow APIs and undocumented endpoints
- API schema validation (OpenAPI, JSON Schema)

## Analysis Procedure

1. **Review Rate Limiting**: Check for rate limits on sensitive endpoints (auth, password reset, data export)
2. **Evaluate GraphQL Security**: Query depth limits, complexity analysis, introspection disabled in production
3. **Check Mass Assignment**: Verify APIs only accept expected fields, blocklist/allowlist approach
4. **Assess Response Filtering**: Ensure APIs don't return more data than necessary
5. **Review Pagination**: Check for limits, secure cursor implementation, no unbounded queries
6. **Analyze Error Responses**: Verify errors don't leak sensitive information
7. **Check API Inventory (Shadow API Detection)**: Using the endpoint inventory from the threat model (or by discovering routes directly), systematically identify undocumented and shadow APIs:
   - Cross-reference discovered route registrations against API documentation (OpenAPI/Swagger spec, README, developer docs) if available
   - Identify routes present in code but absent from documentation — these are shadow APIs
   - Check for internal, admin, or debug endpoints exposed on the same port/listener as public APIs
   - Look for legacy or deprecated endpoints still registered but not in current documentation
   - Where recognizable, check for non-HTTP entry points (WebSocket, gRPC) not covered by API documentation
   - Flag undocumented state-changing endpoints (POST/PUT/PATCH/DELETE) as higher risk than undocumented read-only endpoints

## Evidence Categories

Evidence categories relevant to this skill: API route definitions, rate limiting middleware/configuration, GraphQL schema and resolvers, request validation schemas, error handling middleware, OpenAPI/Swagger specification, pagination implementation.

## Output Format

For each finding, provide:

```markdown
### {Title}
- **Category**: {Tampering | Denial of Service | Information Disclosure}
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

- **Critical**: No rate limiting on authentication, GraphQL allows unlimited depth/complexity, complete API schema exposed
- **High**: Mass assignment allowing privilege escalation, no pagination limits, excessive data in responses
- **Medium**: Missing rate limiting on non-auth endpoints, verbose error messages, introspection enabled
- **Low**: Suboptimal rate limit values, minor pagination issues, deprecated API versions still active
- **Minimal**: Standard controls sufficient; theoretical risk only; no practical impact

**Constraint**: Do NOT calculate CVSS vector strings. Use qualitative ratings only.

## OWASP API Top 10 Coverage

This skill specifically addresses:
- API1: Broken Object Level Authorization (with sec-authz)
- API3: Broken Object Property Level Authorization (mass assignment)
- API4: Unrestricted Resource Consumption (rate limiting, pagination)
- API6: Unrestricted Access to Sensitive Business Flows
- API9: Improper Inventory Management (shadow APIs)
- API10: Unsafe Consumption of APIs

## Boundaries

- Do NOT perform actual API requests or load testing
- Do NOT modify API code or configuration
- Focus on identifying and documenting issues, not implementing fixes
- Coordinate with sec-authz for object-level authorization issues
- Tag findings with applicable named risk patterns (Control Theater, Incomplete Assessment) when the finding matches a pattern

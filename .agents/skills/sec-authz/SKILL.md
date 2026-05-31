---
user-invocable: false
name: sec-authz
description: >
  Authorization and access control security analysis.
  USE THIS SKILL for: authorization, access control, RBAC, ABAC, permissions, ACL, policy, IDOR, BOLA, tenant isolation, privilege.
  STRIDE: Elevation of Privilege, Information Disclosure.
version: 2.0.0
license: MIT
---

# Authorization & Access Control Analysis

## Purpose

Analyze authorization and access control security concerns identified in the threat model. Focus on Elevation of Privilege and Information Disclosure threats related to what authenticated users can access.

## Scope

- Role-based access control (RBAC) and attribute-based access control (ABAC)
- Object-level authorization (IDOR/BOLA vulnerabilities)
- Function-level authorization (admin endpoints, privileged operations)
- Multi-tenant isolation and tenant boundary enforcement
- Resource ownership validation
- Permission inheritance and delegation
- Default permission policies
- Invite, share, and delegation authorization flows

## Analysis Procedure

1. **Map Authorization Model**: Identify the access control pattern (RBAC, ABAC, ACL, custom)
2. **Check Object-Level Authorization**: Verify every data access validates user can access that specific object — not just "is authenticated" but "owns or is authorized for this specific resource"
3. **Discover Privileged Operations**: Using the endpoint inventory from the threat model (or by searching the codebase directly), systematically identify all privileged and state-changing operations:
   - Enumerate all state-changing endpoints (POST, PUT, PATCH, DELETE methods)
   - Search for endpoints with admin, privileged, approval, management, or configuration keywords in route paths or handler names
   - For each discovered privileged operation: verify it has **role or permission checks**, not just authentication — "authenticated" is not the same as "authorized"
   - Cross-reference with the endpoint inventory to find privileged endpoints with no authorization checks at all
   - This step directly operationalizes OWASP API5 (Broken Function Level Authorization)
4. **Evaluate Function-Level Authorization**: Confirm privileged operations check appropriate roles
5. **Assess Tenant Isolation**: For multi-tenant systems, verify cross-tenant access is impossible at data layer (not just API layer)
6. **Review Default Permissions**: Check if defaults are restrictive (deny-by-default)
7. **Trace Privilege Paths**: Identify how users can escalate privileges (legitimate and illegitimate)
8. **Check Delegation Flows**: Verify invite/share/delegation endpoints validate the recipient identity and tenant membership — not just token validity

## Evidence Categories

Evidence categories relevant to this skill: authorization middleware/code, RBAC/ABAC policy configuration, tenant isolation implementation, row-level security policies, permission model documentation, invite/share flow authorization checks.

## Output Format

For each finding, provide:

```markdown
### {Title}
- **Category**: {Elevation of Privilege | Information Disclosure}
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

- **Critical**: Cross-tenant data access, admin privilege escalation, complete authorization bypass
- **High**: IDOR allowing access to other users' data, missing function-level authorization, delegation flow bypassing tenant checks
- **Medium**: Overly permissive default roles, incomplete ownership validation
- **Low**: Minor permission gaps, authorization logic that's correct but could be clearer
- **Minimal**: Standard controls sufficient; theoretical risk only; no practical impact

**Constraint**: Do NOT calculate CVSS vector strings. Use qualitative ratings only.

## Boundaries

- Do NOT attempt to access resources or test authorization
- Do NOT modify authorization code or policies
- Focus on identifying and documenting issues, not implementing fixes
- This skill focuses on AUTHORIZATION (what can you do), not AUTHENTICATION (who are you)
- Tag findings with applicable named risk patterns (Control Theater, Incomplete Assessment) when the finding matches a pattern

---
user-invocable: false
name: sec-authn
description: >
  Authentication and identity security analysis.
  USE THIS SKILL for: authentication, identity, account takeover, login, session, OAuth, JWT, password.
  STRIDE: Spoofing, Elevation of Privilege.
version: 2.0.0
license: MIT
---

# Authentication & Identity Analysis

## Purpose

Analyze authentication and identity-related security concerns identified in the threat model. Focus on Spoofing and Elevation of Privilege threats.

## Scope

- Authentication mechanisms (password, MFA, OAuth, SAML, JWT)
- Session management
- Identity verification
- Account lifecycle (registration, recovery, deletion)
- Credential storage and transmission
- Multi-tenant authentication boundaries
- Delegation and invite flows

## Analysis Procedure

1. **Auth Coverage Verification**: Using the endpoint inventory from the threat model (or by discovering endpoints directly if not available), verify authentication coverage across the full API surface:
   - Cross-reference all discovered endpoints against auth middleware attachment
   - Identify the auth architecture pattern: **default-deny** (global middleware, public routes explicitly opted out) or **default-allow** (auth applied per-route). Flag default-allow as an architectural risk — any new endpoint is unauthenticated by default
   - For each endpoint without visible auth: classify as **intentionally public** (health check, login, public docs, password reset) or **unintentionally exposed**
   - Flag unintentionally exposed endpoints as **Critical** severity (authentication bypass)
   - Include non-HTTP entry points (WebSocket, gRPC, message queue consumers) where the inventory identifies them
   - **Design-doc fallback**: When the endpoint inventory shows "Auth Status: Unverifiable", note this limitation and recommend code-level review for auth coverage verification
2. **Review Authentication Flow**: Trace the authentication path from user input to session creation. Check for bypasses at each step.
3. **Check Credential Handling**: Verify secure storage (bcrypt/scrypt/argon2 for passwords — flag MD5/SHA1/unsalted hashing) and transmission (TLS)
4. **Evaluate Session Management**: Token generation (cryptographic randomness), expiration, invalidation on logout/password change
5. **Assess JWT Security**: Check algorithm (reject `none`, verify RS256 vs HS256 confusion), validate issuer/audience claims, check token expiry
6. **Check Multi-Tenant Auth Boundaries**: For multi-tenant systems, verify authentication checks tenant membership — trace invite/share/delegation flows for cross-tenant access via token-only authorization
7. **Assess Account Security**: Lockout policies, recovery mechanisms (reset token expiry — flag >1hr), MFA availability
8. **Review Delegation Patterns**: Check invite acceptance, share links, and API key flows — verify the authenticated user is the intended recipient, not just any authenticated user

## Evidence Categories

Evidence categories relevant to this skill: authentication middleware/code, JWT configuration, password hashing configuration, session store configuration, MFA implementation, invite/share flow code, OAuth/SAML configuration.

## Output Format

For each finding, provide:

```markdown
### {Title}
- **Category**: {Spoofing | Elevation of Privilege}
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

- **Critical**: Authentication bypass, credential exposure, session hijacking, cross-tenant auth bypass
- **High**: Weak password policies, missing MFA, privilege escalation, JWT algorithm confusion
- **Medium**: Session fixation risks, insecure token storage, excessive reset token lifetime
- **Low**: Minor session timeout issues, informational leaks
- **Minimal**: Standard controls sufficient; theoretical risk only; no practical impact

**Constraint**: Do NOT calculate CVSS vector strings. Use qualitative ratings only.

## Boundaries

- Do NOT test credentials or attempt authentication
- Do NOT modify authentication code
- Focus on identifying and documenting issues, not implementing fixes
- Tag findings with applicable named risk patterns (Control Theater, Incomplete Assessment) when the finding matches a pattern

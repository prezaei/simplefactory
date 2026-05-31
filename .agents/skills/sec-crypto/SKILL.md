---
user-invocable: false
name: sec-crypto
description: >
  Cryptographic security analysis.
  USE THIS SKILL for: cryptography, encryption, hashing, keys, certificates, TLS, SSL, secrets.
  STRIDE: Information Disclosure, Tampering.
version: 2.0.0
license: MIT
---

# Cryptography Analysis

## Purpose

Analyze cryptographic weaknesses identified in the threat model. Focus on Information Disclosure and Tampering threats.

## Scope

- Encryption algorithms and key sizes
- Hashing algorithms (password storage, integrity)
- Key management and rotation
- Certificate handling and validation
- TLS/SSL configuration
- Random number generation
- Secrets management

## Analysis Procedure

1. **Inventory Crypto Usage**: Identify all cryptographic operations in the codebase
2. **Check Algorithm Strength**: Flag deprecated algorithms (MD5, SHA1 for integrity, DES, RC4, 3DES). Check AES mode — flag ECB (deterministic, leaks patterns), prefer GCM (authenticated encryption)
3. **Verify Password Hashing**: Check for proper key derivation functions (bcrypt, scrypt, argon2). Flag raw SHA-256/SHA-512, unsalted hashing, or low iteration counts
4. **Check JWT Security**: Verify algorithm is explicit (not from token header), flag `none` algorithm acceptance, check for RS256/HS256 confusion (using public key as HMAC secret)
5. **Evaluate Key Management**: Generation (cryptographic RNG), storage (HSM/KMS vs config files), rotation schedule, destruction
6. **Review TLS Configuration**: Protocol versions (flag TLS 1.0/1.1), cipher suites, certificate validation (flag disabled verification)
7. **Assess Secrets Handling**: Hardcoded secrets, secure storage, rotation

## Evidence Categories

Evidence categories relevant to this skill: encryption configuration, TLS/SSL settings, password hashing implementation, key management documentation, JWT signing configuration, secrets manager configuration.

## Output Format

For each finding, provide:

```markdown
### {Title}
- **Category**: {Information Disclosure | Tampering}
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

- **Critical**: Hardcoded secrets, broken encryption (ECB mode on sensitive data), key exposure, JWT `none` algorithm accepted
- **High**: Weak algorithms (MD5 for passwords, SHA1 for signatures), missing certificate validation, JWT algorithm confusion
- **Medium**: Short key lengths, outdated TLS versions (1.0/1.1), low KDF iteration counts
- **Low**: Suboptimal cipher suite ordering, minor configuration issues
- **Minimal**: Standard controls sufficient; theoretical risk only; no practical impact

**Constraint**: Do NOT calculate CVSS vector strings. Use qualitative ratings only.

## Web Search Guidance

For algorithms or configurations where currency matters (TLS versions, cipher suites, algorithm deprecation timelines), use web search to verify current recommendations rather than relying on training data. Cryptographic best practices evolve — cite the source when referencing current guidance.

## Boundaries

- Do NOT attempt to decrypt data or crack keys
- Do NOT modify cryptographic implementations
- Focus on identifying and documenting issues, not implementing fixes
- Tag findings with applicable named risk patterns (Control Theater, Incomplete Assessment) when the finding matches a pattern

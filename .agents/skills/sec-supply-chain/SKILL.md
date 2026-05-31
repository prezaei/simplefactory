---
user-invocable: false
name: sec-supply-chain
description: >
  Supply chain and dependency security analysis.
  USE THIS SKILL for: dependency, supply chain, package, library, npm, pip, Maven, NuGet, vulnerability, CVE.
  STRIDE: Tampering, Elevation of Privilege.
version: 2.0.0
license: MIT
---

# Supply Chain & Dependency Analysis

## Purpose

Analyze supply chain and dependency risks identified in the threat model. Focus on Tampering and Elevation of Privilege threats.

## Scope

- Third-party dependencies (npm, pip, Maven, NuGet, etc.)
- Known vulnerabilities (CVEs)
- Dependency versioning and pinning
- Package integrity verification
- Build pipeline security
- Software Bill of Materials (SBOM)

## Analysis Procedure

1. **Inventory Dependencies**: Catalog direct and transitive dependencies from manifest files (package.json, requirements.txt, pom.xml, etc.)
2. **Check Known Vulnerabilities**: Use web search to look up CVEs for specific package@version combinations. **Do NOT cite CVE numbers from memory or training data.** If web search returns no results, state "No known CVEs found via search" rather than guessing.
3. **Evaluate Version Policies**: Check for pinned versions, range constraints, and update frequency
4. **Review Integrity Checks**: Verify lock files exist and are committed, check for checksum/signature verification
5. **Assess Build Security**: Review CI/CD pipeline for artifact signing, provenance, and supply chain protections (e.g., SLSA)
6. **Check for typosquatting risk**: Review package names for similarity to popular packages

## Evidence Categories

Evidence categories relevant to this skill: dependency manifest files (package.json, requirements.txt), lock files (package-lock.json, yarn.lock), CI/CD pipeline configuration, SBOM documentation, vulnerability scan results, artifact signing configuration.

## Output Format

For each finding, provide:

```markdown
### {Title}
- **Category**: {Tampering | Elevation of Privilege}
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

- **Critical**: Known RCE vulnerabilities, compromised packages
- **High**: High-severity CVEs, missing integrity verification
- **Medium**: Medium-severity CVEs, unpinned dependencies
- **Low**: Outdated but non-vulnerable packages, missing SBOM
- **Minimal**: Standard controls sufficient; theoretical risk only; no practical impact

**Constraint**: Do NOT calculate CVSS vector strings. Use qualitative ratings only.

## Web Search Guidance

Use web search to verify CVE information for dependencies with known version numbers. When citing vulnerabilities:
- Always include the source URL from web search results
- If web search returns no CVE results for a package, state "No known CVEs found via search at time of analysis"
- Do NOT cite CVE numbers from training data — they may be outdated, retracted, or fabricated

## Boundaries

- Do NOT install or execute packages
- Do NOT modify dependency files
- Focus on identifying and documenting issues, not implementing fixes
- Tag findings with applicable named risk patterns (Control Theater, Incomplete Assessment) when the finding matches a pattern

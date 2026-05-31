---
user-invocable: false
name: think-variant-analysis
description: >
  Study past fixes and patches to find similar unfixed vulnerabilities elsewhere in the codebase.
  USE THIS SKILL for: git history, past fix, patch, CVE, variant, incomplete fix, changelog, commit, regression.
  STRATEGY: Variant Analysis.
version: 1.0.0
license: MIT
---

# Variant Analysis

## Purpose

Study past security fixes (via git history, changelogs, or code patterns) to find similar unfixed vulnerabilities elsewhere in the codebase. The "fix one, miss the variant" pattern — a fix applied in one location but not in another location with the same vulnerability.

## Scope

- Git commit history analysis for security-relevant fixes
- Incomplete fix detection (fix in one file, missing in similar code paths)
- Pattern variant search (same dangerous pattern in different locations)
- Changelog and CVE reference analysis
- Regression detection (previously fixed issue reintroduced)

## Analysis Procedure

1. **Check git history availability**:
   - Run `git log --oneline -50` to assess history depth
   - If unavailable: degrade gracefully (see Degraded Mode below)

2. **Find security-relevant commits**:
   - Search for commits with keywords: fix, vuln, security, CVE, bounds, overflow, inject, sanitize, auth, bypass, patch
   - `git log --all --oneline --grep="fix" --grep="security" --grep="CVE" --grep="bounds" --grep="overflow"`
   - Read the most promising commits: `git show {hash}`

3. **Understand what was fixed**:
   - What vulnerability pattern was the fix addressing?
   - What specific code change was made (bounds check added, validation inserted, function replaced)?
   - Where exactly was the fix applied?

4. **Search for unfixed variants**:
   - Look for OTHER locations where the same pre-fix pattern exists
   - Search for calls to the same function, similar buffer operations, parallel code paths
   - Check if the fix was applied consistently across all callers/locations

5. **Inline verification**: Before reporting:
   - Confirm the variant location actually has the same vulnerability pattern
   - Check that no separate fix was applied at the variant location
   - Set Verification Status based on completeness of trace

### Degraded Mode (no git history)

When git history is unavailable:
1. State: "Git history unavailable — degrading to pattern-based variant detection"
2. Search for code patterns that commonly indicate past vulnerability fixes (bounds checks that exist in some locations but not others, inconsistent validation)
3. Look for TODO/FIXME/HACK comments referencing security
4. Findings from degraded mode default to Confidence: Low

## Output Format

For each finding, provide:

```markdown
### {Title}

**Attack Hypothesis**: {What an attacker could achieve via the unfixed variant}

**Reasoning Chain**:
{Narrative: "I found commit {hash} which fixed {vulnerability} by adding {fix}
in {file}. I then searched for other callers of the same function and found
{variant location} which does NOT have the fix applied. The vulnerable
pattern is..."}

**Evidence**: {git commit hash, file:line for fix, file:line for unfixed variant}

| Field | Value |
|-------|-------|
| Reasoning Strategy | Variant Analysis |
| Severity | {Critical · High · Medium · Low · Minimal} |
| Confidence | {High · Medium · Low} — {justification} |
| Verification Status | {Verified · Challenged · Unverified} — {what was checked} |
| Control Status | {Observed · Documented · Not Found} |

**Remediation**: {Apply the same fix pattern to the variant location}
**Proportionality** (High/Critical only): {Why — typically: "the fix already exists for an identical pattern"}
```

## Severity Definitions

- **Critical**: Variant of a known critical fix (RCE, auth bypass) that remains unpatched
- **High**: Variant of a high-severity fix with confirmed exploitable pattern
- **Medium**: Inconsistent security fix application with moderate impact
- **Low**: Minor variant or incomplete fix with limited exploitability
- **Minimal**: Cosmetic inconsistency in fix application with no security impact

## Boundaries

- Do NOT modify code or apply fixes
- Do NOT assume a variant is vulnerable without confirming the pattern matches — the variant location may have different preconditions
- Do NOT read the entire git history — focus on recent and security-relevant commits
- MAY use flow tracing tactics to verify a variant is actually reachable/exploitable
- Report honestly when git history is insufficient: "I could only review N commits"

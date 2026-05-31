---
user-invocable: false
name: think-adversarial-patterns
description: >
  Current red-team techniques, attack chains, and adversarial patterns via web-augmented analysis.
  USE THIS SKILL for: red team, attack chain, LLM abuse, prompt injection, cloud exploitation, identity attack, supply chain attack, lateral movement, privilege escalation, adversarial patterns.
  STRATEGY: Adversarial Patterns.
version: 1.0.0
license: MIT
---

# Adversarial Patterns

## Purpose

Apply current red-team techniques and attack chains to a codebase, using mandatory web search to discover techniques that may not exist in model training data. This skill bridges the gap between static code analysis and the evolving attacker landscape.

## Scope

- LLM/AI abuse patterns (prompt injection, jailbreaking, training data extraction, model inversion)
- Identity-based attack chains (OAuth token theft → lateral movement, session fixation → escalation, consent phishing, device code abuse)
- Cloud exploitation patterns (IMDS credential theft, cross-account role chaining, container escape → host access, service identity abuse)
- Supply chain attack chains (dependency confusion, typosquatting, build pipeline compromise, malicious SDK injection)
- API abuse chains (mass assignment → privilege escalation, BOLA chaining, rate limit bypass → data exfiltration)
- Multi-step attack paths that span domains (identity + cloud, supply chain + LLM)

## Analysis Procedure

1. **Identify applicable red-team domains**: From the codebase characterization provided by attack-reasoning, determine which domains apply:
   - Does the codebase use or integrate AI/LLM models? → AI/LLM domain
   - Does it use OAuth, OIDC, SAML, or session-based auth? → Identity domain
   - Does it deploy to cloud (AWS, Azure, GCP) or use containers? → Cloud domain
   - Does it consume external packages or have CI/CD pipelines? → Supply chain domain
   - Does it expose APIs? → API abuse domain

2. **MANDATORY WEB SEARCH** — For each applicable domain, execute at least one targeted web search to discover current attack techniques:
   - Use technology-specific queries: e.g., "OAuth PKCE downgrade attack 2025", "Kubernetes IMDS exploitation techniques", "LLM prompt injection bypass methods"
   - Do NOT use generic queries like "red team techniques" — be specific to the codebase's stack
   - Minimum: one search per applicable domain
   - If search returns no relevant results for a domain, state: "No current techniques found via search for [domain]"
   - If web search is unavailable, disclose: "Analysis of [domain] relied on training data only — no current web sources available"

3. **Map techniques to attack surface**: For each discovered technique, assess:
   - Is this codebase's implementation susceptible to this technique?
   - What specific code path or configuration would an attacker target?
   - What preconditions must hold for the attack to succeed?

4. **Generate findings**: Each finding must chain: web-sourced technique → codebase evidence → exploitability assessment. Follow the narrative-first finding template.

5. **Inline verification**: Before reporting each finding:
   - Verify the technique applies to this specific technology version and configuration
   - Check for mitigating controls that would prevent the attack
   - If technique applicability is uncertain, set Confidence to Low

## Output Format

For each finding, provide:

```markdown
### {Title}

**Attack Hypothesis**: {What an attacker could achieve using this technique}

**Reasoning Chain**:
{Narrative: "Current red-team research shows [technique] (source: [URL]).
I checked whether the codebase is susceptible by examining [code path].
The implementation [does/does not] include [mitigation]..."}

**Source**: {URL, publication date, technique name}

**Evidence**: {file:line or configuration reference in the codebase}

| Field | Value |
|-------|-------|
| Reasoning Strategy | Adversarial Patterns |
| Severity | {Critical · High · Medium · Low · Minimal} |
| Confidence | {High · Medium · Low} — {justification} |
| Verification Status | {Verified · Challenged · Unverified} — {what was checked} |
| Control Status | {Observed · Documented · Not Found} |

**Remediation**: {Actionable fix}
**Proportionality** (High/Critical only): {Why this fix is proportionate}
```

## Web Search Guidance

**This skill uses PROACTIVE web search — fundamentally different from other skills.**

Existing skills (sec-supply-chain, sec-crypto, sec-infra) use web search REACTIVELY to verify specific claims. This skill uses web search PROACTIVELY to discover current attack techniques and apply them to the codebase.

**Web search is MANDATORY, not optional.** The skill's value comes from applying current industry knowledge. For each applicable red-team domain, execute at least one targeted search before generating findings.

**Anti-hallucination guardrails**:
- Never fabricate CVEs, technique names, tool names, or source URLs
- If web search returns no results, say so — do NOT fall back to inventing techniques
- Cross-reference every web-sourced claim against actual codebase evidence
- Always include the source URL in findings — traceability is non-negotiable
- Prefer established sources: MITRE ATT&CK, PortSwigger, Project Zero, Wiz Research, OWASP, vendor security blogs

## Severity Definitions

- **Critical**: Current attack technique directly exploitable against the codebase with no observed mitigations; known active exploitation in the wild
- **High**: Current technique applicable to the technology stack with incomplete mitigations
- **Medium**: Technique applicable but requires specific preconditions or chaining with other weaknesses
- **Low**: Technique theoretically applicable but strong mitigations observed or preconditions unlikely
- **Minimal**: Technique documented for the technology but no evidence of susceptibility in this codebase

## Boundaries

- Do NOT fabricate attack techniques or CVEs — every technique must have a verifiable source
- Do NOT generate findings without codebase evidence — a technique existing is not a finding; the codebase must be susceptible
- Do NOT duplicate findings that sec-* specialist skills would produce — focus on attack CHAINS and current techniques, not individual control gaps
- Do NOT skip web search — this skill without web search is just training-data pattern matching, which other skills already do
- MAY use tactics from other strategies (trace a data flow, check git history) when investigating whether a technique applies
- Focus on techniques that are CURRENT and RELEVANT to the detected technology stack

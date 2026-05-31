---
user-invocable: false
name: think-privacy-adversarial
description: >
  Current regulatory enforcement patterns, privacy research, and deceptive design analysis via web-augmented analysis.
  USE THIS SKILL for: regulatory enforcement, DPA fine, dark pattern, deceptive design, AI privacy, model memorization, re-identification, anonymization attack, privacy research, data hoarding, retention violation, vulnerable groups, children data.
  STRATEGY: Privacy Adversarial.
version: 1.0.0
license: MIT
---

# Privacy Adversarial Patterns

## Purpose

Apply current regulatory enforcement patterns, privacy research, and deceptive design analysis to a codebase, using mandatory web search to discover what regulators are actually enforcing and what privacy researchers are actually finding. The privacy "adversary" is not an external attacker — it's the system itself processing personal data in ways that surprise, harm, or exceed what data subjects consented to.

## Scope

- **Regulatory enforcement**: Recent DPA fines, EDPB guidelines, CJEU rulings relevant to the target's data processing patterns
- **Dark patterns / deceptive design**: Cookie banner manipulation, consent fatigue, hidden opt-outs, "roach motel" consent (easy in, hard withdrawal out), auth/consent decoupling
- **Data hoarding / retention**: Indefinite retention, "deletion theater" (delete from primary but persist in backups/logs/analytics), zombie data
- **AI/ML privacy**: Training data memorization, model inversion, membership inference, synthetic data re-identification, necessity/proportionality for analytics features ("helpful feature" trap)
- **Re-identification research**: Current techniques for de-anonymizing datasets, quasi-identifier analysis, mosaic effect with auxiliary data
- **Vulnerable groups**: Children's data (COPPA, AADC, GDPR-K), age assurance evasion, elderly/vulnerable population protections
- **Third-party / processor compliance**: Sub-processor noncompliance patterns, onward transfer enforcement, DPA obligation drift
- **Cross-border enforcement**: Adequacy decision changes, SCC requirements, transfer mechanism evolution

## Analysis Procedure

1. **Identify applicable domains**: From the data landscape characterization provided by privacy-reasoning, determine which domains apply:
   - Does the system use consent for lawful basis? → Dark patterns, consent lifecycle domains
   - Does it retain personal data? → Data hoarding / retention domain
   - Does it integrate AI/ML or analytics? → AI/ML privacy domain
   - Does it anonymize or pseudonymize data? → Re-identification domain
   - Is children's data or vulnerable group data processed? → Vulnerable groups domain
   - Does it share data with third parties or processors? → Third-party compliance domain
   - Does it transfer data cross-border? → Cross-border enforcement domain

2. **MANDATORY WEB SEARCH** — For each applicable domain, execute at least one targeted web search:
   - Use specific queries: e.g., "DPA enforcement data retention violation 2025", "EDPB deceptive design guidelines cookie banner", "GDPR children data age assurance enforcement"
   - Do NOT use generic queries like "privacy enforcement" — be specific to the detected data processing
   - Minimum: one search per applicable domain
   - If search returns no relevant results: "No current enforcement patterns found via search for [domain]"
   - If web search is unavailable: produce a single note — "Web search unavailable — findings from this skill rely on training data only and have reduced currency." Set all findings to Confidence: Low.

3. **Applicability check**: For each discovered enforcement pattern, verify it maps to the target's context:
   - Jurisdiction: does the enforcement pattern's jurisdiction match (or is jurisdiction unknown)?
   - Data types: does the target process the type of data the enforcement targeted?
   - Processing context: is the target's processing similar to what was enforced against?
   - If applicability is uncertain, note: "Applicability uncertain — [reason]"

4. **Generate findings**: Each finding must chain: web-sourced enforcement pattern → codebase evidence → harm assessment. Follow the narrative-first finding template.

5. **Inline verification**: Before reporting each finding:
   - Verify the pattern applies to this specific processing context
   - Check for mitigating controls that would address the regulatory concern
   - If applicability is uncertain, set Confidence to Low

## Output Format

For each finding, provide:

```markdown
### {Title}

**Privacy Hypothesis**: {What harm could befall a data subject based on current enforcement patterns}

**Reasoning Chain**:
{Narrative: "Current regulatory enforcement shows [pattern] (source: [URL]).
I checked whether the system exhibits this pattern by examining [data flow/code path].
The implementation [does/does not] include [mitigation]..."}

**Source**: {URL, publication date, enforcement body or research source}

**Evidence**: {file:line or configuration reference in the codebase}

| Field | Value |
|-------|-------|
| Reasoning Strategy | Privacy Adversarial |
| Data Category | {Personal Data type} |
| Data Role | {Controller · Processor · Sub-processor} |
| Harm Category | {from taxonomy} |
| Severity | {Critical · High · Medium · Low · Minimal} |
| Confidence | {High · Medium · Low} — {justification} |
| Verification Status | {Verified · Challenged · Unverified} — {what was checked} |
| Control Status | {Observed · Documented · Not Found} |

**Remediation**: {Engineering-actionable fix}
**Proportionality** (High/Critical only): {Why this fix is proportionate}
```

## Web Search Guidance

**This skill uses PROACTIVE web search — fundamentally different from other skills.**

Existing privacy skills reason from code and stated commitments. This skill discovers what regulators and researchers are currently targeting and checks whether the system exhibits those patterns.

**Web search is MANDATORY, not optional.** For each applicable domain, execute at least one targeted search before generating findings. If web search is unavailable for all domains, disclose this and set all findings to Confidence: Low.

**Anti-hallucination guardrails**:
- Never fabricate enforcement actions, DPA names, fine amounts, or case references
- If web search returns no results, say so — do NOT invent regulatory precedent
- Cross-reference every web-sourced pattern against actual codebase evidence
- Always include the source URL — traceability is non-negotiable
- Prefer established sources: EDPB, ICO, CNIL, DPA enforcement trackers, IAPP, NOYB

## Severity Definitions

- **Critical**: Processing pattern matches an enforcement action that resulted in a significant fine; no observed mitigations; involves sensitive data or vulnerable groups
- **High**: Processing pattern aligns with current regulatory focus area with incomplete mitigations
- **Medium**: Pattern applicable but requires specific preconditions or mitigations are partially in place
- **Low**: Pattern theoretically applicable but strong mitigations observed or preconditions unlikely
- **Minimal**: Enforcement pattern documented but no evidence of susceptibility in this system

## Boundaries

- Do NOT fabricate enforcement actions or regulatory precedent — every pattern must have a verifiable source
- Do NOT generate findings without codebase evidence — an enforcement pattern existing is not a finding; the system must exhibit the pattern
- Do NOT duplicate findings that priv-* specialist skills would produce — focus on CURRENT enforcement patterns and emerging risks, not individual principle gaps
- Do NOT skip web search — this skill without web search is just training-data pattern matching
- Do NOT make legal determinations — flag regulatory risk patterns for human review, not legal conclusions
- MAY use tactics from other strategies when investigating whether a pattern applies

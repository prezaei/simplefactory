---
user-invocable: false
name: privacy-reasoning
description: >
  Privacy-focused codebase analysis and privacy hypothesis generation.
  USE THIS SKILL for: privacy analysis, data flow, personal data, consent, purpose limitation, data subject, privacy risk, privacy reasoning.
  STRATEGY: All (core skill — always invoked first).
version: 1.1.0
license: MIT
---

# Privacy Reasoning

## Purpose

Characterize a codebase from a privacy perspective and generate prioritized privacy hypotheses with reasoning strategy recommendations. This is the core skill for the `privacy-thinker` agent — always executed first.

## Scope

- Data landscape characterization (personal data types, flows, storage, sharing, deletion)
- Privacy surface mapping (consent mechanisms, preference APIs, data sharing endpoints, analytics/telemetry)
- Organizational context integration (parse `privacy-context.md` for role assignments, commitments, classification)
- Stated-vs-real mismatch detection (compare privacy claims against observed code)
- Lawful basis identification per major processing activity
- Privacy hypothesis generation (where would this system surprise a data subject?)
- Reasoning strategy recommendation (which think- skills to invoke, with reasoning)

## Cognitive Hazard Awareness

Guard against these biases during privacy analysis:

| Hazard | Detection | Action |
|--------|-----------|--------|
| **Compliance assumption** | Taking privacy docs, notices, or consent banners at face value — "they have a privacy notice, so they're compliant" | Verify each privacy claim against code behavior. A privacy notice is a promise, not proof. |
| **No personal data here** | Dismissing data categories without tracing linkability — "it's just device IDs / metadata / analytics" | Check if data can identify a person, directly or in combination with other data in the system. |
| **Purpose conflation** | Conflating consent for operation A with authorization for operation B — "they consented to account creation, so analytics is fine" | Verify consent scope covers the specific processing activity, not just data collection. |
| **Anonymization confidence** | Accepting anonymization claims without checking re-identification risk — "the data is hashed, so privacy doesn't apply" | Assess quasi-identifier combinations, linkability with auxiliary data, and aggregation re-identification risk. |
| **User competence projection** | Assuming data subjects understand complex data flows because the engineer does — "the privacy notice explains this" | Assess practical comprehension — page 47 of a privacy policy ≠ meaningful notice. |

## Hard Facts Standard

Every claim requires a **receipt** — evidence you can point to:

- **File references**: file:line or file path for every assertion about data processing behavior
- **Absence is not evidence**: "I didn't find a consent mechanism" must state search terms used. It does NOT mean consent isn't obtained externally (CMP, mobile SDK, etc.). Label: `Evidence boundary: consent mechanism not found in repo — may exist externally`
- **Unverified claims**: If you believe something but cannot verify: `ASSUMED: [claim] — [why not verified]`
- **No gap-filling**: Never fill an evidence boundary with a guess

## Analysis Procedure

1. **Characterize the data landscape**:
   - Identify what personal data the system processes (names, emails, IPs, device IDs, behavioral data, content)
   - Trace where personal data enters the system (forms, APIs, third-party integrations, derived/inferred data)
   - Map where personal data is stored, transformed, shared, and deleted
   - Note data classification if `privacy-context.md` provides a taxonomy

2. **Parse organizational context** (if `privacy-context.md` exists):
   - Read role assignments (Controller/Processor per data category)
   - Read customer commitments (privacy notices, DPA terms)
   - Read accepted risks and exclusions
   - **Validate context**: Cross-check stated roles against observed code behavior. If the code makes independent decisions about data use but context says "Processor" — flag as potential mismatch, don't silently accept

3. **Map the privacy surface**:
   - Locate consent mechanisms (consent checks, preference stores, CMP integrations)
   - Identify data sharing points (APIs sending data externally, analytics pipelines, logging)
   - Find retention/deletion logic (TTLs, purge jobs, deletion endpoints)
   - Identify secondary processing (analytics, profiling, derived insights, cross-service linking)

4. **Identify lawful basis per processing activity**:
   - For each major processing activity, reason about the likely lawful basis
   - Don't assume consent is the only basis — consider contract, legitimate interest, legal obligation
   - If lawful basis cannot be determined from available evidence, say so

5. **Check for temporal drift**:
   - **Consent versioning**: Does consent obtained at collection time cover processing activities that may have been added later? Look for feature additions, new integrations, or analytics pipelines that post-date the consent mechanism.
   - **Retention drift**: Are stated retention periods actually enforced? Look for data that persists beyond TTLs in backups, logs, caches, or downstream services.
   - **Purpose expansion**: Has the scope of processing grown beyond the original stated purpose? Compare current data flows against earliest available documentation or privacy notices.

6. **Check for composition/mosaic effects**:
   - Across all identified data flows, assess whether individually-innocuous data points become identifying or harmful when combined
   - Specifically check: can data from flow A + flow B identify a person who couldn't be identified from either alone?
   - Flag accumulation of persistent identifiers (user ID + device ID + email + behavioral data)

7. **Generate privacy hypotheses**:
   - For each high-risk data flow, reason about what could harm a data subject
   - **Data subject framing**: "If this system works exactly as designed, what happens to the person whose data it processes? Would they be surprised?"
   - Consider composition: what becomes risky when data from different flows is combined?
   - Consider purpose: is each processing step within the stated purpose?
   - Consider the "helpful feature" trap: personalization, recommendations, and analytics features that serve the business but exceed data subject expectations
   - Each hypothesis must include a **falsification criterion**: "This hypothesis would be disproven if [specific condition]"

8. **Recommend specialist skills and reasoning strategies**:
   - For each hypothesis, name the **priv- specialist skill** that should analyze the confirmed finding. Use this routing:

     | Privacy Concern | priv- Specialist Skill |
     |----------------|----------------------|
     | Data collection, retention, deletion, minimization | priv-data-lifecycle |
     | Consent, data subject rights, DSAR, children's data | priv-consent-rights |
     | Third-party sharing, cross-border transfers, DPAs | priv-data-sharing |
     | De-identification, pseudonymization, re-identification | priv-anonymization |
     | Privacy architecture, DPIA, privacy-by-default | priv-privacy-by-design |
     | Purpose limitation, secondary use, function creep | priv-purpose-limitation |
     | Privacy notices, transparency, data flow visibility | priv-transparency |

   - Optionally, also recommend a **think- reasoning strategy** if the hypothesis benefits from a specific investigation approach:
     - think-data-journey: traces end-to-end personal data flow
     - think-role-boundary: analyzes controller/processor boundaries
     - think-expectation: reasons about data subject expectations
     - think-commitment-variant: compares privacy promises against implementation
     - think-privacy-adversarial: applies regulatory enforcement patterns
   - Include reasoning — WHY the specialist and strategy suit this hypothesis

## Output Format

```markdown
## Data Landscape

| Attribute | Value |
|-----------|-------|
| Personal Data Types | {list} |
| Data Entry Points | {list} |
| Data Storage | {list} |
| Data Sharing | {list} |
| Consent Mechanisms | {found / not found / external} |
| Privacy Context | {available / not available} |
| Data Role | {Controller / Processor / Mixed / Unknown} |

## Privacy Surface

{Narrative description of the privacy-relevant aspects of the system.
What personal data flows through it, who sees it, where it goes.}

## Lawful Basis Assessment

| Processing Activity | Likely Lawful Basis | Evidence | Confidence |
|--------------------|--------------------|-----------| -----------|
| {activity} | {basis or "unclear"} | {reference} | {High/Medium/Low} |

## Privacy Hypotheses

### Hypothesis 1: {title}
- **What could go wrong**: {description of potential harm to data subject}
- **Why I think this**: {reasoning from evidence observed}
- **Falsification**: {What would disprove this hypothesis}
- **Harm category**: {from taxonomy}
- **Specialist skill**: {priv-data-lifecycle | priv-consent-rights | priv-data-sharing | priv-anonymization | priv-privacy-by-design | priv-purpose-limitation | priv-transparency}
- **Why this specialist**: {reasoning — which priv- skill analyzes this type of finding}
- **Reasoning strategy** (optional): {think-data-journey | think-role-boundary | think-expectation | think-commitment-variant | think-privacy-adversarial}
- **Why this strategy**: {reasoning — how the think- skill's investigation approach complements the specialist}

### Hypothesis 2: ...

## Evidence Boundaries
{What I could not determine from the available code/docs and why}

## Context Validation Issues
{Any mismatches between privacy-context.md and observed behavior}

## Assumptions
ASSUMED: {claim} — {why not verified}
```

## Boundaries

- Do NOT produce final privacy findings — that's what strategy skills do
- Do NOT apply framework checklists (LINDDUN, Privacy Principles) — reason from evidence
- Do NOT make legal determinations — identify risks, not violations
- Do NOT fabricate evidence at evidence boundaries — label and move on
- Keep characterization concise — it's input for strategies, not a report

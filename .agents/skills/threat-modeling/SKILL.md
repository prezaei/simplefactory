---
user-invocable: false
name: threat-modeling
description: >
  Systematic threat identification using STRIDE, OWASP Top 10, MITRE ATT&CK, and Zero Trust frameworks.
  USE THIS SKILL for: threat, risk, attack, vulnerability, security analysis, privacy threat, privacy risk, privacy analysis.
  STRIDE: All categories.
  PRINCIPLES: All (when invoked by privacy-engineer).
version: 2.1.0
license: MIT
---

# Threat Modeling

## Purpose

Perform systematic threat identification using a unified framework approach. This skill is always executed first and produces structured output that triggers specialist skills.

## Scope

- Design documents and architecture specifications
- API specifications and schemas
- Source code repositories
- Infrastructure-as-Code configurations
- Data flow diagrams and system documentation

## Frameworks

Apply these security frameworks in a unified approach:

| Framework | Purpose | Application |
|-----------|---------|-------------|
| **STRIDE** | Threat categorization | Primary taxonomy for threat identification |
| **OWASP Top 10** | Web vulnerability patterns | Vulnerability assessment |
| **MITRE ATT&CK** | Attack patterns and TTPs | Attack pattern analysis |
| **Zero Trust** | Architecture validation | Trust boundary and access validation |

## Analysis Modes

### Fast Pass Mode

Quick risk identification for time-sensitive reviews. Execute these 3 phases:

| Phase | Description | Output |
|-------|-------------|--------|
| 1. Asset & Trust Boundary Identification | What are we protecting? Where are the boundaries? | Asset inventory, boundary map |
| 2. Threat Surface Scan | Quick pass across STRIDE + OWASP + Zero Trust violations | Threat list with categories |
| 3. Critical Findings Summary | Top risks with severity ratings | Prioritized risk table |

### Comprehensive Mode

Full security review for thorough assessment. Execute these 7 phases:

| Phase | Description | Output |
|-------|-------------|--------|
| 1. Asset & Data Flow Mapping | Components, data flows, trust boundaries | Data flow diagram (textual) |
| 2. Threat Modeling (STRIDE) | Systematic threat identification | Threat model table |
| 3. Attack Pattern Analysis (MITRE ATT&CK) | Map to known TTPs | ATT&CK mapping |
| 4. Vulnerability Assessment (OWASP) | Common weakness patterns | Vulnerability findings |
| 5. Zero Trust Validation | Identity, access, network segmentation | Trust validation results |
| 6. Risk Prioritization | Severity × Likelihood matrix | Risk matrix |
| 7. Mitigation Recommendations | Actionable remediation plan | Remediation roadmap |

## Output Format

For each identified threat, output:

```markdown
### Threat: {title}
- **Category**: {STRIDE category}
- **Summary**: {brief description}
- **Details**: {full explanation}
- **Tags**: {keywords for skill matching}
- **Severity**: {Critical | High | Medium | Low}
- **Confidence**: {High | Medium | Low} ({justification})
```

**Severity Ratings** (CVSS v3.1 Qualitative):
- **Critical**: System compromise, remote code execution, or auth bypass
- **High**: Significant data loss or privilege escalation
- **Medium**: Partial impact or requires user interaction (e.g., XSS)
- **Low**: Minor info leak or best-practice violation

**Constraint**: Do NOT calculate numeric CVSS vector strings (e.g., `AV:N/AC:L...`). Use qualitative definitions only.

**Confidence Levels**:
- **High**: Clear evidence, well-understood pattern
- **Medium**: Likely issue, some ambiguity
- **Low**: Possible concern, needs human verification

Always include justification for confidence rating.

The threat model output should also include an **### API Surface Inventory** section containing the endpoint inventory table produced during API Surface Enumeration (step 1b of the Analysis Procedure). This section appears after the individual threat entries.

## Terminology

Use *Personal Data* and *Sensitive Personal Data* for data classification. The term "PII" is treated as a legacy subset within *Personal Data* for compatibility with older sources only.

## Analysis Procedure

1. **Identify Assets**: Enumerate components, data stores, and sensitive resources

   **1b. API Surface Enumeration** (mandatory for codebase analysis):
   - Detect the web framework(s) in use (Express, Django, Flask, FastAPI, ASP.NET, Spring Boot, Go net/http, Rails, etc.)
   - Search the codebase for route/endpoint registration patterns appropriate to the detected framework (e.g., `app.get`, `router.post`, `@app.route`, `@RequestMapping`, `MapGet`, `HandleFunc`)
   - Where recognizable, also search for non-HTTP entry points: WebSocket handlers, gRPC service definitions, message queue consumers, webhook receivers
   - Produce a compact **endpoint inventory** table:
     ```
     | Route | Method | Auth Middleware Visible | Notes |
     ```
   - Identify the **auth architecture pattern**:
     - Default-deny: global auth middleware applied, public routes explicitly opted out
     - Default-allow: auth applied per-route, meaning any forgotten route is unauthenticated
   - For each endpoint with no visible auth middleware, generate a **Spoofing threat** with tags including "authentication", "bypass", "unauthenticated endpoint" to ensure `sec-authn` triggers downstream

   **Design-doc-only fallback**: When analyzing a design document without source code, extract the endpoint inventory from the document's API section or architecture description. Flag all endpoints as "Auth Status: Unverifiable from design doc" and include a note recommending code-level review for auth coverage.

   **Limitations**: Static endpoint enumeration may miss dynamically registered routes (e.g., reflection-based registration, runtime route building, feature-flag-gated endpoints). If the framework is not recognized, fall back to searching for common HTTP handler patterns (`listen`, `serve`, `handle`, `route`). Note incomplete coverage in the output.

2. **Map Trust Boundaries**: Define where trust levels change (e.g., user→API, API→database)
3. **Enumerate Data Flows**: Trace how data moves between components
4. **Apply STRIDE**: For each component/flow, systematically check each STRIDE category
5. **Check OWASP Patterns**: Cross-reference against OWASP Top 10 weakness patterns
6. **Map to ATT&CK**: Identify relevant MITRE ATT&CK techniques where applicable
7. **Validate Zero Trust**: Check identity verification, least privilege, assume breach principles
8. **Prioritize Risks**: Assess severity and likelihood for each identified threat
9. **Document Findings**: Output each threat using the required format

## Severity Definitions

| Level | Criteria | Examples |
|-------|----------|----------|
| **Critical** | System compromise, remote code execution, authentication bypass | RCE via deserialization, auth bypass via JWT none algorithm |
| **High** | Significant data loss or privilege escalation | SQL injection with data exfil, vertical privilege escalation |
| **Medium** | Partial impact or requires user interaction | Stored XSS, CSRF on sensitive action |
| **Low** | Minor information leak or best-practice violation | Version disclosure, missing security headers |

## Identifying Specialist Skills

After completing threat identification, analyze the threats to determine which specialist skills should be invoked. For each threat:

1. Note its STRIDE category
2. Extract relevant keywords from summary/details
3. These will be matched against specialist skill triggers

Specialist skills are discovered from `.agents/skills/sec-*/SKILL.md` and triggered based on:
- STRIDE category overlap: each sec- skill lists its covered STRIDE categories in its `description` field (e.g., `STRIDE: Tampering, Denial of Service`). Match the threat's STRIDE category against these description-embedded categories.
- Keyword match (case-insensitive) against the skill's `description` field and the threat's summary/details/tags

**Specialist routing table** (derived from sec- skill descriptions):

| STRIDE Category | Primary sec- Skill(s) |
|----------------|----------------------|
| Spoofing | sec-authn |
| Tampering | sec-injection, sec-api, sec-crypto, sec-infra, sec-supply-chain |
| Repudiation | sec-logging |
| Information Disclosure | sec-data, sec-crypto, sec-authz, sec-api, sec-infra |
| Denial of Service | sec-api, sec-infra |
| Elevation of Privilege | sec-authz, sec-authn, sec-infra, sec-supply-chain |

## Privacy Threat Modeling (when invoked by privacy-engineer)

**Note**: When invoked by `privacy-engineer`, skip the auth coverage verification sub-steps of API Surface Enumeration (auth middleware checks, auth architecture pattern, Spoofing threat generation for unauth endpoints). Still enumerate endpoints for data flow mapping purposes.

When invoked by the `privacy-engineer` agent, this skill adapts its approach:

### Privacy Analysis Mode

Instead of STRIDE, produce a **principle-based** privacy threat model using these 10 privacy principles as the primary taxonomy:

| Principle | What to Check |
|-----------|--------------|
| Data Minimization | Is the system collecting only data necessary for stated purposes? |
| Purpose Limitation | Is collected data used only for disclosed purposes? |
| User Control | Can users access, export, correct, and delete their personal data? |
| Transparency | Are data collection, use, and sharing practices clearly disclosed? |
| Consent & Choice | Where consent is the justification, is it freely given, specific, informed, and revocable? |
| Data Sharing Boundaries | Is sharing limited to what's disclosed? Are third-party obligations documented? |
| Retention Limits | Is data retained only as long as necessary? Are deletion mechanisms in place? |
| Children's Data | Are age-gating and parental consent mechanisms present where applicable? |
| Cross-border Safeguards | Are adequate transfer mechanisms in place for international data flows? |
| Security of Personal Data | Are technical/organizational measures proportionate to data sensitivity? |

### LINDDUN Sweep

After the principle-based analysis, run a targeted **LINDDUN sweep** for non-obvious threats:

- **Linkability**: Can data be correlated across subsystems or with external data? **Identifier accumulation check**: Does the system combine 4+ persistent identifiers (e.g., user ID + device ID + email + advertising ID) that could enable comprehensive user profiling? Classify identifiers by persistence tier: persistent cross-service (highest risk) > persistent single-service > session-scoped > derived/ephemeral. Flag systems exceeding 4+ persistent identifiers as creating correlation attack surface.
- **Identifiability**: Can "anonymized" or "aggregated" data be re-identified via quasi-identifier combinations?
- **Detectability**: Can an attacker infer the existence of sensitive records without accessing content?
- **Non-repudiation** (privacy inversion): Do logs/audit trails prove actions users should be able to deny? **CRITICAL: This is the INVERSE of security's Non-repudiation. In STRIDE, non-repudiation is a GOAL. In LINDDUN, it is a THREAT.**

LINDDUN sweep findings are tagged with the LINDDUN category but categorized under the most relevant privacy principle.

### Named Risk Detection Patterns

During privacy analysis, also check for these empirically-derived risk patterns:

| Pattern | What to Look For | Tag When Found |
|---------|-----------------|----------------|
| **Control Theater** | Controls claimed in documentation but no implementation evidence observed in code/config/architecture | `Control Theater` |
| **Silent Data Flow** | Data flowing to downstream services or cross-product boundaries not documented in privacy notices or stated purposes | `Silent Data Flow` |
| **Incomplete Assessment** | Insufficient information to assess key privacy questions (what data, what purpose, what legal justification, what retention, what sharing) | `Incomplete Assessment` |

Identifier Accumulation is detected via the Linkability sweep above and tagged as `Identifier Accumulation`.

### Privacy Threat Output Format

```markdown
### Threat: {title}
- **Principle**: {privacy principle}
- **Summary**: {brief description}
- **Details**: {full explanation}
- **Tags**: {keywords for skill matching}
- **LINDDUN Tag**: {Linkability | Identifiability | Non-repudiation | Detectability | — if not from sweep}
- **Severity**: {Critical | High | Medium | Low | Minimal}
- **Confidence**: {High | Medium | Low} ({justification})
- **Control Status**: {Observed | Documented | Not Found}
- **Evidence Expected**: {evidence type — UI artifact, configuration, API/code, documentation, test/verification}
- **Risk Pattern**: {Control Theater | Silent Data Flow | Identifier Accumulation | Incomplete Assessment | —}
```

**Privacy Severity Ratings** (privacy-specific, not CVSS):
- **Critical**: Mass personal data exposure, processing without any lawful basis, complete absence of data subject rights mechanisms
- **High**: Systematic processing without DPIA, no deletion capability, sensitive personal data without enhanced protections
- **Medium**: Incomplete data subject rights implementation, excessive data collection, purpose creep
- **Low**: Documentation gaps, policy-implementation mismatches, best-practice deviations with limited user impact
- **Minimal**: Standard controls sufficient, limited privacy impact, theoretical risk only

**Constraint**: Do NOT calculate numeric risk scores. Use qualitative definitions only.

### Severity Escalation Triggers

After producing findings, check whether any of these trigger conditions are present and tag applicable findings:

| Trigger Condition | Severity Floor | Applies To | Tag |
|-------------------|---------------|------------|-----|
| Children's data present | High | Consent, DSR, Purpose Limitation gaps | `Escalation: Children's Data` |
| Sensitive personal data without explicit consent | High | Consent & Choice, Transparency findings | `Escalation: Sensitive Data` |
| Personal data used for AI/ML model training | Medium | Purpose Limitation, Consent & Choice, Data Minimization | `Escalation: AI/ML Training` |

For each finding where a trigger applies, add the escalation tag. Downstream specialist skills use these tags to enforce severity floors. If the analyst assesses a lower severity than the floor, the finding must include: "Severity floor default is {floor} due to {trigger}. Assessed as {actual} because: {justification}."

### Identifying Privacy Specialist Skills

After completing privacy threat identification, analyze the threats to determine which specialist skills should be invoked. For each threat:

1. Note its privacy principle
2. Extract relevant keywords from summary/details
3. These will be matched against specialist skill triggers

Privacy specialist skills are discovered from `.agents/skills/priv-*/SKILL.md` and triggered based on:
- Privacy principle overlap with skill's `PRINCIPLES` list
- Keyword match (case-insensitive) in summary/details/tags

## Boundaries

- Do NOT perform runtime security testing
- Do NOT execute code or make changes to the codebase
- Do NOT provide compliance certifications
- Focus on identifying and documenting threats, not fixing them

---
user-invocable: false
name: attack-reasoning
description: >
  Adversarial codebase analysis and attack hypothesis generation.
  USE THIS SKILL for: security analysis, attack surface, threat hypothesis, codebase characterization, adversarial reasoning, vulnerability discovery.
  STRATEGY: All (core skill — always invoked first).
version: 1.1.0
license: MIT
---

# Attack Reasoning

## Purpose

Characterize a codebase from an adversarial perspective and generate prioritized attack hypotheses with reasoning strategy recommendations. This is the core skill for the `security-thinker` agent — always executed first.

## Scope

- Codebase characterization (language, paradigm, maturity, security surface)
- Attack surface mapping (trust boundaries, entry points, privilege operations)
- Attack hypothesis generation (what would an attacker target and why)
- Stated-vs-real behavior mismatch detection
- Reasoning strategy recommendation (which think- skills to invoke, with reasoning)

## Cognitive Hazard Awareness

Guard against these biases during analysis — they compromise investigation quality:

| Hazard | Detection | Action |
|--------|-----------|--------|
| **Inherited certainty** | Taking any claim from the user's problem statement or design docs at face value | Treat every claim as a testable hypothesis. The user's description is input, not axiom. |
| **Confirmation bias** | All evidence found supports the first hypothesis; no disconfirming evidence sought | Explicitly search for evidence that would REFUTE the leading hypothesis before committing. |
| **Hasty commitment** | About to report a hypothesis after finding one supporting data point | Generate at least one alternative hypothesis. If the alternative is equally plausible, investigate both. |

## Hard Facts Standard

Every claim in the output requires a **receipt** — evidence you can point to:

- **File references**: file:line or file path for every assertion about code behavior
- **Absence is not evidence**: "I didn't find X" must state the search terms used. It means the search was incomplete or the data doesn't exist in the searched location — NOT that X doesn't exist.
- **Unverified claims**: If you believe something but cannot verify it, label it: `ASSUMED: [claim] — [why not verified]`
- **No gap-filling**: If you can't verify something, say "I could not verify X" — never fill the gap with a guess

## Analysis Procedure

1. **Characterize the codebase**:
   - Identify language(s), framework(s), paradigm (monolith, microservices, library)
   - Assess maturity: greenfield vs established, test coverage indicators, documentation quality
   - Check for git history availability (depth, recent security-relevant commits)
   - Note available context: config files, API specs, dependency manifests

2. **Map the attack surface**:
   - **Entry point census** (mandatory): Perform a systematic codebase search for all route/endpoint registrations — this search does NOT count against the ~10 file read budget (it is a targeted grep, not a full file read). Detect the web framework(s) in use and search for framework-specific route patterns (e.g., `app.get`, `router.post`, `@app.route`, `@RequestMapping`, `HandleFunc`). Where recognizable, also search for non-HTTP entry points: WebSocket handlers, gRPC service definitions, message queue consumers.
   - Produce a compact **entry point list** in the output, noting for each entry point whether auth middleware is visibly attached
   - Identify the **auth architecture pattern**: default-deny (global middleware) vs default-allow (per-route). Flag default-allow as higher risk.
   - Map trust boundaries (where trust levels change: user→API, API→database, service→service)
   - Locate privilege operations (authentication, authorization, data access, admin functions)
   - Identify component interactions that could create emergent risks
   - **Design-doc-only fallback**: When analyzing a design document without source code, extract entry points from the document's API section or architecture description. Flag auth status as "Unverifiable from design doc."
   - **Limitation**: Static endpoint enumeration may miss dynamically registered routes (reflection, runtime route building, feature flags). Note incomplete coverage if framework is unrecognized.

3. **Detect stated-vs-real mismatches**:
   - If `security-context.md`, design docs, or code comments make security claims (e.g., "all endpoints require auth", "data is encrypted at rest"), compare against observed code behavior
   - Flag mismatches with tag `Model-Reality Mismatch` — these are high-value findings because they indicate the team's mental model diverges from reality
   - If no claims are available to compare, note: "No stated security model found to compare against"

4. **Generate attack hypotheses**:
   - For each high-value target, reason about HOW an attacker could reach it
   - **Attacker goal framing**: "If I wanted to [steal data / escalate privileges / abuse an LLM / compromise the supply chain], what would I try?"
   - **Abuse path enumeration**: Not just what's vulnerable, but how an attacker chains steps — entry point → intermediate exploitation → objective
   - **Trust invariant challenges**: "What would break if [network isolation / auth middleware / input validation / dependency integrity] fails?"
   - Prioritize by potential impact, not by vulnerability category
   - Consider composition risks: what could go wrong because of how components are wired together?
   - Consider AI/LLM misuse scenarios if the codebase integrates AI models

5. **Recommend reasoning strategies and specialist skills**:
   - For each hypothesis, recommend which think- strategy skill(s) would be most productive for investigation
   - Include reasoning for each recommendation — WHY this strategy suits this hypothesis
   - Consider codebase characteristics: variant analysis needs git history, algo-reasoning needs custom logic, flow-tracing needs input paths
   - **Also name which sec- specialist skill(s)** should analyze the confirmed findings. Use this routing:

     | Attack Category | sec- Specialist Skill |
     |----------------|----------------------|
     | API/endpoint attacks, rate limiting, mass assignment | sec-api |
     | Authentication bypasses, session hijacking, credential theft | sec-authn |
     | Authorization flaws, IDOR, privilege escalation | sec-authz |
     | Crypto weaknesses, key management, TLS issues | sec-crypto |
     | Data exposure, sensitive data leakage | sec-data |
     | Infrastructure, container escapes, IaC misconfig | sec-infra |
     | Injection (SQL, command, XSS, SSRF, template) | sec-injection |
     | Logging gaps, audit trail tampering | sec-logging |
     | Supply chain, dependency vulnerabilities | sec-supply-chain |

## Output Format

```markdown
## Codebase Characterization

| Attribute | Value |
|-----------|-------|
| Language(s) | {languages} |
| Framework(s) | {frameworks} |
| Paradigm | {monolith / microservices / library / CLI tool} |
| Maturity | {greenfield / established / legacy} |
| Git history | {available (N commits) / unavailable} |
| Security-relevant commits | {count and brief description if any} |
| Test coverage indicators | {observed / not observed} |

## Attack Surface

### Trust Boundaries
{Where trust levels change, with file references}

### Entry Point Census
| Entry Point | Method | Auth Visible | Notes |
|-------------|--------|-------------|-------|
| {route/handler} | {HTTP method or protocol} | {Yes/No/Unverifiable} | {framework, handler file} |

### Entry Points
{External input sources, with file references}

### Privilege Operations
{Auth, access control, data access, with file references}

## Attack Hypotheses

### H1: {Title}
- **Target**: {What the attacker wants to reach/compromise}
- **Reasoning**: {Why this is a high-value target and how it might be reachable}
- **Falsification**: {What would disprove this hypothesis — makes the threat model testable}
- **Recommended Strategy**: {Flow Tracing / Variant Analysis / Algorithmic Reasoning / Adversarial Patterns}
- **Why this strategy**: {Specific reasoning for this recommendation}
- **Specialist Skill**: {sec-api | sec-authn | sec-authz | sec-crypto | sec-data | sec-infra | sec-injection | sec-logging | sec-supply-chain}

### H2: {Title}
...

## Stated-vs-Real Mismatches
| Claim Source | Stated Behavior | Observed Behavior | Tag |
|-------------|----------------|-------------------|-----|
| {file/doc} | {what was claimed} | {what was found} | Model-Reality Mismatch |

## Assumptions
ASSUMED: {claim} — {why not verified}
```

## Severity Definitions

Not directly applicable — this skill generates hypotheses, not findings. Downstream strategy skills assign severity to confirmed findings.

## Boundaries

- Do NOT produce final vulnerability findings — generate hypotheses for strategy skills to investigate
- Do NOT apply STRIDE, OWASP, or other framework taxonomies — reason adversarially
- Do NOT read more than ~10 files during characterization — save context budget for strategy skills. Note: targeted grep/search for route registration patterns does NOT count against this budget.
- Do NOT assume a vulnerability exists — hypothesize and recommend investigation
- Do NOT fill evidence gaps with guesses — label unknowns as ASSUMED
- Focus on identifying the MOST PRODUCTIVE investigation targets, not cataloguing every possible risk

---
user-invocable: false
name: priv-privacy-by-design
description: >
  Privacy architecture patterns, defaults, and DPIA indicator analysis.
  USE THIS SKILL for: privacy by design, privacy by default, DPIA, data protection impact assessment, privacy architecture, privacy patterns, privacy defaults, privacy engineering.
  PRINCIPLES: All (architectural assessment).
version: 1.1.0
license: MIT
---

# Privacy by Design Analysis

## Purpose

Analyze system architecture for privacy-by-design patterns and defaults, and flag DPIA indicators. This is an architectural-level assessment across all privacy principles.

## Scope

- Privacy-by-design and privacy-by-default patterns
- DPIA indicator identification
- Privacy architecture review (data minimization by design, purpose limitation by architecture)
- Default settings and their privacy implications
- Data protection by design patterns
- Privacy-enhancing technologies (PETs) adoption
- Privacy governance accountability (designated privacy owner, roles)
- Records of processing and documentation completeness

## Trigger Rules

This skill is NOT always-run. It triggers when:
- 2+ distinct privacy principles are implicated in the threat model, OR
- The target lacks explicit privacy architecture patterns (no privacy-context.md, no privacy-by-design signals), OR
- DPIA indicators are present in other skill findings, OR
- Governance gaps are detected (no designated privacy owner, no records of processing)

## Analysis Procedure

1. **Assess privacy defaults**: Check whether the system defaults to the most privacy-protective settings
2. **Review data minimization by design**: Is the architecture built to collect minimal data, or does minimization rely on policy alone?
3. **Check purpose separation**: Are data flows architecturally separated by purpose, or is all data co-mingled?
4. **Evaluate access controls**: Are access controls designed around data sensitivity and purpose?
5. **Identify DPIA indicators**: Flag factors that may warrant a DPIA:
   - Large-scale processing of personal data
   - Sensitive personal data processing
   - Automated decision-making with legal/significant effects
   - Systematic monitoring of public areas
   - Innovative use of new technologies
   - Cross-border data transfers at scale
6. **Review privacy patterns**: Check for adoption of privacy-enhancing technologies (data vaults, purpose-based access, consent-aware architectures)
7. **Assess audit capability**: Can the system demonstrate compliance through logs and records?
8. **Check governance accountability**: Is there a designated privacy owner or accountable role? Are privacy responsibilities documented?
9. **Verify records of processing**: Are records maintained documenting what personal data is collected, how it is used, shared, transferred, and retained?

## Controller/Processor Perspectives

- **Controller**: Must implement privacy by design across all processing; must conduct DPIA when indicators present; must maintain records demonstrating compliance
- **Processor**: Must implement technical measures as instructed by controller; must assist controller with DPIA; must support controller's audit requirements

## Evidence Categories

Evidence categories relevant to this skill: architecture review artifacts, default settings documentation/configuration, DPIA indicators checklist, privacy pattern implementation evidence, data protection by default configuration.

## Output Format

For each finding, provide:

```markdown
### {Title}
- **Principle**: {applicable principle(s)}
- **Summary**: {Brief description}
- **Details**: {Full explanation}
- **Severity**: {Critical | High | Medium | Low | Minimal}
- **Confidence**: {High | Medium | Low} ({justification})
- **Evidence**: {file path/line OR quote from design doc}
- **Control Status**: {Observed | Documented | Not Found}
- **Evidence Expected**: {applicable evidence category from this skill's Evidence Categories}
- **Tags**: {keywords, including "Commitment Misalignment", LINDDUN category, and/or named risk pattern (Control Theater, Silent Data Flow, Identifier Accumulation, Incomplete Assessment) where applicable}
- **Remediation**: {Actionable fix}
- **Proportionality** (High/Critical only): {Why this remediation is proportionate to the risk}
```

### DPIA Indicators Output

```markdown
## DPIA Indicators
- Indicators present: {list of applicable indicators}
- Indicators absent: {list or "none identified"}
- **Note**: This section flags indicators only. The agent does NOT determine whether a DPIA is legally required — that is a decision for the data protection officer or legal team.
```

## Severity Definitions

- **Critical**: System architecture fundamentally incompatible with privacy (e.g., no ability to delete data, all data co-mingled with no separation); multiple DPIA indicators with no assessment referenced
- **High**: Defaults expose maximum data; no purpose separation in data flows; privacy added as afterthought rather than by design; DPIA indicators present with no acknowledgment
- **Medium**: Partial privacy-by-design patterns; defaults not fully protective; some architectural gaps in data separation; DPIA indicators partially addressed
- **Low**: Minor default setting improvements; privacy patterns present but could be strengthened; documentation gaps in privacy architecture
- **Minimal**: Standard controls sufficient for this domain; theoretical risk only; no practical user impact

## Boundaries

- Do NOT provide legal advice on whether a DPIA is legally required
- Do NOT recommend specific privacy-enhancing technology products
- Do NOT modify system architecture
- Flag DPIA indicators only — leave the determination to the data protection officer or legal team
- Focus on identifying architectural privacy gaps, not implementing redesigns
- Tag findings with applicable named risk patterns (Control Theater, Silent Data Flow, Identifier Accumulation, Incomplete Assessment) when the finding matches a pattern

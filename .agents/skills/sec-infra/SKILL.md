---
user-invocable: false
name: sec-infra
description: >
  Infrastructure, cloud, and IaC security analysis.
  USE THIS SKILL for: network, cloud, IaC, container, infrastructure, Kubernetes, Terraform, Docker, firewall, VPC.
  STRIDE: Tampering, Denial of Service, Information Disclosure, Elevation of Privilege.
version: 2.0.0
license: MIT
---

# Infrastructure & Cloud Security Analysis

## Purpose

Analyze infrastructure, cloud, and IaC security concerns identified in the threat model. Focus on Tampering, Denial of Service, Information Disclosure, and Elevation of Privilege threats.

## Scope

- Network architecture and segmentation
- Cloud configuration (AWS, Azure, GCP)
- Infrastructure as Code (Terraform, CloudFormation, ARM)
- Container security (Docker, Kubernetes)
- Firewall rules and security groups
- Load balancing and availability
- IAM policies and service accounts

## Analysis Procedure

1. **Review Network Architecture**: Segmentation, trust boundaries, ingress/egress rules
2. **Check Cloud Configuration**: Security groups, public exposure, resource policies
3. **Evaluate IAM Policies**: Service accounts, managed identities, least-privilege enforcement — flag overly permissive policies (e.g., `*` actions, broad resource scopes)
4. **Analyze IaC Templates**: Hardcoded secrets, overly permissive policies, missing encryption settings
5. **Evaluate Container Security**: Base images (flag `latest` tag), privilege escalation (flag `privileged: true`), resource limits
6. **Assess Availability**: Single points of failure, DDoS protection, health checks

## Evidence Categories

Evidence categories relevant to this skill: network architecture diagrams, security group/firewall rules, IaC templates (Terraform/ARM/CloudFormation), Kubernetes manifests, IAM policy documents, container Dockerfiles.

## Output Format

For each finding, provide:

```markdown
### {Title}
- **Category**: {Tampering | Denial of Service | Information Disclosure | Elevation of Privilege}
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

- **Critical**: Public exposure of sensitive services, privileged container escape, wildcard IAM policies on sensitive resources
- **High**: Missing network segmentation, overly permissive security groups, overly broad IAM roles
- **Medium**: Suboptimal resource limits, incomplete logging, `latest` tag on container images
- **Low**: Minor configuration drift, documentation gaps
- **Minimal**: Standard controls sufficient; theoretical risk only; no practical impact

**Constraint**: Do NOT calculate CVSS vector strings. Use qualitative ratings only.

## Web Search Guidance

For cloud-provider-specific configurations, use web search to verify current security best practices. Cloud providers frequently update security features and deprecation timelines — cite the source when referencing provider-specific guidance.

## Boundaries

- Do NOT modify infrastructure or cloud resources
- Do NOT execute IaC templates
- Focus on identifying and documenting issues, not implementing fixes
- Tag findings with applicable named risk patterns (Control Theater, Incomplete Assessment) when the finding matches a pattern

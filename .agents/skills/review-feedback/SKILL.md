---
name: review-feedback
description: |
  Incorporate feedback from design review files into the original design document.
  Locates the design doc and its companion -REVIEW.md file, categorizes feedback by
  priority, updates the design, and deletes the review file when done.
  Triggers on: "review feedback", "incorporate review comments", "apply review suggestions",
  "process design review", "merge reviewer feedback".
---

# Review Feedback

Incorporate feedback from design review files into the original design document.

## Usage

```
/review-feedback [design-file-path]
```

If no path is provided, searches for the most recently modified design doc in `docs/design/` or `docs/refactor/` directories.

## Instructions

You are tasked with incorporating review feedback into a design document. Follow these steps:

### Step 1: Locate Files

1. If a design file path was provided as an argument, use that
2. Otherwise, search for design documents in `docs/design/` and `docs/refactor/` directories across the codebase
3. The review file is **always in the same directory** with a `-REVIEW.md` suffix (e.g., `feature.md` → `feature-REVIEW.md`)
4. If multiple design/review pairs exist, use the most recently modified pair

**Supported document types:**
- Design documents in `docs/design/` (feature designs, architecture plans)
- Refactor documents in `docs/refactor/` (architecture cleanup plans)

### Step 2: Analyze the Review

Read the review file carefully and categorize feedback:

| Priority | Description | Action Required |
|----------|-------------|-----------------|
| **BLOCKING** | Critical issues that must be addressed before implementation | Must resolve before proceeding |
| **HIGH** | Important concerns that significantly impact the design | Should address in this revision |
| **MEDIUM** | Suggestions that improve quality | Address if straightforward |
| **LOW** | Minor improvements or style suggestions | Optional, use judgment |

### Step 3: Create Incorporation Plan

For each piece of feedback:
1. Identify which section of the design it affects
2. Determine if it requires adding, modifying, or removing content
3. For BLOCKING issues, draft a specific resolution approach
4. Note any feedback that conflicts with other feedback

### Step 4: Update the Design Document

Make the following updates to the original design:

1. **Version History**: Add a new version entry noting "Incorporated review feedback"
2. **Address BLOCKING items first**: These are non-negotiable
3. **Incorporate HIGH priority items**: Update relevant sections
4. **Add MEDIUM/LOW items**: Where they improve clarity without over-complicating
5. **Open Questions**: Move unresolved items or trade-off decisions to Open Questions section
6. **Alternatives Considered**: Add any rejected approaches from feedback to this section

### Step 5: Delete the Review File

After successfully incorporating the feedback into the design document, **delete the `-REVIEW.md` file**. This signals that the review has been processed and prevents duplicate processing.

### Step 6: Document Changes

At the end of your response, provide a summary:

```markdown
## Review Incorporation Summary

### Blocking Issues Resolved
- [Issue]: [How it was resolved]

### Key Changes Made
- [Section]: [Change description]

### Feedback Deferred/Declined
- [Feedback]: [Reason for deferring or declining]

### Remaining Open Questions
- [Question that needs user input]

### Cleanup
- Deleted: [path/to/design-REVIEW.md]
```

## Important Guidelines

- **Never silently ignore BLOCKING feedback** - if you can't resolve it, explicitly call it out
- **Preserve the original design's intent** - feedback should refine, not completely rewrite
- **Be specific in resolutions** - don't just say "added error handling", show what was added
- **Ask for clarification** if blocking feedback is ambiguous or conflicts with requirements
- **Update implementation checklists** if new tasks are identified from feedback

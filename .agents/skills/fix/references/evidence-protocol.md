# Evidence Protocol — Fix RCA

All RCA Agent claims follow these rules. Internalize before investigating.

## Receipts

Every claim requires a receipt. No receipt after one self-challenge = claim excluded from output.

| Receipt Type | Format | Valid For |
|---|---|---|
| Code location | `file:line` of executable code (not comments/docs) | Code behavior claims |
| Commit | `commit:<sha>` from `git log` or `git show` output | Introduction/attribution claims |
| Issue / PR | `issue:<N>` or `pr:<N>` verified with `gh` | Historical pattern claims |
| Blame output | `git blame` line showing committer, date, sha | Line-level attribution |

## Evidence Hierarchy

Tool output > executable code paths > verifiable git artifacts.

Never cite as evidence:
- Comments or docstrings (they describe intent, not behavior)
- Documentation or README content
- Your own reasoning without a supporting tool call

## Confidence Gate

| Range | Meaning | Action |
|---|---|---|
| 80-100 | Multiple independent evidence paths converge | Assert root cause confidently |
| 60-79 | One clear evidence path, no contradictions | Assert root cause with caveat |
| 40-59 | Suggestive but incomplete | State hypothesis, list confirming evidence needed |
| 0-39 | Insufficient evidence | State "unconfirmed", list what's missing |

## Absence Rule

"No related issues found" is only valid if you list:
1. The exact search queries used
2. The result count for each query

Absence of evidence requires proof of the search.

## Chain-of-Verification

For each claim:
1. State what tool call verifies it
2. Run that tool call
3. Revise the claim if the result contradicts it

Re-reading your own reasoning is not verification. Tool output only.

# Reasoning Techniques for Skills & Agents

Academically-verified techniques that improve LLM reasoning. Use these patterns when writing skill/agent instructions.

---

## Key Findings

- **Chain-of-Thought variants** provide 15-70% improvements on reasoning benchmarks
- **Self-consistency** (majority voting) is simple but powerful (+17% GSM8K)
- **Verification techniques** like CoVe reduce hallucination by 23% F1
- **Critical limitation:** LLMs cannot self-correct reasoning without external feedback

---

## Top Techniques by Use Case

| Need | Technique | Integration |
|------|-----------|-------------|
| Step-by-step reasoning | Chain-of-Thought | "Break down into steps, show reasoning for each" |
| Higher confidence | Self-Consistency | "Generate 3 independent paths, answer in 2+ wins" |
| Reduce hallucination | Chain-of-Verification | "Draft → verify questions → answer independently → revise" |
| Complex problems | Tree of Thoughts | "Generate 2-3 candidates, evaluate, backtrack if stuck" |
| Abstract first | Step-Back Prompting | "What principle applies? Then solve specific case" |
| Build up solutions | Least-to-Most | "Solve simplest subproblem first, build up" |

---

## Chain-of-Thought Variants

### CoT for Reasoning Models (2024-2026 Update)

| Model Type | Recommendation |
|------------|----------------|
| Reasoning (o1, R1, DeepSeek-R1) | Omit "step by step" - only +2.9% benefit |
| Standard (Sonnet, GPT-4) | Keep explicit CoT prompts |

For reasoning-native models, explicit CoT shows diminishing returns. Remove "let's think step by step" prompts. **Claude Code uses standard models (Sonnet, Opus, Haiku) — keep explicit CoT prompts.**

### Basic CoT
**Evidence:** GSM8K state-of-the-art with 8 exemplars (Wei et al., 2022)

```
When solving complex problems:
1. Break down the problem into steps
2. Show your reasoning for each step
3. Verify each step before proceeding
```

### Zero-Shot CoT
**Evidence:** MultiArith 17.7% → 78.7% (Kojima et al., 2022)

```
Before answering, think through this step by step.
```

### Self-Consistency
**Evidence:** GSM8K +17.9%, SVAMP +11.0% (Wang et al., 2023)

```
Generate 3 independent reasoning paths.
Compare conclusions. Answer appearing in 2+ paths has higher confidence.
```

### Tree of Thoughts
**Evidence:** Game of 24: 4% (CoT) → 74% (ToT) (Yao et al., 2023)

```
Consider multiple approaches before committing:
1. Generate 2-3 candidate next steps
2. Evaluate which is most promising
3. If stuck, backtrack and try alternative
```

---

## Verification Techniques

### Chain-of-Verification (CoVe)
**Evidence:** +23% F1 on QA, reduces hallucination (Dhuliawala et al., 2024)

```
After generating a response:
1. Identify key factual claims
2. Generate verification questions for each
3. Answer verification questions using tool retrieval (Read, Grep, WebSearch), not by re-reading your draft
4. Revise response based on verification
```

### SelfCheck (Step-Level)
**Evidence:** Identifies errors in intermediate steps (Miao et al., 2023)

```
For each reasoning step, ask:
- Does this step logically follow?
- Are there any computational errors?
- Is this consistent with prior steps?
```

### CRITICAL: Self-Correction Limits
**Evidence:** Self-critique without external feedback often degrades performance (Huang et al., 2023; Stechly et al., 2024)

```
CAUTION: Do not assume self-review catches all errors.
When possible, use external verification (tools, retrieval).
Flag areas where external validation would help.
```

---

## Metacognitive Patterns

### Five-Stage Metacognition
**Evidence:** PaLM with MP approaches GPT-4 (Wang et al., 2024)

```
Follow this sequence:
1. UNDERSTAND: What is being asked?
2. INTERPRET: What is my initial answer?
3. EVALUATE: Is this interpretation accurate?
4. DECIDE: What is my final answer and why?
5. CALIBRATE: How confident am I?
```

### Reflexion (Learning from Errors)
**Evidence:** HumanEval 91% pass@1 vs GPT-4's 80% (Shinn et al., 2023)

```
After each attempt:
1. Analyze what went wrong
2. Generate verbal reflection on the error
3. Use reflection to guide next attempt
```

### Self-Refine
**Evidence:** ~20% average improvement across 7 tasks (Madaan et al., 2023)

```
After initial response:
1. Generate specific feedback on weaknesses
2. Refine based on feedback
3. Repeat until satisfied (max 3 iterations)
```

---

## Decomposition Patterns

### Step-Back Prompting
**Evidence:** +27% on reasoning tasks (Zheng et al., 2023)

```
Before solving:
1. What general principle applies here?
2. What are the first principles?
3. Now apply to the specific case.
```

### Least-to-Most
**Evidence:** SCAN 16% (CoT) → 99%+ (Zhou et al., 2023)

```
For complex problems:
1. Identify the simplest subproblem
2. Solve it
3. Use that solution for the next subproblem
4. Build up to full solution
```

---

## Confidence Calibration

**Evidence:** Verbalized confidence can be calibrated (Xiong et al., 2024)

```
Rate confidence in this answer:
- 0-24: Uncertain, state explicitly, identify verification needs
- 25-49: Possible, note uncertainty, proceed with caveats
- 50-74: Likely, verify key claims
- 75+: Verified, assert confidently

Only assert at 75+ confidence.
```

### Knowing When to Abstain

```
If confidence < 75% after reasoning:
- Do not assert as fact
- State uncertainty explicitly
- Identify what would resolve uncertainty
```

---

## Reasoning + Acting (ReAct)

**Evidence:** HotPotQA overcomes hallucination via API (Yao et al., 2022)

```
Alternate between:
- Thought: What do I need to figure out?
- Action: What external resource can help?
- Observation: What did I learn?
- Repeat until solved
```

---

## Anti-Patterns

| Pattern | Problem | Fix |
|---------|---------|-----|
| Underthinking | Switching approaches too quickly | Explore current path with 2+ variations first |
| Self-verification alone | Cannot reliably self-correct | Use external tools/retrieval |
| Inherited certainty | Accepting user claims as axioms | Treat as hypothesis, verify externally |
| Premature commitment | First approach without alternatives | Generate 2-3 candidates, then choose |

---

## Integration Template

For skill/agent instructions, combine patterns:

```
## Reasoning Protocol

1. **VALIDATE** (Step-Back)
   - What principle applies?
   - What assumptions am I making?

2. **DECOMPOSE** (Least-to-Most)
   - Break into simpler subproblems
   - Solve in sequence

3. **SOLVE** (CoT)
   - Show reasoning for each step
   - Flag uncertainties

4. **VERIFY** (CoVe)
   - Identify key claims
   - Generate verification questions
   - Answer using tool retrieval, not self-review
   - Revise if needed

5. **CALIBRATE**
   - Rate confidence (0-25, 50, 75+)
   - Only assert at 75+
```

---

**Deep dive:** `.claude/meta/reasoning-catalog.md` (verified techniques + evidence)

**Also see:** `.claude/meta/authoring-patterns.md` (practical patterns for skills/agents)

# Validate graders against human labels

`--grader-votes` on an `llm` grader shows when a judge was unsure. It shows
nothing about the case where a judge is unanimous and confidently wrong,
which only a human reviewer can catch. Label a sample of scored assertions
by hand, then check every grader against those labels with
`eval grader-agreement` before trusting it in a gate.

## Label an assertion

Every grader's result carries a stable assertion id, `<eval-case-id>:g<index>`
(the index of that grader in the eval case's `graders` array, zero-based).
Find it in `grading.json` or `report.json` under `dimensions.assertions`, then
record your verdict:

```shell
trg ai skills eval feedback label ./artifacts/my-skill/20260526T120000Z-abc \
  --run run-001 --assertion case-a:g0 --verdict fail \
  --rationale 'evidence quoted the wrong file'
```

`--verdict` is `pass` or `fail`, read as agreeing or disagreeing with what
the grader itself decided. `--rationale` is optional but worth recording: it
is the only context a later reader of a disagreement has for why a human
called it differently. Labelling the same assertion again replaces the
verdict rather than adding a second one, so relabelling after a rationale
gets corrected is safe.

## Check agreement

```shell
trg ai skills eval grader-agreement ./artifacts/my-skill/20260526T120000Z-abc
```

This joins every scored assertion across the bundle's runs against the
verdicts you have recorded, and reports agreement counts, an agreement rate,
and a Wilson 95% confidence interval, both overall and broken out by grader
kind (`mechanical`, `declarative`, `script`, `llm`). It also lists every
disagreement, with the grader's own evidence next to your rationale, and how
many scored assertions still have no human label at all.

The result is written to `grader-agreement.json` in the report directory
either way, so a CI run's artifact bundle carries the same evidence a human
reviewer would look at directly.

## Gate on it

```shell
trg ai skills eval grader-agreement ./artifacts/my-skill/20260526T120000Z-abc --min-agreement 0.9
```

`--min-agreement` fails the command when the `llm` bucket's confidence
interval's lower bound falls below the given proportion. It checks the `llm`
bucket specifically, not the overall rate: a mechanical or declarative
grader's verdict is already deterministic given its inputs, so there is
nothing about it a re-run could disagree with, and gating it against human
labels would only measure how correctly you wrote the check. An LLM judge is
the one grader kind whose confidence has to be earned run over run.

Gating on the interval's lower bound rather than the point estimate means a
handful of labels is not enough to pass: five agreements out of five is a
rate of 1.0, but the interval around five samples is wide, and the gate
reads that width rather than the number alone. Label enough assertions that
the interval narrows before you rely on the gate.

If no `llm`-graded assertion has a human label yet, the gate fails rather
than passing silently, so `--min-agreement` never reads as checked when
nothing was actually checked.

## Related docs

- [Write graders](./write-graders.md)
- [`eval feedback label` reference](../reference/ai-skills-eval.md#eval-feedback-label)
- [`eval grader-agreement` reference](../reference/ai-skills-eval.md#eval-grader-agreement)
- [Artifact: `grader-agreement.json`](../reference/ai-skills-eval.md#artifact-grader-agreementjson)

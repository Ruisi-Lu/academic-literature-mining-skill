# Academic value and screening policy

Read this file before changing screening thresholds or accepting papers manually.

## Hard exclusions

Reject a work when any of these conditions applies:

- authoritative metadata marks it retracted or withdrawn;
- it is paratext, an editorial, a peer-review record, a dataset-only record, news, slides, or a
  poster rather than an eligible scholarly work;
- title, identifiable authorship, publication year, or screening abstract is unavailable;
- the research plan excludes its document type or preprint status;
- no independently authorized open-access PDF is available;
- its Nemotron relevance score is below the research plan threshold.

Never override a retraction because citation counts or model relevance are high.

## Scored evidence

Calculate a 0–100 academic-value score from multiple evidence classes:

- scholarly work type and venue metadata;
- persistent identifiers such as DOI, arXiv ID, and OpenAlex ID;
- metadata completeness;
- age-normalized citations rather than raw lifetime citations;
- field-normalized citation percentile and FWCI when OpenAlex provides them;
- influential citations when Semantic Scholar enrichment is available;
- evidence-synthesis markers such as systematic review and meta-analysis;
- authorized full-text availability.

Assign tiers:

- A: 80–100
- B: 65–79.99
- C: 50–64.99
- D: below 50

Use the research plan's `min_quality_score` as a separate acceptance floor. Do not equate the score
with truth, methodological validity, journal impact factor, or peer-review status.

## Relevance and final priority

Rerank title, venue, type, year, and abstract against the research question and explicit inclusion
and exclusion criteria using:

`nvidia/llama-nemotron-rerank-vl-1b-v2`

Normalize the reranker logit with the sigmoid function. Calculate:

`priority = 0.55 × relevance + 0.45 × (quality / 100)`

Require both the quality and relevance thresholds. Select only the highest-priority
`target_papers`; retain rejected records and reasons for auditability.

## Bias controls

Do not use citation counts as the only value signal. Age-normalize citations so recent work can
compete. Include query variants, backward and forward citation searches, reviews, preprints when
allowed, non-English terms when relevant, and region-specific sources. Record source coverage and
failed shards.

For a publishable systematic review, treat this automated score as triage. Perform human
methodological appraisal with the field-appropriate instrument before drawing conclusions.

# Academic value and screening policy

Read this file before changing screening thresholds or accepting papers manually.

## Publication status and version control

Prefer a formally published, peer-reviewed journal article. In disciplines where
peer-reviewed conferences are archival publications, an official proceedings version is also a
formal version. Treat arXiv, bioRxiv, medRxiv, SSRN, repository manuscripts, and OpenReview
submissions as preprints or manuscripts unless an authoritative venue record proves otherwise.
An acceptance note, author claim, project page, or preprint `journal-ref` is a discovery lead, not
by itself proof of formal publication.

Before accepting or citing a preprint:

1. Search the exact title and authors for a journal version and, when relevant, an archival
   conference version.
2. Verify that the candidate is the same work by matching title, authors, and substantive content.
3. Require an authoritative publisher, journal, or proceedings record to mark it formally
   published, and verify the DOI when one is assigned. Use Crossref or OpenAlex for discovery and
   corroboration, not as a substitute for the venue record.
4. Record the publication status, authoritative evidence URLs, and date checked.
5. If a formal version exists, make it the canonical citation and retain the preprint only as
   alternate-version provenance or an authorized full-text source.

Exclude standalone preprints by default. Permit one only when the research plan explicitly allows
preprints, the work is indispensable to the research question, and no formal version was found
after the checks above. Label the citation and every evidence table entry as `preprint`, state that
peer review was not verified, and do not use it as the sole support for a key claim. Recheck its
status immediately before the final report or manuscript.

## Hard exclusions

Reject a work when any of these conditions applies:

- authoritative metadata marks it retracted or withdrawn;
- it is paratext, an editorial, a peer-review record, a dataset-only record, news, slides, or a
  poster rather than an eligible scholarly work;
- title, identifiable authorship, publication year, or screening abstract is unavailable;
- the research plan excludes its document type or preprint status;
- it is a preprint that is not explicitly allowed as an indispensable exception by the research
  plan;
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
compete. Include query variants, backward and forward citation searches, reviews, formally
published versions of discovered preprints, non-English terms when relevant, and region-specific
sources. Search preprints for discovery when useful, but do not cite them when a verified formal
version exists. Record source coverage and failed shards.

For a publishable systematic review, treat this automated score as triage. Perform human
methodological appraisal with the field-appropriate instrument before drawing conclusions.

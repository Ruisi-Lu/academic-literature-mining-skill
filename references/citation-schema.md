# Citation and provenance schema

Read this file when changing metadata mappings, Qdrant payloads, citation exports, or audits.

## Canonical work record

Preserve these fields without fabrication:

- `ids`: DOI, OpenAlex, arXiv, PubMed, Semantic Scholar, and other source identifiers;
- `title`;
- `authors`: given name, family name, literal name, ORCID, affiliations, and source IDs;
- `issued.date-parts` plus raw date when parsing is incomplete;
- `work_type`;
- `container_title`, publisher, volume, issue, page range, and article number;
- language, keywords, subjects, ISSN, and ISBN;
- canonical URL, authorized full-text candidates, and restricted publisher/DOI handoff URLs;
- citation metrics;
- retraction and paratext flags;
- quality signals, thresholds, rejection reasons, and screening timestamp;
- every metadata source and retrieval timestamp.

Store the exact raw response from each scholarly source in SQLite `source_records`. Merge fields
by filling gaps or preferring the more complete value, except when authoritative metadata
promotes a preprint to a verified journal article or archival conference paper. In that case,
prefer the formal title, type, venue, publisher, publication date, volume, issue, pages, article
number, and DOI landing URL; retain the richer abstract plus every arXiv identifier, authorized
full-text candidate, and provenance source. Keep retraction and withdrawal flags monotonic. Never
erase provenance.

When no public abstract is available, preserve `abstract_text` as empty and record
`screening-abstract-unavailable:+0`. Never fabricate the abstract or use its absence alone as a
rejection reason; the work remains incomplete until authorized full text is obtained and checked.
If ScienceDirect abstract enrichment is explicitly enabled, store the exact `META_ABS` response
as a `sciencedirect` source record and merge `dc:description` only after an exact normalized DOI
match. The source is abstract provenance, not a full-text license or permission assertion.

Preserve the normalized active research plan at `metadata/research-plan.json` and an immutable,
content-addressed copy under `metadata/plans/` whenever discovery or screening runs.

## Qdrant page payload

Attach these values to every multimodal page point:

- persistent workspace `corpus_id`, local `page_id`, and a deterministic Qdrant point UUID derived
  from both;
- canonical work ID and one-based page number;
- rendered image path and SHA-256;
- embedded native page text and the actual embedding modality (`text_image` or `image`);
- preserved PDF path, source URL, SHA-256, and license assertion;
- embedding model ID and schema version;
- complete CSL-JSON citation;
- complete canonical work record and quality assessment;
- publication year and authorized PDF URL.

This duplication intentionally keeps every retrieved page independently citable.
Every query must load `corpus_id` from the selected live SQLite workspace and apply an exact
Qdrant payload filter before candidates are reranked.

For a user-supplied paywalled PDF, add a `manual-pdf` source record with its source URL, exact local
path, checksum, acquisition type, and `reuse_license_asserted: false`. Store the PDF license value
as `user-supplied; reuse rights not established`; never convert lawful access into an open-license
claim.

## Local exports

Generate all of these files:

- `exports/library.csl.json` for CSL processors and reference managers;
- `exports/library.bib` for LaTeX and BibLaTeX workflows;
- `exports/library.ris` for common reference managers;
- `exports/records.jsonl` for lossless canonical records;
- `exports/citation-audit.json` for missing-field review.
- `exports/corpus-audit.json` for provenance, original-PDF checksum, page-count, and indexing-state
  review.

Use a stable, collision-resolved citation key. Do not infer missing authors, dates, DOI values,
venues, volume, issue, or page numbers.

## Page-level citations

When a retrieved claim comes from a multimodal page point, cite the canonical work and include the
one-based PDF page number from the payload. Treat both extracted text and the rendered image only
as retrieval evidence; the preserved PDF and its SHA-256 remain the source artifact.

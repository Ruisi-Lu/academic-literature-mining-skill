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
- canonical URL and authorized full-text candidates;
- citation metrics;
- retraction and paratext flags;
- quality signals, thresholds, rejection reasons, and screening timestamp;
- every metadata source and retrieval timestamp.

Store the exact raw response from each scholarly source in SQLite `source_records`. Merge fields
only by filling gaps or preferring the more complete value. Never erase provenance.

Preserve the normalized active research plan at `metadata/research-plan.json` and an immutable,
content-addressed copy under `metadata/plans/` whenever discovery or screening runs.

## Qdrant page payload

Attach these values to every visual page point:

- deterministic page UUID;
- canonical work ID and one-based page number;
- rendered image path and SHA-256;
- preserved PDF path, source URL, SHA-256, and license assertion;
- embedding model ID and schema version;
- complete CSL-JSON citation;
- complete canonical work record and quality assessment;
- publication year and authorized PDF URL.

This duplication intentionally keeps every retrieved page independently citable.

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

When a retrieved claim comes from a visual page point, cite the canonical work and include the
one-based PDF page number from the payload. Treat the rendered image only as retrieval evidence;
the preserved PDF and its SHA-256 remain the source artifact.

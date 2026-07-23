# Live corpus query routing

Read this file before querying an existing corpus or using corpus data in a
manuscript. Treat the routing rules as mandatory.

Select exactly one per-paper workspace before any lookup. Never query a global or
implicit Qdrant collection. `litmine query --workspace <workspace>` reads that
workspace's persistent `corpus_id`; the runtime includes it in point identities
and applies it as a mandatory Qdrant filter.

## Contents

- Select the authoritative query surface
- Prohibit archive-first retrieval
- Form semantic evidence queries
- Query relational state
- Preserve MCP tool semantics
- Join and verify evidence

## Select the authoritative query surface

| Need | Required live source | Command or tool |
| --- | --- | --- |
| Find passages, claims, methods, results, tables, figures, or counterevidence by meaning | Qdrant `content` vector followed by Nemotron VL reranking | `litmine query` / `search_evidence` |
| Filter or list works by status, year, quality, or title | SQLite `works` and `pages` tables | `litmine catalog` / `catalog_works` |
| Resolve one retrieved `work_id`, its canonical metadata, provenance, PDF, and page locators | SQLite `works`, `source_records`, and `pages` tables | `litmine inspect-work` / `inspect_work` |
| Count pipeline states | SQLite | `litmine status` / `corpus_status` |
| Check citation, artifact, checksum, and indexing integrity | SQLite plus preserved source artifacts | `litmine audit` / `audit_corpus` |
| Hand citations to a citation processor or another application | Generated export | `litmine export`; then use only the requested export format |

For a mixed research request, query Qdrant first, take the returned `work_id`
values into `inspect-work`, and then verify the exact PDF pages. Do not reverse
this order by scanning the relational catalog for words that might answer the
research question.

## Prohibit archive-first retrieval

Do not use filesystem search or file-reading tools to discover research evidence
inside any of these paths:

- `projects/<slug>/exports/*.json`, `projects/<slug>/exports/*.jsonl`, or `records.jsonl`;
- `projects/<slug>/metadata/**/*.json`, including preserved research plans;
- `projects/<slug>/state.sqlite3`, its WAL/SHM files, or copied database files;
- raw provider JSON in `source_records`;
- rendered-page directories or extracted page text before vector retrieval.

Do not run `rg`, `grep`, `find`, `cat`, `jq`, ad hoc scripts, or arbitrary SQL as
a substitute for the live commands above. Exported JSON/JSONL is a point-in-time
exchange artifact and may be stale. A research plan describes intended scope; it
is not evidence. Raw provider JSON is provenance, not a manuscript source.

Open a preserved PDF or rendered page only after `query` returns its exact
`pdf_path`, `image_path`, and one-based `page_number`, or after `inspect-work`
resolves a known `work_id`. If a required live command is unavailable or fails,
report that retrieval is unavailable. Never silently fall back to archive files.

## Form semantic evidence queries

Use one natural-language evidence question per call. State the entities or
population, relationship or intervention, outcome, and relevant context. Prefer:

```text
What controlled evidence shows whether X changes Y under Z, and what limitations
affect that conclusion?
```

Avoid title fragments, DOI values, Boolean keyword dumps, and vague requests such
as `papers about X`. Use `catalog` or `inspect-work` for known identifiers and
metadata.

Start a manuscript evidence pass with:

```bash
target/release/litmine --env-file "projects/$PROJECT_SLUG/.env" query \
  "state one atomic evidence question" \
  --workspace "projects/$PROJECT_SLUG" \
  --top-k 20 --candidate-limit 80
```

For each important claim, make separate calls for:

1. direct supporting evidence;
2. contradictory evidence, failure conditions, and limitations;
3. measurement details or quantitative results when the claim depends on them.

Treat `vector_score` and `rerank_score` only as ranking signals. Do not cite a
score or infer truth, certainty, effect size, or study quality from it. Deduplicate
results by `work_id`, but retain multiple pages when they support different parts
of the claim.

## Query relational state

Use `catalog` for structured selection:

```bash
target/release/litmine catalog \
  --workspace "projects/$PROJECT_SLUG" \
  --status indexed \
  --year-from 2020 \
  --min-quality-score 65 \
  --limit 50
```

Use `inspect-work` with the exact identifier returned by `query` or `catalog`:

```bash
target/release/litmine inspect-work \
  'doi:10.1234/example' \
  --workspace "projects/$PROJECT_SLUG"
```

`inspect-work` intentionally returns page locators without `page_text` and source
provenance without raw provider JSON. Use Qdrant for semantic page discovery and
the original PDF for evidence verification.

## Preserve MCP tool semantics

When an MCP adapter exposes this corpus, use these tool definitions:

- `search_evidence(workspace, query, top_k=20, candidate_limit=80)`: Open exactly
  one workspace, read its `corpus_id`, and perform the complete
  Nemotron query embedding, Qdrant `content` vector search, and Nemotron VL page
  rerank with the mandatory corpus filter. Make this the only tool described as
  capable of finding passage-level research evidence.
- `catalog_works(workspace, status?, year_from?, year_to?, min_quality_score?,
  title_contains?, limit=50)`: Query live SQLite metadata. State explicitly that
  it cannot answer passage or manuscript-content questions.
- `inspect_work(workspace, work_id)`: Query live SQLite for the canonical record, compact
  provenance locators, PDF artifact, and page locators. Exclude page text and raw
  source responses.
- `corpus_status(workspace)`: Return live SQLite pipeline counts.
- `audit_corpus(workspace)`: Run the live citation/artifact audit before manuscript work.

Keep evidence, vectors, and relational metadata immutable during manuscript
retrieval; an audit may refresh only its generated audit report. Do not expose
the generic `qdrant-store` tool to a writing agent. Do not describe a generic
`qdrant-find` tool as compatible with this corpus: it does not implement the
required multimodal embedding, named-vector, payload, and reranking contract.

## Join and verify evidence

For every manuscript claim:

1. Record the exact semantic query.
2. Retain the returned `work_id`, `page_id`, and one-based `page_number`.
3. Run `inspect-work` for canonical citation, provenance, and artifact identity.
4. Open the preserved PDF at the returned page and verify wording, table or figure
   labels, population, method, result, and limitations in context.
5. Record whether the page supports, contradicts, or only contextualizes the
   claim.
6. Cite the canonical work with a pinpoint page only after verification.

Do not let a Qdrant payload, extracted text, archive export, abstract, or AI
summary replace verification against the preserved PDF.

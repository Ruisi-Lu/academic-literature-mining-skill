---
name: academic-literature-mining
description: Mine, screen, download, preserve, multimodally index, and retrieve academically valuable scholarly literature with a Rust CLI, budget-model search subagents, NVIDIA Build Nemotron Embed/Rerank VL models, and Qdrant. Use when Codex must build or update a large citation-complete research corpus, conduct agent-assisted literature discovery, preserve authorized PDFs and authoritative citation metadata, import subagent search results, retrieve evidence from native-text-plus-image PDF pages without OCR, export CSL JSON/BibTeX/RIS, or audit sources before writing a paper.
---

# Academic Literature Mining

Build a reproducible scholarly corpus from persistent identifiers and authoritative
metadata. Preserve the original PDF, complete citation record, provenance, quality
decision, and page-level multimodal vectors so later writing can cite the source
safely.

## Enforce the invariants

- Use the Rust `litmine` CLI for the complete runtime. Keep PDF preparation in
  Rust and PDFium; do not add OCR or an external PDF-to-text service.
- Enforce a strict read-only boundary for
  `~/Project/visual-encoding-vs-raw-iot-reasoning`: never create, edit, delete,
  rename, or move any file there. Read only
  `research/academic-literature-mining-skill-issues.md` and the exact files that
  report explicitly names; do not list, search, or inspect any other path in that
  project. Apply fixes only in the repository containing this `SKILL.md`.
- Keep discovery workers untrusted and cheap. Let them search only, and require the
  coordinator to resolve identifiers, verify metadata, score quality, authorize
  downloads, and write to Qdrant.
- Accept a candidate only when it has a DOI, arXiv ID, or OpenAlex ID that can be
  resolved through a scholarly metadata source.
- Prefer the formally published, peer-reviewed version over a preprint. Before
  accepting or citing any preprint, search by exact title and authors for a journal
  article and, in fields with archival conferences, a proceedings version; verify
  the match through an authoritative publisher, journal, or proceedings record and
  verify its DOI when one is assigned.
- When a formal version exists, cite and canonicalize that version. Retain the
  preprint only as alternate-identifier, full-text, and version provenance. Use a
  standalone preprint only when it is indispensable and no formal version can be
  verified; label it explicitly, record the sources and date checked, and never use
  it as the sole support for a key conclusion.
- Download only an openly licensed or otherwise authorized full-text URL. Never
  infer download permission from the ability to access a URL.
- Preserve the original PDF. For each page, extract only its embedded native text
  layer with PDFium and render the complete page to an image. Embed and rerank both
  as one `text_image` page; when no usable native text exists, fall back to the
  complete page image. Do not OCR or split a page into semantic text chunks.
- Do not send an `application/pdf` payload to the NVIDIA Embed or Rerank endpoint;
  those model APIs accept text and image data URLs, not raw PDF files.
- Use exactly:
  `nvidia/llama-nemotron-embed-vl-1b-v2` for embeddings and
  `nvidia/llama-nemotron-rerank-vl-1b-v2` for reranking.
- Store the canonical citation object, source records, identifiers, license,
  provenance, quality signals, PDF checksum, and page coordinates alongside every
  indexed point.
- Treat retrieval output as evidence candidates. Re-open the original PDF before
  quoting or making a claim in a manuscript.

## Install the skill

Read [INSTALL.MD](INSTALL.MD) before running the workflow. Follow its portable
subagent-manifest procedure instead of assuming a particular agent framework.

Copy `.env.example` to `.env`, then set `NVIDIA_API_KEY`, `QDRANT_API_KEY`,
`OPENALEX_API_KEY`, and `CONTACT_EMAIL` as applicable. Do not place secrets in a
subagent manifest or inherit the coordinator environment into search workers.

Initialize the runtime:

```bash
cp .env.example .env
# Edit .env before continuing.
rustup update stable
cargo build --release --locked
docker compose up -d
cargo run --release --locked -- doctor
```

## Define the research run

Copy `assets/research-plan.example.json` and edit it for the research question.
Specify:

- the exact question and search concepts;
- inclusion and exclusion criteria;
- publication window and accepted work types;
- publication-status policy, with preprints excluded by default unless the plan
  explicitly permits indispensable, clearly labeled exceptions;
- minimum quality score and corpus size;
- a `min_relevance_score` of `0.0` for rank-only triage unless a nonzero cutoff
  has been calibrated on labeled examples for this exact screening query;
- source-specific queries rather than one broad query;
- stopping rules so repeated runs are bounded and reproducible.

Read [references/quality-policy.md](references/quality-policy.md) before changing
thresholds. A citation count alone is never sufficient evidence of academic value.

## Delegate discovery to budget search workers

Read [references/subagent-contract.md](references/subagent-contract.md). Give the
installing agent `assets/subagent-manifest.example.json`,
`assets/subagent-task.schema.json`, `assets/subagent-result.schema.json`, and
`assets/search-subagent-prompt.md`.

Translate the portable manifest into the host agent runtime's native manifest.
Do not invent a native filename or schema. Keep the following boundary:

1. The coordinator shards source/query/page tasks.
2. Budget workers search scholarly systems and return strict NDJSON candidates.
3. Workers never download PDFs, call NVIDIA, access Qdrant, run a shell, or receive
   secrets.
4. The coordinator imports results and independently resolves every persistent ID.

Import worker output:

```bash
cargo run --release --locked -- \
  import-agent-results corpus/inbox/candidates.ndjson
```

Reject malformed output rather than repairing unsupported claims manually.

## Run the mining workflow

Run all built-in discovery and corpus stages:

```bash
cargo run --release --locked -- \
  mine --plan assets/research-plan.example.json
```

For controlled runs, execute the stages independently:

```bash
cargo run --release --locked -- discover --plan research-plan.json
cargo run --release --locked -- refresh-metadata
cargo run --release --locked -- screen --plan research-plan.json
cargo run --release --locked -- download
cargo run --release --locked -- render
cargo run --release --locked -- ingest
cargo run --release --locked -- audit
cargo run --release --locked -- export
```

After upgrading an existing workspace that may contain DOI-backed arXiv
preprints, run `refresh-metadata` and then rerun `screen` with the preserved
research plan. Let `refresh-metadata` re-resolve affected DOI records through
Crossref, promote verified journal or archival-conference citation fields, and
queue promoted records for screening. `screen` performs this refresh
automatically as well. Keep the arXiv identifier and authorized PDF as
alternate-version provenance.

After upgrading an existing image-only corpus, rebuild its stored pages once and
re-ingest them:

```bash
cargo run --release --locked -- render --refresh-existing
cargo run --release --locked -- ingest
```

Inspect `status` and the audit output after every large run. Do not treat a network
retry, a missing abstract, or an inaccessible PDF as a successful source.

## Retrieve literature

Search the multimodal page corpus:

```bash
cargo run --release --locked -- \
  query "state the evidence question precisely" --top-k 20
```

The CLI embeds the text query, retrieves candidate PDF pages from Qdrant, then
reranks each page using the same native-text-plus-image representation used for
indexing. Image-only pages remain supported. Use the returned work ID, page
number, DOI, and canonical citation to inspect and cite the original source.

## Export citations and audit provenance

Read [references/citation-schema.md](references/citation-schema.md) before
integrating exports into a writing system.

Export:

- CSL JSON for citation managers and structured writing tools;
- BibTeX for LaTeX workflows;
- RIS for broad reference-manager interoperability;
- canonical JSONL for lossless corpus exchange;
- an audit report covering metadata, provenance, PDF checksums, and indexing state.

Run `audit` immediately before manuscript work. Never create a citation from an
embedded page alone when the canonical record is incomplete.

## Consult API contracts

Read [references/api-contracts.md](references/api-contracts.md) before changing
NVIDIA, Qdrant, Crossref, OpenAlex, arXiv, or Semantic Scholar integrations. Keep
raw provider records in SQLite so later metadata corrections remain traceable.

---
name: academic-literature-mining
description: Mine, screen, acquire, preserve, multimodally index, and query academically valuable scholarly literature with a Rust CLI, budget-model search subagents, NVIDIA Build Nemotron Embed/Rerank VL models, Qdrant vector retrieval, SQLite relational state, and isolated per-paper Compose projects. Includes explicit missing-token onboarding, opt-in ScienceDirect abstract enrichment, opt-in paywalled-journal search with a resumable user-download handoff, and optional DOI-by-DOI Google Scholar library access through an authorized Chrome DevTools MCP session. Use when Codex must build or update one or more citation-complete research corpora without cross-paper data leakage, conduct agent-assisted literature discovery, guide scholarly API or browser setup, preserve authorized or explicitly user-supplied PDFs and authoritative citation metadata, import subagent search results, retrieve or inspect evidence without scanning archive JSON, export CSL JSON/BibTeX/RIS, or audit sources before writing a paper.
---

# Academic Literature Mining

Build a reproducible scholarly corpus from persistent identifiers and authoritative
metadata. Preserve the original PDF, complete citation record, provenance, quality
decision, and page-level multimodal vectors so later writing can cite the source
safely.

## Enforce the invariants

- Use the Rust `litmine` CLI for the complete runtime. Keep PDF preparation in
  Rust and PDFium; the CLI may be a release-pinned prebuilt image or a native
  build. Do not add OCR or an external PDF-to-text service.
- Keep an installed skill as a complete, non-shallow Git checkout of the canonical
  repository. Never replace it with a source archive. Pair a checkout with only
  the exact published container release declared by its `Cargo.toml`; never infer
  compatibility from `latest`.
- Treat every unrelated paper as a separate corpus boundary. Follow
  [references/project-isolation.md](references/project-isolation.md): use a new
  `projects/<slug>/` workspace and Compose project with its own Qdrant volume.
  Never reuse or copy another paper's workspace. Require `query` to open the
  intended workspace so its persisted `corpus_id` scopes every vector operation.
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
- Automatically download only an openly licensed or otherwise authorized
  full-text URL. Never infer download permission from the ability to access a
  URL. Accept a paywalled PDF only through the explicit user-authorized workflow
  after opt-in; never handle authentication secrets, scrape, or bypass access
  controls for the user, and never label that PDF openly licensed.
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
- Route every live corpus lookup through
  [references/query-routing.md](references/query-routing.md). Use Qdrant plus
  Nemotron reranking for semantic evidence and SQLite-backed CLI commands for
  metadata, provenance, state, and audits. Never search exported or preserved JSON
  files as a substitute.
- Treat retrieval output as evidence candidates. Re-open the original PDF before
  quoting or making a claim in a manuscript.

## Install the skill

Read [references/user-interaction.md](references/user-interaction.md) before any
credential check, optional-feature decision, service startup, PDFium download,
or manual/browser PDF request. This is mandatory: when a parameter is missing, name the
official acquisition location, exact `.env` field, setup steps, and verification
command. Never merely report a missing token and never ask the user to paste a
secret into chat.

Read [INSTALL.MD](INSTALL.MD) before running the workflow. Follow its portable
subagent-manifest procedure instead of assuming a particular agent framework.
Read [references/project-isolation.md](references/project-isolation.md) before
creating or starting any corpus.

Ask whether to use the prebuilt Docker image or a native Rust build. When Docker
Compose is available, recommend the image to avoid a host Rust compile and PDFium
download. Read package version `X.Y.Z` from the checked-out `Cargo.toml`, verify
that the complete clone contains tag `vX.Y.Z` and that the corresponding GitHub
Release is published, set `LITMINE_VERSION=X.Y.Z`, and pull only
`ghcr.io/ruisi-lu/academic-literature-mining-skill:X.Y.Z`. Verify both
`litmine --version` and the image's `org.opencontainers.image.version` label. If
any check fails, explain it and use the native build; never fall back to an
unverified or `latest` image. If Docker itself is absent, provide the official
installation path and obtain authorization before changing the host.

Before copying or editing configuration, check only whether required values are
present; do not print their values. Ask the user to choose or confirm one unique
paper slug, create a new `projects/<slug>/`, and copy `.env.example` to that
directory as `.env`. Set `LITMINE_PROJECT` to the same slug, then guide the user
through stage-required values. Ask whether optional Semantic Scholar enrichment
is wanted before requesting its key. Do not place secrets in a subagent manifest
or inherit the coordinator environment into search workers.

Initialize the prebuilt runtime:

```bash
PROJECT_SLUG=paper-a
mkdir -p "projects/$PROJECT_SLUG"
cp .env.example "projects/$PROJECT_SLUG/.env"
cp assets/research-plan.example.json "projects/$PROJECT_SLUG/research-plan.json"
# Edit only this project's .env and plan before continuing.
docker compose --env-file "projects/$PROJECT_SLUG/.env" pull litmine
docker compose --env-file "projects/$PROJECT_SLUG/.env" up -d qdrant
docker compose --env-file "projects/$PROJECT_SLUG/.env" run --rm litmine \
  doctor --check-qdrant
docker compose --env-file "projects/$PROJECT_SLUG/.env" run --rm litmine \
  init --workspace /workspace
```

Replace `paper-a` with the confirmed slug and refuse to overwrite an existing
project. SQLite remains at `projects/<slug>/state.sqlite3`; it is embedded state,
not a separate Compose service. Container-mode Qdrant stays only on that
project's private network.

For native mode, follow [INSTALL.MD](INSTALL.MD), build with locked Cargo
dependencies, ask before downloading PDFium, and run the same Rust CLI directly.

## Define the research run

Edit `projects/<slug>/research-plan.json` for the research question.
Before editing any switch, ask the user whether to enable it. Treat silence as
disabled; do not infer consent from an existing key. At minimum ask about
`include_preprints`, `include_paywalled`,
`use_google_scholar_library_access`, `use_sciencedirect_abstracts`, and optional Semantic Scholar
enrichment.
For `include_paywalled`, explain the lawful-access and external
NVIDIA-processing conditions in
[references/user-interaction.md](references/user-interaction.md) and require an
explicit answer. Offer Google Scholar library access only after paywalled access
is enabled; it remains off unless the user separately authorizes both browser
attachment and any missing MCP installation.

Specify:

- the exact question and search concepts;
- inclusion and exclusion criteria;
- publication window and accepted work types;
- publication-status policy, with preprints excluded by default unless the plan
  explicitly permits indispensable, clearly labeled exceptions;
- an `include_paywalled` value of `false` by default; set it to `true` only after
  the user opts in to publisher/subscription searches and the manual PDF handoff;
- a `use_google_scholar_library_access` value of `false` by default; enable it
  only for an existing user-configured library affiliation and after reading
  [references/chrome-library-access.md](references/chrome-library-access.md);
- a `use_sciencedirect_abstracts` value of `false` by default; enable it only after the user asks
  to use Elsevier's Article Retrieval API and has placed `ELSEVIER_API_KEY` in this paper's `.env`;
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
   Pass the user-approved `include_paywalled` value on every worker task.
3. Workers never download PDFs, call NVIDIA, access Qdrant, run a shell, or receive
   secrets.
4. The coordinator imports results and independently resolves every persistent ID.

Import worker output:

```bash
cargo run --release --locked -- \
  --env-file "projects/$PROJECT_SLUG/.env" \
  import-agent-results "projects/$PROJECT_SLUG/inbox/candidates.ndjson" \
  --workspace "projects/$PROJECT_SLUG"
```

Reject malformed output rather than repairing unsupported claims manually.

## Run the mining workflow

For open-access-only runs, run all built-in discovery and corpus stages:

```bash
cargo run --release --locked -- \
  --env-file "projects/$PROJECT_SLUG/.env" \
  mine --plan "projects/$PROJECT_SLUG/research-plan.json" \
  --workspace "projects/$PROJECT_SLUG"
```

In prebuilt-image mode, invoke the same subcommands through
`docker compose --env-file projects/<slug>/.env run --rm litmine`, use
`/workspace` for the corpus and `/workspace/research-plan.json` for its plan. Only
that paper's ignored project directory is bind-mounted; keep the Git checkout
separate and updateable. Do not run a host `cargo build` or request
`--prepare-pdfium` in image mode.

If `include_paywalled` is enabled, prefer controlled stages so the workflow can
pause cleanly for the user. The `mine` command is still resumable and reports the
same manual handoffs, but it cannot complete pending works until the user supplies
their files.

For controlled runs, execute the stages independently:

```bash
cargo run --release --locked -- --env-file "projects/$PROJECT_SLUG/.env" discover --plan "projects/$PROJECT_SLUG/research-plan.json" --workspace "projects/$PROJECT_SLUG"
cargo run --release --locked -- --env-file "projects/$PROJECT_SLUG/.env" refresh-metadata --workspace "projects/$PROJECT_SLUG"
cargo run --release --locked -- --env-file "projects/$PROJECT_SLUG/.env" screen --plan "projects/$PROJECT_SLUG/research-plan.json" --workspace "projects/$PROJECT_SLUG"
cargo run --release --locked -- --env-file "projects/$PROJECT_SLUG/.env" download --workspace "projects/$PROJECT_SLUG"
cargo run --release --locked -- --env-file "projects/$PROJECT_SLUG/.env" render --workspace "projects/$PROJECT_SLUG"
cargo run --release --locked -- --env-file "projects/$PROJECT_SLUG/.env" ingest --workspace "projects/$PROJECT_SLUG"
cargo run --release --locked -- --env-file "projects/$PROJECT_SLUG/.env" audit --workspace "projects/$PROJECT_SLUG"
cargo run --release --locked -- --env-file "projects/$PROJECT_SLUG/.env" export --workspace "projects/$PROJECT_SLUG"
```

Do not hard-reject a verified scholarly record only because Crossref, OpenAlex, or another public
metadata source omits its abstract. Preserve the DOI and canonical metadata, record
`screening-abstract-unavailable:+0`, and let Nemotron perform conservative bibliographic triage
from the title, year, type, and venue. Treat that triage as low-confidence: obtain and verify the
authorized PDF before using the work as evidence. When `include_paywalled` is enabled, continue to
the normal manual or authorized Scholar handoff.

If the user explicitly enables `use_sciencedirect_abstracts`, obtain `ELSEVIER_API_KEY` from
<https://dev.elsevier.com/>, save it only as `ELSEVIER_API_KEY=<key>` in
`projects/<slug>/.env`, and rerun `screen`. The coordinator calls the official Article Retrieval
API with `view=META_ABS` only for likely ScienceDirect records whose abstract is empty. Accept an
abstract only when the returned DOI normalizes to the requested DOI. Do not scrape the
ScienceDirect page, request full text through this option, infer access rights, or expose the key.
An unavailable or unauthorized article-level response falls back to bibliographic triage; a
missing or rejected key requires the user to correct the project `.env` or disable the switch.

After upgrading a workspace whose candidates were rejected only for missing abstracts, do not
repeat discovery. Confirm the preserved plan still reflects the user's paywalled-access choice,
ask separately whether to enable ScienceDirect enrichment, rerun `screen` (which reconsiders
stored `rejected` records), then run `download` and present every manual handoff.

## Hand off paywalled PDFs to the user

Never download restricted content. After `download`, inspect its
`manual_downloads` array. For every item, present the title and DOI, turn every
`download_urls` entry into a clickable link, show the exact absolute
`destination`, and explain why restricted access is required.

If the plan also enables `use_google_scholar_library_access` and a request has a
`google_scholar_query_url`, read
[references/chrome-library-access.md](references/chrome-library-access.md) and
try that bounded DOI-by-DOI browser handoff before asking the user to move the
file. Use only an existing `chrome-devtools` MCP, or explain the official setup
and obtain explicit authorization before installing it at user scope. Keep all
Node/npm/MCP configuration outside this repository. Never bulk-query Scholar,
inspect unrelated tabs, extract session secrets, handle login/2FA/CAPTCHA, or
bypass a paywall. Fall back to the manual steps above whenever browser automation
cannot proceed safely.

Otherwise, tell the user to use their own lawful publisher or institutional
access, save the correct PDF exactly at `destination`, and confirm when ready.
Then pause; do not request credentials, cookies, tokens, or the PDF in chat.

After the browser has saved the file successfully, or after the user confirms a
manual placement, rerun:

```bash
cargo run --release --locked -- --env-file "projects/$PROJECT_SLUG/.env" download --workspace "projects/$PROJECT_SLUG"
cargo run --release --locked -- --env-file "projects/$PROJECT_SLUG/.env" render --workspace "projects/$PROJECT_SLUG"
cargo run --release --locked -- --env-file "projects/$PROJECT_SLUG/.env" ingest --workspace "projects/$PROJECT_SLUG"
cargo run --release --locked -- --env-file "projects/$PROJECT_SLUG/.env" audit --workspace "projects/$PROJECT_SLUG"
cargo run --release --locked -- --env-file "projects/$PROJECT_SLUG/.env" export --workspace "projects/$PROJECT_SLUG"
```

`download` validates and hashes the supplied regular PDF, records manual
provenance, and changes the work from `awaiting-manual-download` to `downloaded`.
Verify its title, authors, and DOI against `inspect-work` and the original PDF
before continuing. Report remaining handoffs and failures. Record its license as
`user-supplied; reuse rights not established`; lawful access is not permission to
redistribute. Follow the complete pause/resume contract in
[references/user-interaction.md](references/user-interaction.md).

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

After upgrading a pre-isolation workspace, follow the migration procedure in
[references/project-isolation.md](references/project-isolation.md). Run `init`
once to assign its persistent `corpus_id`; this queues legacy indexed pages for
the new `academic_literature_v2` collection. Re-ingest before querying.

Inspect `status` and the audit output after every large run. A missing public abstract is incomplete
metadata, not a rejection reason and not evidence that screening is complete. Do not treat a
network retry or inaccessible PDF as a successful source.

## Query the live corpus

Read [references/query-routing.md](references/query-routing.md) before querying a
corpus or drafting from it. Classify the lookup before selecting a tool:

- use `query` for semantic passage, claim, method, result, table, figure,
  limitation, or counterevidence retrieval;
- use `catalog` for structured SQLite filters and corpus listings;
- use `inspect-work` for one known `work_id`, canonical metadata, provenance, PDF
  identity, and page locators;
- use `status` and `audit` for live corpus state and integrity.

Do not inspect `exports/*.json*`, `metadata/**/*.json`, raw provider JSON,
`state.sqlite3`, extracted page text, or rendered-page directories to discover
evidence. Do not fall back to those files when a live query fails.

Search the multimodal page corpus:

```bash
cargo run --release --locked -- \
  --env-file "projects/$PROJECT_SLUG/.env" \
  query "state one atomic evidence question precisely" \
  --workspace "projects/$PROJECT_SLUG" \
  --top-k 20 --candidate-limit 80
```

Query relational metadata:

```bash
cargo run --release --locked -- \
  catalog --workspace "projects/$PROJECT_SLUG" --status indexed --limit 50
cargo run --release --locked -- \
  inspect-work 'doi:10.1234/example' --workspace "projects/$PROJECT_SLUG"
```

The CLI embeds the text query, retrieves candidate PDF pages from Qdrant, then
reranks each page using the same native-text-plus-image representation used for
indexing. Image-only pages remain supported. Use the returned work ID, page
number, DOI, and canonical citation to inspect and cite the original source.
Join a relevant vector result to SQLite with `inspect-work`, then open the exact
preserved PDF page. Treat vector and rerank scores only as ordering signals.

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
embedded page alone when the canonical record is incomplete. Read exported files
only for explicit citation-manager handoff, requested exchange formats, or audit
review; never use them for corpus discovery or semantic evidence retrieval.

## Consult API contracts

Read [references/api-contracts.md](references/api-contracts.md) before changing
NVIDIA, Qdrant, Crossref, OpenAlex, arXiv, or Semantic Scholar integrations. Keep
raw provider records in SQLite so later metadata corrections remain traceable.

Read [references/release-process.md](references/release-process.md) before changing
the container image, CI workflows, package version, tag, or GitHub Release. A
runtime request never authorizes publishing or changing package visibility.

# Install Academic Literature Mining Skill

Install this skill as a Rust application and translate its neutral subagent contract into the
manifest format supported by the local agent runtime.

## 1. Acquire the canonical repository

Use this repository as the only canonical installation source:

```text
https://github.com/Ruisi-Lu/academic-literature-mining-skill
```

Do not assume that the repository is already present, and do not invent a skill-installation
directory. Read the host agent runtime's authoritative documentation or configuration first and
determine:

1. its native skill root or package-installation mechanism;
2. whether it accepts a Git repository URL directly;
3. whether installed skill folder names must match the `name` in `SKILL.md`.

If the runtime accepts Git URLs, install the canonical URL through that native mechanism. Otherwise,
set `INSTALL_DIR` to the absolute native skill path discovered above. Use
`academic-literature-mining` as the folder name unless the runtime requires a different mapping:

```bash
test -n "${INSTALL_DIR:?set INSTALL_DIR to the runtime's native skill path}"
git clone --depth 1 \
  https://github.com/Ruisi-Lu/academic-literature-mining-skill.git \
  "$INSTALL_DIR"
cd "$INSTALL_DIR"
```

If `INSTALL_DIR` already exists, do not overwrite or delete it. Confirm that it is a clean Git
checkout of the canonical repository before updating it:

```bash
git -C "$INSTALL_DIR" remote get-url origin
git -C "$INSTALL_DIR" status --short
git -C "$INSTALL_DIR" pull --ff-only
```

Stop if the origin differs, the worktree is dirty, or a fast-forward update is impossible. After
installation, record `git -C "$INSTALL_DIR" rev-parse HEAD` and verify that `SKILL.md`,
`INSTALL.MD`, `Cargo.toml`, `rust-toolchain.toml`, and `agents/openai.yaml` exist at the repository
root.

## 2. Inspect the local agent runtime

Before creating a subagent manifest:

1. Locate and read the runtime's authoritative manifest documentation or existing local examples.
2. Identify its exact fields for agent name, model, prompt, tool permissions, concurrency,
   environment inheritance, working directory, timeout, and structured output.
3. Preserve existing repository agent configuration. Add only the role defined here.
4. Never claim installation succeeded until the runtime accepts or validates the generated
   manifest.

Treat [`assets/subagent-manifest.example.json`](assets/subagent-manifest.example.json) as a
portable contract, not as a runtime-ready file. Translate its semantics into the runtime's native
format.

## 3. Map the search-worker contract

Map these concepts to the local manifest:

| Portable concept | Required native behavior |
| --- | --- |
| `role_id` | Create one role named `academic-search-worker` or the closest valid equivalent. |
| `model_class: budget` | Select the cheapest available model that reliably supports web/API search and strict JSON output. |
| `tools.network_read` | Permit read-only web search and scholarly API requests. |
| `tools.filesystem_write` | Restrict writes to the assigned result file or spool directory. |
| `tools.denied` | Deny shell mutation, Docker, Qdrant writes, secrets, messages, and deployment tools. |
| `max_concurrency` | Start with 4; reduce when scholarly API rate limits require it. |
| `input_contract` | Supply one bounded task conforming to `assets/subagent-task.schema.json`. |
| `output_contract` | Require UTF-8 NDJSON conforming to `assets/subagent-result.schema.json`. |
| `prompt_template` | Install `assets/search-subagent-prompt.md` as the role's system/task prompt. |

Do not pass `NVIDIA_API_KEY`, `QDRANT_API_KEY`, `SEMANTIC_SCHOLAR_API_KEY`, or unrelated
environment variables to search workers. The coordinator alone imports and verifies results.

## 4. Shard discovery work

Have the coordinator generate bounded tasks by splitting:

- research-query variants;
- year ranges;
- source families such as Crossref, OpenAlex, arXiv, PubMed, and publisher or repository pages;
- citation directions such as references, cited-by works, systematic reviews, and recent work.

Assign a stable `task_id` to each shard. Require DOI, arXiv ID, or OpenAlex ID for every candidate.
The cheap model may discover candidates, but it must not decide final academic value, open-access
authorization, retraction status, or citation completeness.

Concatenate successful worker outputs into one NDJSON file, then import it:

```bash
cargo run --release --manifest-path Cargo.toml -- \
  import-agent-results spool/candidates.ndjson --workspace corpus
```

The Rust coordinator resolves persistent identifiers against authoritative metadata, deduplicates
records, and applies the quality gates. Invalid or unverifiable worker output is rejected.

## 5. Build the Rust CLI

From this skill directory:

```bash
rustup update stable
rustc --version
cargo build --release --locked
cp .env.example .env
```

The project follows the official Rust stable channel through
`rust-toolchain.toml` and requires Rust `1.97.1` or newer. If Rust is managed
without rustup, install an equivalent current stable toolchain and run Cargo
directly.

Read [`references/user-interaction.md`](references/user-interaction.md) and guide the user through
every missing parameter. Do not ask them to paste a key into chat or expose secret values while
checking configuration. In particular:

- get `NVIDIA_API_KEY` from <https://build.nvidia.com/settings/api-keys>;
- get `OPENALEX_API_KEY` from <https://openalex.org/settings/api> when OpenAlex is selected;
- set `CONTACT_EMAIL` for Crossref requests;
- ask before enabling optional Semantic Scholar enrichment and, if accepted, request its key at
  <https://www.semanticscholar.org/product/api#api-key-form>.

Set real secrets in `.env`. Keep both required model IDs unchanged:

```dotenv
NEMOTRON_EMBED_MODEL_ID=nvidia/llama-nemotron-embed-vl-1b-v2
NEMOTRON_RERANK_MODEL_ID=nvidia/llama-nemotron-rerank-vl-1b-v2
```

## 6. Start Qdrant

The bundled self-hosted Qdrant does not issue a vendor token. Guide the user to generate a strong
local secret (for example, `openssl rand -hex 32`) and set it as `QDRANT_API_KEY` in `.env`.
Docker Compose and the Rust client read the same value. Explain that the next command starts a
persistent localhost-only service, then start it after the user is ready:

```bash
docker compose up -d qdrant
```

Do not expose ports 6333 or 6334 beyond localhost without TLS and network access controls.

## 7. Validate the installation

Explain that `--prepare-pdfium` downloads and caches a native PDFium library. Ask before enabling
that switch, then validate NVIDIA configuration and Qdrant:

```bash
cargo run --release --locked -- \
  doctor --prepare-pdfium --check-qdrant
cargo test --locked
```

`PDFIUM_AUTO_CACHE_DIR` defaults to a skill-local `.cache/pdfium` in `.env.example`. PDFium
extracts each page's embedded native text and renders its complete page image. The pipeline
combines both for model input, performs no OCR, and falls back to image-only for pages without a
usable text layer.

## 8. Run a corpus

Before editing the plan, ask whether to enable `include_preprints`, `include_paywalled`,
`use_google_scholar_library_access`, and optional Semantic Scholar enrichment. All are disabled by
default. Enabling paywalled access requires the lawful-access and external-processing confirmation
in [`references/user-interaction.md`](references/user-interaction.md).

```bash
cargo run --release --locked -- \
  mine --plan assets/research-plan.example.json \
  --workspace corpus --max-candidates 1000
```

For controlled or resumable execution, run `discover`, `screen`, `download`, `render`, `ingest`,
and `export` separately. Inspect `target/release/litmine --help` for exact options.

When paywalled searching is enabled, `download` returns official publisher/DOI links and one exact
destination path per pending PDF. If the separately approved Google Scholar option is enabled,
follow [`references/chrome-library-access.md`](references/chrome-library-access.md); otherwise
present the links and paths to the user and pause. After the file is placed, rerun `download` to
validate, hash, and register it, then continue with `render`, `ingest`, `audit`, and `export`.
Never automate restricted access or receive publisher credentials.

Chrome DevTools MCP is a user-scope optional dependency, not a repository dependency. Do not add
Node/npm, `.prototools`, Chrome settings, or an MCP manifest here. If `chrome-devtools` is absent,
explain the official setup and security implications, request explicit authorization, and only
then install it through the host runtime with proto-managed Node/npm. Reload the runtime and verify
the tools before use; without authorization, retain the manual fallback.

When upgrading a workspace created with version `0.1.0`, run
`refresh-metadata --workspace corpus` followed by
`screen --plan <preserved-plan> --workspace corpus`. This re-resolves
DOI-backed arXiv preprints through Crossref and queues verified formal versions
for screening without discarding the authorized arXiv PDF or provenance.
`screen` also performs the metadata refresh automatically.

When upgrading a corpus produced by the older image-only pipeline, run
`render --refresh-existing` followed by `ingest` once. The SQLite migration is automatic, while
the explicit refresh avoids unexpectedly rerendering every indexed PDF during normal runs.

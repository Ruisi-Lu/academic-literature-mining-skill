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

Set real secrets in `.env`. Keep both required model IDs unchanged:

```dotenv
NEMOTRON_EMBED_MODEL_ID=nvidia/llama-nemotron-embed-vl-1b-v2
NEMOTRON_RERANK_MODEL_ID=nvidia/llama-nemotron-rerank-vl-1b-v2
```

## 6. Start Qdrant

Set the same strong `QDRANT_API_KEY` in `.env`, then start the localhost-only service:

```bash
docker compose up -d qdrant
```

Do not expose ports 6333 or 6334 beyond localhost without TLS and network access controls.

## 7. Validate the installation

Download and cache PDFium through the Rust binary, validate NVIDIA configuration, and check
Qdrant:

```bash
cargo run --release --locked -- \
  doctor --prepare-pdfium --check-qdrant
cargo test --locked
```

`PDFIUM_AUTO_CACHE_DIR` defaults to a skill-local `.cache/pdfium` in `.env.example`. PDFium
renders PDF pages to images; the pipeline performs no OCR and extracts no PDF text.

## 8. Run a corpus

```bash
cargo run --release --locked -- \
  mine --plan assets/research-plan.example.json \
  --workspace corpus --max-candidates 1000
```

For controlled or resumable execution, run `discover`, `screen`, `download`, `render`, `ingest`,
and `export` separately. Inspect `target/release/litmine --help` for exact options.

# User setup, consent, and manual handoffs

Read this file before inspecting credentials, creating a research plan, starting a local service,
downloading PDFium, or asking the user to supply a PDF.

## Choose an execution mode

Ask whether the user wants the release-pinned prebuilt Docker image or a native Rust build. The
image avoids a host Rust toolchain and already contains PDFium; native mode avoids downloading the
CLI image. Treat no answer as a request for guidance only.

For container mode, retain the complete canonical Git checkout and read version `X.Y.Z` from its
`Cargo.toml`. Verify the clone's exact `vX.Y.Z` tag and published, non-draft GitHub Release, then
set `LITMINE_VERSION=X.Y.Z` in `.env` and pull only
`ghcr.io/ruisi-lu/academic-literature-mining-skill:X.Y.Z`. Never choose `latest`, an unverified
tag, or a release version different from the checkout. Explain the network and disk impact before
pulling. If Docker is missing, give the applicable official Docker installation URL, explain the
host-level changes, and install only after explicit authorization; otherwise offer native mode.

## Handle missing parameters

Never stop at “missing token” or ask the user to paste a secret into chat. For every missing value:

1. Name the stage that needs it and whether it is required or optional.
2. Give the official acquisition or creation URL.
3. Tell the user to copy `.env.example` to the skill-root `.env` if `.env` does not exist.
4. Show the exact variable assignment with a placeholder, never the real value.
5. Explain the verification command and wait for the user to confirm setup is complete.

Inspect only whether a value is present and non-placeholder. Never print, quote, log, or expose its
value. `.env` is ignored by Git; keep it out of subagent manifests and search-worker environments.
When `--env-file` is used, apply the same rules to that file.

| Setting | When needed | User action |
| --- | --- | --- |
| `NVIDIA_API_KEY` | `doctor`, `screen`, `ingest`, `query`, and `mine` | Sign in at <https://build.nvidia.com/settings/api-keys>, generate a key, and set `NVIDIA_API_KEY=<key>` in `.env`. Verify with `litmine doctor`; do not change the two pinned model IDs. |
| `OPENALEX_API_KEY` | Any plan whose `sources` contains `openalex`, or an OpenAlex-only lookup | Create an OpenAlex account, copy the key from <https://openalex.org/settings/api>, and set `OPENALEX_API_KEY=<key>` in `.env`. If the user declines, ask before removing `openalex` from `sources`. |
| `CONTACT_EMAIL` | Crossref discovery or DOI resolution | Set `CONTACT_EMAIL=<address>` in `.env` for Crossref polite-pool requests. This is an email address, not a token. |
| `QDRANT_API_KEY` | Bundled local Qdrant and all `ingest` or `query` operations | No vendor token is issued for the bundled self-hosted service. Have the user generate a strong secret, for example with `openssl rand -hex 32`, and set `QDRANT_API_KEY=<secret>` in `.env`; Docker Compose and `litmine` read the same value. For Qdrant Cloud, direct the user to the cluster’s Database API Keys page instead. |
| `SEMANTIC_SCHOLAR_API_KEY` | Optional Semantic Scholar enrichment only | Ask whether to enable this enrichment. If yes, request a key at <https://www.semanticscholar.org/product/api#api-key-form>, wait for the emailed key, and set `SEMANTIC_SCHOLAR_API_KEY=<key>` in `.env`. If no, leave it empty. |

After container setup, run:

```bash
docker compose run --rm litmine doctor --check-qdrant
```

After native setup, run:

```bash
cargo run --release --locked -- doctor --check-qdrant
```

On first native use, explain that `--prepare-pdfium` downloads and caches a native PDFium library,
ask before enabling it, and then run:

```bash
cargo run --release --locked -- doctor --prepare-pdfium --check-qdrant
```

The prebuilt image already contains a read-only PDFium cache; do not run `--prepare-pdfium` for it.
Explain that `docker compose up -d qdrant` starts a localhost-bound persistent service before
running it. If Docker, Rust, an agent manifest, institutional authentication, or any other manual
prerequisite is unavailable, give the exact next action and wait instead of claiming success.

## Ask about every optional switch

Before editing a plan or enabling optional behavior, ask a compact explicit question for each
applicable switch. Treat no answer as disabled; never infer consent from an existing key or file.

- `include_preprints` (default `false`): ask whether standalone preprints may be considered under
  the strict exception policy.
- `include_paywalled` (default `false`): ask whether to search and retain subscription/paywalled
  journal articles that may require a manual PDF handoff.
- `use_google_scholar_library_access` (default `false`): after paywalled opt-in, ask whether an
  existing or separately authorized Chrome DevTools MCP may use the user's dedicated, already
  authenticated Chrome profile to resolve pending DOIs through Google Scholar library links. Read
  [chrome-library-access.md](chrome-library-access.md) before offering it.
- Semantic Scholar enrichment (default off): ask whether to use it and explain that it needs the
  optional key above.
- `--refresh-existing` (default off): ask before rebuilding already rendered or indexed pages.

Before enabling `include_paywalled`, tell the user that automatic paywall access remains forbidden.
Require confirmation that they will use lawful personal or institutional access and that their
terms permit local preservation and sending page text/images to the configured NVIDIA service.
If external processing is not permitted, do not ingest that PDF with this runtime.

Never add Chrome DevTools MCP or its Node/npm prerequisites to this repository. If the optional
browser switch is accepted but the MCP is absent, provide the official setup links and disclose
the user-level configuration, network, and authenticated-browser impact. Install only after the
user explicitly authorizes it, keep Node/npm under `proto`, and follow
[chrome-library-access.md](chrome-library-access.md). Without authorization, use the manual
fallback.

## Complete a paywalled PDF handoff

When `include_paywalled` is `true`, run discovery and screening normally. `litmine download`
automatically downloads only independently authorized full text and returns `manual_downloads`
for the remainder. For every returned item:

1. Present its title, DOI, and each `download_urls` value as a clickable link.
2. Present the exact absolute `destination` path.
3. If the plan also enables `use_google_scholar_library_access` and the item has a
   `google_scholar_query_url`, follow
   [chrome-library-access.md](chrome-library-access.md). Otherwise tell the user to authenticate
   with the publisher or institution themselves, download the correct PDF without bypassing
   access controls, save it exactly at `destination`, and confirm when ready.
4. For the manual branch, pause until the user confirms placement. For a successful authorized
   Chrome branch, continue as soon as the completed file is safely at `destination`. Never request
   credentials, cookies, a token, or the PDF through chat.
5. Rerun `litmine download --workspace <workspace>`. It validates that the destination is a
   regular non-symlink PDF within the size limit, hashes it, records manual provenance, and changes
   the work to `downloaded`.
6. Verify that the PDF title/authors/DOI match the selected work, then continue with `render`,
   `ingest`, `audit`, and `export`. Report any remaining `manual_downloads` or failures.

Record user-supplied PDFs as `user-supplied; reuse rights not established`. Lawful access does not
create an open license. Never redistribute the PDF or describe it as open access.

# External API contracts

Read this file before changing endpoints, payload formats, model IDs, or collection dimensions.
Recheck authoritative vendor documentation when APIs may have changed.

## NVIDIA Build

Use:

- embeddings: `POST https://integrate.api.nvidia.com/v1/embeddings`
- model: `nvidia/llama-nemotron-embed-vl-1b-v2`
- reranking: `POST https://ai.api.nvidia.com/v1/retrieval/nvidia/llama-nemotron-rerank-vl-1b-v2/reranking`
- model: `nvidia/llama-nemotron-rerank-vl-1b-v2`
- authentication: `Authorization: Bearer $NVIDIA_API_KEY`

Embed digital document pages with `input_type: "passage"`, `modality: "text_image"`, float
output, embedded native page text, and a base64 image data URL for the complete rendered page.
The API accepts a modality array whose entries align with the input array. Use `modality: "image"`
for individual pages whose native text is empty. Embed retrieval questions with
`input_type: "query"` and `modality: "text"`.

Rerank a text query against passages containing both `text` and `image` data URLs. For image-only
pages, omit `text` rather than sending an empty string. Preserve the returned `index` when mapping
logits back to candidates. Preserve both the raw logit and sigmoid-normalized score; do not
interpret the sigmoid value as a calibrated relevance probability without project-specific
validation.

The current embeddings request contract accepts text and image inputs, not an
`application/pdf` data URL. Treat the PDF as the source document and render each complete page to
an image with PDFium. Extract only the embedded native text layer for the same page and combine it
with the page image as `text_image`; do not OCR or create semantic text chunks. The image preserves
layout, tables, charts, formulas, figures, and text-order information that native extraction can
lose. Fall back to image-only for scans or extraction failures.

Keep each decoded page image below 25 MiB. The default vector size is 2048. Re-ingest into a new
collection if the embedding model or vector dimension changes.

## Qdrant

Run Qdrant with `docker-compose.yml`, pinned to `qdrant/qdrant:v1.18.2`. Use one collection with a
named `content` vector, cosine distance, on-disk vectors, and on-disk payloads.

Create payload indexes only for frequent filters:

- `work_id`: keyword
- `record_type`: keyword
- `citation.DOI`: keyword
- `publication_year`: integer
- `quality.tier`: keyword

Use deterministic UUID page IDs so retries are idempotent. Wait for upserts before marking SQLite
pages as indexed.

## Scholarly sources

- OpenAlex: require `OPENALEX_API_KEY`; obtain it from <https://openalex.org/settings/api>, use
  cursor paging, and retain raw work objects.
- Crossref: use the polite pool by supplying `CONTACT_EMAIL`; use it as the authoritative DOI
  citation enrichment source.
- arXiv: respect the API delay and treat repository PDFs as authorized submitted versions.
- Semantic Scholar: enrich citations and open-access locations only when an API key is available.

Never treat a Crossref TDM link alone as proof of open access. Authorize downloads only from an
open-access metadata assertion, an open license, or a recognized public repository. Never use
Sci-Hub, bypass access controls, or scrape paywalled full text.

When the user explicitly enables paywalled searching, return public DOI and publisher landing
URLs only. The user must authenticate and download through lawful access, save the PDF at the
coordinator-provided path, and confirm completion. Do not receive or automate their credentials.

## Authoritative documentation

- NVIDIA Embed VL API:
  <https://docs.nvidia.com/nim/nemo-retriever/text-embedding/2.0.0/reference.html>
- NVIDIA Build Embed VL endpoint:
  <https://docs.api.nvidia.com/nim/re/reference/nvidia-llama-nemotron-embed-vl-1b-v2-infer>
- NVIDIA Rerank VL API:
  <https://docs.nvidia.com/nim/nemo-retriever/text-reranking/latest/use-the-api-openai.html>
- NVIDIA Build Rerank VL endpoint:
  <https://docs.api.nvidia.com/nim/reference/nvidia-llama-nemotron-rerank-vl-1b-v2-infer>
- Qdrant installation and monitoring:
  <https://qdrant.tech/documentation/installation/> and
  <https://qdrant.tech/documentation/ops-monitoring/monitoring/>
- Crossref REST API and text/data-mining guidance:
  <https://www.crossref.org/documentation/retrieve-metadata/rest-api/> and
  <https://www.crossref.org/documentation/retrieve-metadata/rest-api/text-and-data-mining/>
- OpenAlex API overview: <https://docs.openalex.org/>
- arXiv API user manual: <https://info.arxiv.org/help/api/user-manual.html>
- Semantic Scholar Academic Graph API:
  <https://api.semanticscholar.org/api-docs/graph>

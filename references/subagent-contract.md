# Subagent discovery contract

Use this reference when installing or operating cheap-model search workers.

## Trust boundary

Search workers are untrusted candidate generators. They may find useful identifiers quickly, but
their claims are not accepted as citation metadata or quality evidence. Import their NDJSON through
`litmine import-agent-results`; never write their output directly to Qdrant.

The coordinator must:

1. validate every result against the JSON schema;
2. resolve DOI metadata through Crossref and repository identifiers through authoritative APIs;
3. deduplicate canonical records;
4. run retraction and scholarly-type exclusions;
5. calculate academic-value signals;
6. use Nemotron Rerank for research-question relevance;
7. download only independently authorized open-access PDFs;
8. export and audit citations.

## Task partitioning

Keep a worker task bounded to one query, one source set, and preferably one date range. Use stable
task IDs such as `q03-openalex-2020-2023`. Avoid assigning the entire review to one cheap model.

Good partitions include:

- terminology and synonym variants;
- references of known seed papers;
- cited-by works;
- systematic reviews and meta-analyses;
- recent work with low citation counts;
- geographic or language-specific coverage.

## Failure handling

Retry a failed shard at most twice. Record the model, task ID, timestamps, and error outside the
candidate stream. Do not relax the persistent-identifier requirement to increase yield.

If two workers disagree, preserve both evidence URLs and let authoritative metadata resolution
decide. Never ask a worker to fabricate missing authors, dates, DOI values, page ranges, or venues.

# Per-paper runtime isolation

Read this file before creating, starting, migrating, backing up, or removing a corpus. Treat each
paper or manuscript as a separate security and retrieval boundary.

## Required layout

Use one unique lowercase slug per paper. Keep its complete writable workspace at
`projects/<slug>/`; this directory contains its `.env`, research plan, `state.sqlite3`, PDFs,
rendered pages, metadata, logs, and exports. The repository ignores all of `projects/` so secrets
and research artifacts cannot enter Git accidentally.

Compose derives project name `litmine-<slug>` from `LITMINE_PROJECT`. That creates a distinct
network, Qdrant container, and `litmine-<slug>_qdrant_storage` volume. Container mode publishes no
Qdrant host port. SQLite is an embedded file inside the workspace, not a network service, so it
must not be modeled as a separate Compose container.

The CLI also persists a random `corpus_id` in each workspace. Every Qdrant point ID and payload is
scoped to it, and every vector query applies a mandatory `corpus_id` filter. This is defense in
depth for custom or cloud deployments that intentionally share one Qdrant server.

Never use one project for unrelated papers, point two slugs at the same workspace, or copy an
existing workspace to start a new paper. Create an empty workspace so it receives a new
`corpus_id`. Reuse a project only when the user explicitly wants the manuscripts to share one
corpus.

## Create a container project

Ask the user to choose or confirm a filesystem-safe slug matching `[a-z0-9][a-z0-9_-]*`. Check
that the destination does not exist; never overwrite it. Then create and prepare it:

```bash
PROJECT_SLUG=paper-a
test ! -e "projects/$PROJECT_SLUG"
mkdir -p "projects/$PROJECT_SLUG"
cp .env.example "projects/$PROJECT_SLUG/.env"
cp assets/research-plan.example.json "projects/$PROJECT_SLUG/research-plan.json"
```

Replace `paper-a` before running the commands. Set `LITMINE_PROJECT` in the new `.env` to the exact
same slug, set the release version and required credentials, and edit only that project's plan.
Never print secret values while checking the file.

Use the same env file on every Compose call:

```bash
docker compose --env-file "projects/$PROJECT_SLUG/.env" pull litmine
docker compose --env-file "projects/$PROJECT_SLUG/.env" up -d qdrant
docker compose --env-file "projects/$PROJECT_SLUG/.env" run --rm litmine \
  doctor --check-qdrant
docker compose --env-file "projects/$PROJECT_SLUG/.env" run --rm litmine \
  init --workspace /workspace
```

Inside the container, always use `/workspace` as the workspace and
`/workspace/research-plan.json` as the plan. The bind mount exposes the same files at
`projects/<slug>/` on the host, including manual PDF destinations.

## Use native Rust mode

Native mode uses the same project directory and Qdrant volume, but requires an explicit loopback
override. Choose a host port not used by another running paper, set `QDRANT_HOST_PORT` to it, and
set `QDRANT_URL=http://localhost:<same-port>` in the project `.env`. Then run:

```bash
docker compose \
  -f docker-compose.yml -f docker-compose.native.yml \
  --env-file "projects/$PROJECT_SLUG/.env" up -d qdrant
target/release/litmine --env-file "projects/$PROJECT_SLUG/.env" \
  init --workspace "projects/$PROJECT_SLUG"
```

The override binds only `127.0.0.1`; never publish it on an external interface without separate
TLS and access-control design.

## Stop, back up, and migrate safely

`docker compose --env-file projects/<slug>/.env down` stops only that paper and retains its Qdrant
volume. The host workspace remains directly backupable. Never run `down --volumes` unless the user
explicitly authorizes deletion of that exact paper's vector index; report that the volume is
deleted even though vectors can be regenerated from the preserved workspace.

For a pre-isolation workspace, stop its old runtime, obtain authorization before moving or copying
research data, and place it under one chosen `projects/<slug>/`. Copy the new `.env.example`, carry
secret values without displaying them, set `QDRANT_COLLECTION=academic_literature_v2`, and run
`litmine init --workspace <workspace>` once. The migration assigns `corpus_id` and queues legacy
indexed pages for re-ingestion. Run `ingest`, `audit`, and `status` before using the corpus. Keep the
old Qdrant volume until the user verifies the migrated corpus and separately authorizes removal.

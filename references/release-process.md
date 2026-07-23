# Release and container publishing

Read this file only when maintaining or publishing this skill. Installing and running a corpus do
not authorize a tag, push, package-visibility change, or GitHub Release.

## Prepare one immutable version

Keep these values identical before tagging:

- `[package].version` in `Cargo.toml` and the root package entry in `Cargo.lock`;
- `LITMINE_VERSION` in `.env.example`;
- the fallback `LITMINE_VERSION` in `docker-compose.yml`.

Use a semantic version and tag it as `vX.Y.Z`. Run the full local validation suite and inspect a
clean worktree. Creating or pushing a tag is an external repository change: show the exact commit
and tag to the user and obtain explicit authorization first. Prefer a signed, annotated tag. Never
move or reuse a published tag; release a new patch version instead.

## Automated tag workflow

Pushing `vX.Y.Z` starts `.github/workflows/release.yml`. It:

1. requires the tag, Cargo package, environment example, and Compose fallback versions to match;
2. runs formatting, Clippy, tests, the release build, and JSON validation;
3. builds Linux `amd64` and `arm64` images with the binary and PDFium cache;
4. publishes exact, minor, and—only for a stable tag—`latest` OCI tags to
   `ghcr.io/ruisi-lu/academic-literature-mining-skill`;
5. publishes SBOM/provenance metadata and a GitHub artifact attestation;
6. creates the GitHub Release only after the image and attestation succeed.

The workflow uses the repository-scoped `GITHUB_TOKEN`; do not add NVIDIA, Qdrant, scholarly API,
browser, or publisher credentials. GitHub's official container publishing guide is
<https://docs.github.com/en/actions/tutorials/publish-packages/publish-docker-images>.

## First-package manual check

After the first successful workflow, the repository owner must inspect the package settings and
confirm that the package is linked to this repository and publicly readable if public,
unauthenticated installation is intended. Package visibility is an account-level change; explain
the impact and ask the owner to perform or explicitly authorize it. Do not collect a personal
access token in chat. Follow GitHub's Container registry documentation:
<https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry>.

Verify the exact release without relying on `latest`:

```bash
docker buildx imagetools inspect \
  ghcr.io/ruisi-lu/academic-literature-mining-skill:X.Y.Z
gh attestation verify \
  oci://ghcr.io/ruisi-lu/academic-literature-mining-skill:X.Y.Z \
  --repo Ruisi-Lu/academic-literature-mining-skill
```

Confirm both advertised architectures and the exact `org.opencontainers.image.version` label.
If any publishing job fails, leave the release unpublished, diagnose the workflow, and rerun the
same immutable tag only when no artifact under that tag was successfully published. Otherwise
issue a new patch version.

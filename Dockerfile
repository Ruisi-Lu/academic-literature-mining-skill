# syntax=docker/dockerfile:1.18

ARG RUST_VERSION=1.97.1

FROM rust:${RUST_VERSION}-bookworm AS builder

ARG TARGETARCH

WORKDIR /build

COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src

RUN --mount=type=cache,id=litmine-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=litmine-target,target=/build/target,sharing=locked \
    CARGO_TARGET_DIR="/build/target/${TARGETARCH}" cargo build --release --locked \
    && cp "/build/target/${TARGETARCH}/release/litmine" /tmp/litmine

# Cache the platform-specific PDFium library in the image. The placeholder only satisfies the
# local configuration preflight; the doctor command does not contact NVIDIA in this step.
ENV PDFIUM_AUTO_CACHE_DIR=/opt/litmine/pdfium
RUN NVIDIA_API_KEY=container-build-placeholder \
    /tmp/litmine doctor --prepare-pdfium

FROM debian:bookworm-slim AS runtime

ARG LITMINE_VERSION=dev
ARG VCS_REF=unknown

LABEL org.opencontainers.image.title="Academic Literature Mining" \
      org.opencontainers.image.description="Citation-safe scholarly corpus mining with multimodal PDF retrieval" \
      org.opencontainers.image.source="https://github.com/Ruisi-Lu/academic-literature-mining-skill" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.version="${LITMINE_VERSION}" \
      org.opencontainers.image.revision="${VCS_REF}"

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        ca-certificates \
        fonts-dejavu-core \
        libgcc-s1 \
        libstdc++6 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 litmine \
    && useradd --uid 10001 --gid 10001 --no-create-home --home-dir /tmp litmine \
    && install --directory --owner=10001 --group=10001 /workspace

COPY --from=builder --chmod=0555 /tmp/litmine /usr/local/bin/litmine
COPY --from=builder /opt/litmine/pdfium /opt/litmine/pdfium

ENV HOME=/tmp \
    PDFIUM_AUTO_CACHE_DIR=/opt/litmine/pdfium

WORKDIR /workspace
USER 10001:10001

ENTRYPOINT ["litmine"]
CMD ["--help"]

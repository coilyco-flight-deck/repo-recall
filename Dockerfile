# syntax=docker/dockerfile:1.7
# -----------------------------------------------------------------------------
# repo-recall API: Rust axum + MCP server. Sibling to Dockerfile.web (caddy
# fronts the static SPA, this image serves /api, /openapi.json, /mcp on :7777).
#
# Runtime layer carries `git` and `gh` because the ingest pass shells out to
# both (see AGENTS.md "No GraphQL, one exception"). Filesystem mounts that
# would expose the host project tree and the Claude session JSONL are wired
# up at deploy time, not here.
# -----------------------------------------------------------------------------

# Stage 1: cargo build with deps-only caching.
FROM rust:1-bookworm AS builder
WORKDIR /build

# Cache dependency compile by copying only the manifests first and building a
# dummy binary. Anything past the first COPY src invalidates only the final
# compile, not the dep graph.
COPY Cargo.toml Cargo.lock build.rs ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && \
    echo '' > src/lib.rs && \
    cargo build --release --locked && \
    rm -rf src target/release/deps/repo_recall* target/release/repo-recall*

COPY src ./src
COPY tests ./tests

# build.rs prefers $REPO_RECALL_VERSION over `git describe`. The release
# workflow can set this when building from a tag; for plain main builds the
# fallback writes "0.0.0-dev+<sha>" which is fine for image labelling.
ARG REPO_RECALL_VERSION
ENV REPO_RECALL_VERSION=${REPO_RECALL_VERSION}
RUN cargo build --release --locked --bin repo-recall

# Stage 2: slim runtime. `gh` needs its own apt repo (cli.github.com).
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        git \
        gnupg \
        tini \
    && install -m 0755 -d /etc/apt/keyrings \
    && curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
        | gpg --dearmor -o /etc/apt/keyrings/githubcli-archive-keyring.gpg \
    && chmod go+r /etc/apt/keyrings/githubcli-archive-keyring.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
        > /etc/apt/sources.list.d/github-cli.list \
    && apt-get update && apt-get install -y --no-install-recommends gh \
    && apt-get purge -y gnupg \
    && apt-get autoremove -y \
    && rm -rf /var/lib/apt/lists/*

# Non-root. UID/GID 1000 matches forgejo's pattern and the local-path PVC
# defaults k3s ships with.
RUN groupadd --system --gid 1000 repo-recall && \
    useradd --system --uid 1000 --gid 1000 --home /home/repo-recall \
            --create-home --shell /usr/sbin/nologin repo-recall

COPY --from=builder /build/target/release/repo-recall /usr/local/bin/repo-recall

USER repo-recall
WORKDIR /home/repo-recall

ENV REPO_RECALL_HOST=0.0.0.0 \
    REPO_RECALL_PORT=7777

EXPOSE 7777
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD ["curl", "-fsS", "http://127.0.0.1:7777/api/scan-version"]

ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["/usr/local/bin/repo-recall"]

# Factory container images (P7 local live smoke).
#
# One shared `builder` stage compiles the whole workspace once; two runtime targets
# select what ships:
#   --target services  → the long-running services (intake/tools/dispatcher/
#                        coordinator/dashboard-api). Slim; no git/claude.
#   --target worker    → the per-task worker: the worker binary + `git` + the
#                        `claude` CLI. This is the image `DockerLauncher` runs.
#
# Build both (see docs/runbooks/local-live-smoke.md):
#   docker build --target worker   -t factory-worker:local .
#   docker compose -f docker-compose.yml -f docker-compose.live.yml build
#
# rust-version 1.88+ is required by the azure_storage crates; rust:1-bookworm is
# stable and well past that.

# ---- builder: compile the workspace release once ----
FROM rust:1-bookworm AS builder
WORKDIR /src
# Copy the full workspace (Cargo.lock is committed; --locked keeps CI/image in sync).
COPY . .
RUN cargo build --release --locked \
    -p factory-intake \
    -p factory-tools \
    -p factory-dispatcher \
    -p factory-coordinator \
    -p factory-dashboard-api \
    -p factory-worker

# ---- services: the long-running service binaries ----
FROM debian:bookworm-slim AS services
# rustls builds need CA roots to reach GitHub + Azure over TLS.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder \
    /src/target/release/factory-intake \
    /src/target/release/factory-tools \
    /src/target/release/factory-dispatcher \
    /src/target/release/factory-coordinator \
    /src/target/release/factory-dashboard-api \
    /usr/local/bin/
# The dispatcher launches worker containers via `docker run` against the host's
# mounted socket, so it needs the docker CLI client (only the client, not a daemon).
COPY --from=docker:27-cli /usr/local/bin/docker /usr/local/bin/docker
# No default CMD: each compose service sets its own `command:` (the binary name).

# ---- worker: worker binary + git + claude CLI ----
# node:20-bookworm-slim gives node+npm for the claude CLI; we add git + CA roots.
FROM node:20-bookworm-slim AS worker
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git \
    && rm -rf /var/lib/apt/lists/* \
    && npm install -g @anthropic-ai/claude-code \
    && useradd --create-home --uid 10001 factory
COPY --from=builder /src/target/release/factory-worker /usr/local/bin/factory-worker
# Run as a NON-root user: claude refuses `--dangerously-skip-permissions` under
# root/sudo. The worker writes its worktree + scratch under $TMPDIR (world-writable)
# and its checkpoint session under CLAUDE_CONFIG_DIR in this user's home.
USER factory
ENV HOME=/home/factory
# The worker tars this dir into each checkpoint (claude session state) so a resume
# can `claude --resume`; auth (CLAUDE_CODE_OAUTH_TOKEN) is injected per run by the
# dispatcher. Keep it on the container-local fs — the archive carries it across.
ENV CLAUDE_CONFIG_DIR=/home/factory/.claude
ENTRYPOINT ["/usr/local/bin/factory-worker"]

# Local live smoke (P7)

Prove the **real deploy seams** end-to-end on your machine — real GitHub App, real
`claude`, real Docker worker, real Azure Blob (Azurite) — against a throwaway repo,
*before* Azure infra (P8). This is a **local-dev capability, not CI**: it needs a real
GitHub App webhook, a paid `claude`, and Docker. CI keeps the fixture `e2e` suite plus
the Azurite-gated `libs/blob` integration tests.

What it exercises that CI fakes: `OctocrabGitHub` (real GitHub), `ClaudeCli` (real
`claude --resume`), `GitCli` (real clone/commit/push), `DockerLauncher` (real
`docker run`), and the Azure Blob `CheckpointStore`/`TranscriptStore` (against
Azurite). The crash-resume drill kills a worker mid-run and proves a re-dispatch
restores its checkpoint from Blob and resumes the same `claude` session.

## Prerequisites

| Need | Why |
|------|-----|
| Docker + `docker compose` | Postgres, Azurite, the services, the worker image |
| `gh` CLI (authenticated) | webhook forwarding + driving/observing the smoke |
| `jq`, `curl` | `scripts/smoke.sh` |
| `claude` CLI | to mint the setup-token (`npm i -g @anthropic-ai/claude-code`) |
| A throwaway GitHub repo | Factory mutates it (branches, PRs, comments) |

## 1. Register a GitHub App

GitHub → Settings → Developer settings → **GitHub Apps → New GitHub App**.

- **Webhook URL:** a public URL that forwards to your machine — simplest is
  `gh webhook forward` (step 6), which prints one; or smee.io / ngrok → your
  `http://localhost:8080/webhooks/github`.
- **Webhook secret:** generate one; put the same value in `.env.live`
  (`GITHUB_WEBHOOK_SECRET`). Intake rejects any delivery whose HMAC doesn't match.
- **Repository permissions:** `Issues`, `Pull requests`, `Contents`, `Checks` →
  **Read & write**.
- **Subscribe to events:** `Issues`, `Pull request`, `Pull request review`,
  `Issue comment`, `Push`.
- Create the App, then **Generate a private key** (downloads a `.pem`).
- **Install** the App on your throwaway repo (Install App → choose the repo).

Record: the **App ID**, the **private key**, and the **installation** (installing on
the repo is enough — Factory mints installation tokens by App).

## 2. Prepare the throwaway repo

```bash
REPO=you/factory-smoke           # your throwaway repo
gh label create factory -R "$REPO" --description "Factory opt-in" --color 5319e7
```

Commit a `.factory/config.yml` defining a minimal team (manager + coder + reviewer):

```yaml
# .factory/config.yml
version: 1
settings:
  label: factory
  default_model: claude-opus-4-8
limits:
  max_concurrent_workers: 3
workers:
  - name: manager
    role: manager
    on: [issue_labeled, cr_opened, manager]
    max_concurrent: 1
  - name: coder
    role: coder
    on: [manager, cr_changes_requested]
    max_concurrent: 2
  - name: reviewer
    role: reviewer
    on: [cr_opened, cr_updated]
    max_concurrent: 1
```

## 3. Mint a `claude` setup-token

```bash
claude login              # interactive, once (uses your subscription)
claude setup-token        # prints a long-lived token
```

Put it in `.env.live` as `CLAUDE_CODE_OAUTH_TOKEN`. The dispatcher injects it into
each worker container as an env var — no host `~/.claude` is mounted (this mirrors how
P8 injects a Key Vault secret into Container Apps).

## 4. Fill in `.env.live`

```bash
cp .env.live.example .env.live
```

Edit it: `GITHUB_APP_ID`, `GITHUB_APP_PRIVATE_KEY` (single line, `\n`-escaped),
`GITHUB_WEBHOOK_SECRET`, `CLAUDE_CODE_OAUTH_TOKEN`. The Azurite
`FACTORY_BLOB_CONNECTION_STRING`, `FACTORY_WORKER_IMAGE`, and `FACTORY_TOOLS_URL`
defaults already point at the local stack. `.env.live` is gitignored.

> **PEM as one line:** `awk 'NF {printf "%s\\n", $0}' app.private-key.pem` then paste
> after `GITHUB_APP_PRIVATE_KEY=`.

## 5. Build images + start the stack

```bash
# The worker image DockerLauncher runs (built on the host so the host daemon has it):
docker build --target worker -t factory-worker:local .

# Postgres + Azurite + all services (services self-migrate on startup):
docker compose -f docker-compose.yml -f docker-compose.live.yml --env-file .env.live up --build -d

# Sanity:
curl -fsS localhost:8081/healthz   # tools
curl -fsS localhost:8082/healthz   # dashboard-api
```

## 6. Forward webhooks to localhost

```bash
gh webhook forward --repo="$REPO" --events="issues,pull_request,pull_request_review,issue_comment,push" \
  --url="http://localhost:8080/webhooks/github" --secret="<your GITHUB_WEBHOOK_SECRET>"
```

Use the public URL it prints as the App's Webhook URL (step 1) if you didn't already.

## 7. Run the smoke

```bash
FACTORY_SMOKE_REPO="$REPO" ./scripts/smoke.sh                 # all scenarios
FACTORY_SMOKE_REPO="$REPO" ./scripts/smoke.sh happy           # a subset (positional)
```

With no arguments it runs all scenarios: **config** (push reloads `.factory/config.yml`),
**happy** (issue → manager triage → coder change-request → reviewer verdict), **rework**
(a "changes requested" review → a coder runs on the **same** CR, with **no duplicate CR**
opened — it does not require a new commit, since whether claude makes one is its own
judgment), and **crash** (kill the worker container mid-run → the sweeper reclaims the
lapsed lease → the dispatcher re-dispatches → the issue **still produces a CR**). Reclaim
latency is the lease TTL — set `FACTORY_LEASE_TTL_SECS` low (e.g. 120) for a fast crash
run. Select a subset with positional args, e.g. `./scripts/smoke.sh happy crash`. It
prints a PASS/FAIL matrix at the end.

Observe live at the dashboard: `http://localhost:8082/` (work items, events, SSE).
Transcripts land in the Azurite `transcripts` container; checkpoints in `checkpoints`
(inspect with Azure Storage Explorer / `az storage blob` against the emulator).

## 8. Teardown

```bash
docker compose -f docker-compose.yml -f docker-compose.live.yml --env-file .env.live down -v
docker rm -f $(docker ps -aq --filter ancestor=factory-worker:local) 2>/dev/null || true
```

## Notes / gotchas

- **Worker networking:** workers run as standalone containers (launched off the
  compose network via the host docker socket), so they reach Tools + Azurite at
  `host.docker.internal`. `DockerLauncher` adds `--add-host
  host.docker.internal:host-gateway` so this resolves on Linux too.
- **The dispatcher needs the docker socket** (`/var/run/docker.sock`, mounted in the
  live compose) and the `factory-worker:local` image present on the host.
- **dev↔prod divergence is deliberately minimal.** The only storage fork is the Blob
  *credential*: shared-key/emulator here vs. **managed identity** in P8 — the read/
  write data path is identical and is CI-tested against Azurite. P8 swaps the
  credential and the `WorkerLauncher` impl (Container Apps Jobs), nothing else.

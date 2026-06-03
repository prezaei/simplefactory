# P8 — Azure productionization: implementation blueprint

Status: **design** (Phase 0 of `/implement-e2e`). Architecture is already locked in
`docs/factory-technical-spec.md` (Infrastructure & sizing; Security & least privilege) and
`docs/factory-implementation-plan.md` ("## P8"). This doc is the *implementation blueprint*:
the files to add/change, the seams, the test contract (design → tests → code), the telemetry,
and the build sequence. **Scope (locked with the user): author IaC + CD + deploy-seam impls,
green/buildable in CI; the live `az`/CD deploy is operator-run** (mirrors P7's build-vs-live split).

## Locked decisions (this session)
1. **Scope** — author + you deploy. No `az deployment` against a real subscription from the agent.
2. **Subscription** — sandbox/personal (operator supplies sub id + region at deploy) → **dev Bicep
   uses public-access + firewall** networking. Prod params authored with private endpoints (not the
   dev exit).
3. **Worker Claude auth** — keep the `claude setup-token` (`CLAUDE_CODE_OAUTH_TOKEN`) from **Key
   Vault**; defer the commercial-API/ZDR switch.
4. **Onboarding** — build the GitHub-App installation-webhook repo-onboarding handler.

## Resolved research risks
- **Risk A (blob credential swap) — RESOLVED.** `azure_storage` 0.21 `StorageCredentials` exposes
  `token_credential(Arc<dyn TokenCredential>)` *and* `access_key(...)`. So the same 0.21 crate does
  shared-key (Azurite, local/CI) **and** Managed Identity (prod) — the swap is credential-only and
  Azurite parity is preserved. The MI `TokenCredential` source is `azure_identity` 0.21 **iff** it
  builds openssl-free under our `enable_reqwest_rustls` workspace; otherwise a ~50-line custom
  `TokenCredential` that calls the IMDS/workload-identity token endpoint over the existing rustls
  reqwest. Decided at build time — it is prod-only and behind an env switch, so CI/local never hit it.
- **Risk B (Postgres auth) — DECIDED.** dev = **password-in-Key-Vault** (`DATABASE_URL` injected as a
  KV-referenced Container Apps secret). Prod MI-Postgres (Entra token as rotating sqlx password via a
  connect callback) — Bicep provisions the Entra admin + MI db role; the sqlx token-auth code is an
  explicitly-noted **prod follow-up**, not part of the dev exit.

## Key simplification
Container Apps injects Key Vault secrets as **env vars via `secretRef`** (Bicep-side, using each
app's user-assigned MI with *Key Vault Secrets User*). So services need **no in-code Key Vault
client** — they keep reading `GITHUB_APP_*`, `GITHUB_WEBHOOK_SECRET`, `CLAUDE_CODE_OAUTH_TOKEN`,
`DATABASE_URL`, blob config from the environment exactly as today. "Key Vault secret loading" is an
**infra concern**, not Rust. This shrinks the Rust surface to: the Container Apps Jobs launcher, the
blob MI credential path, the OTel exporter, and the onboarding handler.

## Components (files, seam, telemetry, tests)

### 1. `infra/` — Bicep (IaC; CI gate = `az bicep build` / lint, not cargo)
- `infra/main.bicep` (subscription-scope: resource group + module wiring) + `modules/`:
  `postgres.bicep`, `storage.bicep` (LRS hot + 30-day lifecycle mgmt policy on the checkpoints/
  transcripts containers), `containerapps-env.bicep` (+ Log Analytics), `acr.bicep`,
  `keyvault.bicep`, `monitor.bicep`, `identity.bicep` (per-service user-assigned MIs +
  role assignments: Blob Data Contributor on its container, Key Vault Secrets User, AcrPull),
  `apps.bicep` (intake/tools/coordinator/dispatcher/dashboard-api as Container Apps services, N
  replicas), `worker-job.bicep` (Container Apps **Job**, manual/event trigger, scale-to-zero),
  `migrate-job.bicep` (one-shot migration Job).
- `infra/main.dev.bicepparam`, `infra/main.prod.bicepparam` — env params (dev: public access +
  firewall; prod: private endpoints). `namePrefix=factory` + `env` + `uniqueString()` for
  globally-unique names (ACR/KV/storage).
- **Test contract:** `az bicep build -f infra/main.bicep` compiles clean (add to CI as a lint job);
  `az deployment sub what-if` is operator-run (documented in the runbook). No cargo tests.

### 2. `ContainerAppsJobLauncher` — `WorkerLauncher` impl (`services/dispatcher`)
- New impl behind the existing `trait WorkerLauncher` (DockerLauncher stays for local). Starts a
  Container Apps **Job execution** with per-task env overrides (the same env `docker_env` builds:
  `FACTORY_TASK_TOKEN`, `FACTORY_INSTALL_TOKEN`, `FACTORY_ROLE`, `FACTORY_WORK_ITEM`, tools URL,
  repo URL, config sha, resume vars). Auth via the dispatcher's MI (`TokenCredential`). Uses the
  Container Apps Jobs **start** REST API (`POST .../jobs/{job}/start` with a template override) —
  prefer a thin REST call over pulling a heavy mgmt SDK crate (keeps the dep tree + openssl out;
  reuse the rustls reqwest already in the tree).
- **`main.rs` selection:** `WorkerRuntime::from_env()` already chooses Docker vs (new) ContainerApps
  by env (e.g. `FACTORY_WORKER_RUNTIME=container_apps` + job/resource-group/subscription env). Local
  stays Docker; Azure sets the env.
- **Telemetry:** info-log job-start (job name, execution id, role, work_item), error path on
  start failure.
- **Tests:** pure helpers unit-tested without Azure — the job-template **override builder** (env
  list → request JSON) is a pure fn with a unit test (mirrors `docker_env` tests); the REST call
  itself is a deploy seam (faked in tests via the trait, like DockerLauncher). No live Azure in CI.

### 3. Blob MI credential path — `libs/blob`
- `connect_container` gains a credential branch: if the connection string is present →
  `StorageCredentials::access_key` (today, Azurite/CI). If MI config is present (account name +
  `FACTORY_BLOB_USE_MANAGED_IDENTITY`/endpoint, no key) → `StorageCredentials::token_credential(cred)`
  where `cred` is the MI `TokenCredential`. The data path (`ensure_container`, get/put, append) is
  unchanged.
- **Telemetry:** info-log which credential mode at connect.
- **Tests:** `parse_*`/mode-selection is a pure fn (unit-tested: conn-string ⇒ shared-key,
  MI-env ⇒ token). The real MI token fetch is prod-only (no CI). Azurite round-trip tests unchanged.

### 4. OTel → Azure Monitor exporter — `libs/telemetry`
- `init` keeps its stable signature. When `APPLICATIONINSIGHTS_CONNECTION_STRING` (or
  `FACTORY_OTLP_ENDPOINT`) is set, add an OpenTelemetry layer (traces + metrics) exporting via OTLP
  to Azure Monitor, plus a `tracing-opentelemetry` bridge so `correlation_id` spans propagate;
  otherwise behave exactly as today (structured `tracing`). openssl-free (OTLP over tonic/rustls or
  the azure-monitor OTLP exporter on rustls — build-verified).
- **Telemetry:** the exporter init itself logs enabled/disabled + endpoint.
- **Tests:** `init` stays idempotent (existing test); a unit test asserts the no-env path is
  unchanged (structured tracing) and the enabled path constructs the layer without panic.

### 5. Onboarding webhook handler — `services/intake`
- Add `WebhookEvent::Installation` / `InstallationRepositories` parsing (action
  `created`/`added`/`removed`/`deleted` + the repos list + `installation.id`). On add → `record::
  upsert_repo(id, installation_id, owner, name, status)` (helper already exists). On remove/delete →
  mark the repo inactive (status). Idempotent on delivery id (existing dedupe).
- **Telemetry:** info-log onboarded/offboarded repo + installation_id; this closes the P7 "manual
  seed" gap.
- **Tests:** integration test (DATABASE_URL-gated, like existing intake tests) — an `installation
  created` delivery upserts the repos row with the right installation_id; `deleted` deactivates it;
  redelivery is idempotent. Pure parsing unit-tested.

### 6. CD — `.github/workflows/cd.yml`
- `workflow_dispatch` (manual; env input dev/prod). Steps: OIDC `azure/login` (federated creds — no
  stored secret; operator creates the Entra app + federated credential), `az acr build` (or docker
  build + push) for the services + worker images, run the **one-shot migration Job** (`az containerapp
  job start` on the migrate-job, wait for success), then `az containerapp update`/revision for each
  service + the worker job image. Idempotent; redeploy = prior revision (rollback drill documented).
- **Test contract:** `actionlint` (add to CI) + a documented dry-run; the live run is operator-only.

### 7. One-shot migration step
- A dedicated **migrate** Container Apps Job (same services image, command = a `--migrate` subcommand
  or a tiny `factory-migrate` bin that runs `record::migrate` once and exits). CD runs it to
  completion BEFORE rolling the service revisions, so N replicas never race on startup-migrate
  (sqlx migrate is advisory-locked + idempotent, but a single-runner step honors the spec's
  expand/contract discipline and keeps startup fast). Services keep their idempotent startup-migrate
  as a belt-and-suspenders no-op.
- **Tests:** the migrate entrypoint reuses the tested `record::migrate`; a smoke that the bin runs
  migrate and exits 0.

## Telemetry plan
Every new function/decision/error path gets info-level logging (AGENTS.md): launcher job-start +
failure, blob credential-mode selection, OTel enabled/disabled, onboarding upsert/deactivate,
migration run. The OTel exporter is itself the telemetry upgrade (traces/metrics → Azure Monitor).

## Build sequence / PR slicing
P8 is too large for one safe, reviewable PR. Proposed slices, each its own `/implement-e2e` run
(design → gauntlet → Copilot-gated ship), in dependency order:
- **P8.1 — infra/ Bicep + CD skeleton + migrate bin** (IaC + the one-shot migration entrypoint; CI
  adds `az bicep build` + `actionlint`). No behavior change to running services.
- **P8.2 — onboarding webhook handler** (intake; smallest Rust change, fully CI-testable).
- **P8.3 — blob MI credential path** (libs/blob; pure mode-selection tests).
- **P8.4 — ContainerAppsJobLauncher** (dispatcher; the largest seam).
- **P8.5 — OTel → Azure Monitor exporter** (libs/telemetry).
Each is independently mergeable and leaves the system green; the live Azure deploy (operator) lands
after the slices merge. (If you'd rather one big PR, say so — but the slices keep each Copilot gate
small and the blast radius contained.)

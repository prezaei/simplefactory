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
- **SINGLE resource group for the whole project** (operator decision, overrides the tech spec's
  "separate dev/prod RGs"): every Factory resource lives under ONE resource group so the project is
  one self-contained, easily-torn-down unit. `infra/main.bicep` deploys at **resource-group scope**
  (the operator creates/selects the RG and runs `az deployment group create -g <rg>`); per-env
  separation is by RG choice + params, not by multiple RGs in one deployment.
- `modules/`: `postgres.bicep`, `storage.bicep` (LRS hot + 30-day lifecycle mgmt policy on the
  checkpoints/transcripts containers), `containerapps-env.bicep` (+ Log Analytics), `acr.bicep`,
  `keyvault.bicep`, `monitor.bicep`, `identity.bicep` (per-service user-assigned MIs +
  role assignments: Blob Data Contributor on its container, Key Vault Secrets User, AcrPull),
  `apps.bicep` (intake/tools/coordinator/dispatcher/dashboard-api as Container Apps services, N
  replicas), `worker-job.bicep` (Container Apps **Job**, manual/event trigger, scale-to-zero),
  `migrate-job.bicep` (one-shot migration Job).
- `infra/main.dev.bicepparam`, `infra/main.prod.bicepparam` — env params (dev: public access +
  firewall; prod: private endpoints). `namePrefix=factory` + `env` + `uniqueString()` for
  globally-unique names (ACR/KV/storage). All names share the one RG.
- **Test contract:** `az bicep build -f infra/main.bicep` compiles clean (add to CI as a lint job);
  `az deployment sub what-if` is operator-run (documented in the runbook). No cargo tests.

### 2. `ContainerAppsJobLauncher` — `WorkerLauncher` impl (`services/dispatcher`)
- New impl behind the existing `trait WorkerLauncher` (DockerLauncher stays for local). Starts a
  Container Apps **Job execution** carrying the per-task env `docker_env` builds (`FACTORY_TASK_TOKEN`,
  `FACTORY_INSTALL_TOKEN`, `FACTORY_ROLE`, `FACTORY_WORK_ITEM`, tools URL, repo URL, config sha,
  resume vars). Auth via the dispatcher's MI (an ARM bearer token, `https://management.azure.com/`).
  Thin REST over the rustls reqwest already in the tree (no heavy mgmt SDK / openssl).
- **GET-merge-POST (start REPLACES the template).** Azure's `…/jobs/{job}/start` **replaces the job's
  entire template** for the execution — a partial override is silently dropped (verified against the
  Azure docs: "the job's entire template configuration is replaced … include all required settings";
  the REST body is the **bare template**, NOT wrapped in `template`). The dispatcher doesn't know the
  worker image/secrets, so it mirrors `az containerapp job start --yaml`: **GET** the job's current
  `properties.template`, **merge** the per-task env onto the `worker` container (override-by-name,
  preserving image/resources + the base env incl. the KV-backed `CLAUDE_CODE_OAUTH_TOKEN` secretRef
  + blob connection), **project** to the `JobExecutionTemplate` shape `/start` accepts (drop the
  `volumes`/`probes`/`volumeMounts` the GET'd `JobTemplate` carries but `/start` rejects), then
  **POST** it. So blob/claude come from the live template (hence `WorkerRuntime::default()`).
- **`main.rs` selection:** `FACTORY_WORKER_RUNTIME` chooses Docker (default, local) vs `container_apps`
  (Azure, needs `FACTORY_AZURE_SUBSCRIPTION_ID` / `_RESOURCE_GROUP` / `FACTORY_WORKER_JOB_NAME`,
  optional `FACTORY_MANAGED_IDENTITY_CLIENT_ID`).
- **Telemetry:** info-log job-start (job name, execution id, role, work_item); error paths on GET,
  template-missing, container-missing, and start failure; warn on an unreadable start-response body.
- **Tests:** pure helpers unit-tested without Azure — `job_get_url` / `job_start_url`, the env
  **merge** (preserves image/resources + base secretRefs, overrides same-named once, appends the
  rest), the container-absent error, and `parse_execution_name`. The HTTP calls are the deploy seam
  (faked via the trait, like DockerLauncher). No live Azure in CI.

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
- `workflow_dispatch` (manual; env input dev/prod, image_tag input defaulting to the commit SHA).
  OIDC `azure/login` (federated creds — no stored secret; operator creates the Entra app + federated
  credential per the runbook). **Two-phase deploy** keyed off the Bicep `deployWorkloads` flag, which
  breaks the ACR/image bootstrap cycle (an app can't be created before its image exists, but the ACR
  that holds the image is in the same template): **phase 1** deploys infra + an empty ACR
  (`deployWorkloads=false`); **`az acr build`** builds + pushes the services + worker images;
  **phase 2** deploys the workloads (`deployWorkloads=true`). Then the **one-shot migration Job**
  (`az containerapp job start` + poll to `Succeeded`). Idempotent; redeploy of a prior tag rolls back.
- **Test contract:** `actionlint` (in CI) + `az bicep build`/`build-params` (in CI, warnings = errors)
  + the documented runbook (`docs/runbooks/azure-deploy.md`); the live run is operator-only.

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
failure, blob credential-mode selection, runtime selection (docker vs container_apps), onboarding
upsert/deactivate (incl. uninstall offboard-all), migration run. **Logging transport (operator
decision "Logs→Log Analytics now, OTel later"):** the Bicep wires the Container Apps environment's
stdout/stderr → Log Analytics, so the structured `tracing` JSON is KQL-queryable now; the in-process
OTel traces/metrics exporter is a deferred upgrade (no `opentelemetry` deps added this phase).

## Build sequence (actual cadence)
Delivered as **two PRs** (operator chose to bank the foundational Rust first, then the heavier
deploy seams), each a full `/implement-e2e` run (gauntlet → Copilot-gated ship):
- **Part 1 (this PR) — foundational, fully CI-testable Rust:**
  - onboarding webhook handler (intake) — closes the manual-seed gap;
  - one-shot `factory-migrate` bin;
  - blob Managed-Identity credential path (libs/blob), openssl-free.
  No behavior change to running services; all green on a clean DB.
- **Part 2 (this PR) — deploy seams + infra:**
  - `ContainerAppsJobLauncher` (dispatcher) over the shared `factory-azure-auth` MI crate; runtime
    selected by `FACTORY_WORKER_RUNTIME` (`docker` default / `container_apps`);
  - `infra/` Bicep (**single resource group**): Postgres Flexible Server, Storage (LRS + 30d
    lifecycle), Container Apps env wired → Log Analytics, ACR, Key Vault + secrets, per-service
    user-assigned MIs + least-priv roles, the 5 service apps (intake/dashboard external, tools
    internal, coordinator/dispatcher no-ingress), worker + migrate Jobs; a **two-phase
    `deployWorkloads` flag** breaks the ACR/image bootstrap cycle;
  - CI `az bicep build` + `build-params` (warnings = errors) + `actionlint`;
  - GitHub Actions CD (OIDC, two-phase deploy + `az acr build` + migrate-Job gate) + the operator
    runbook (`docs/runbooks/azure-deploy.md`);
  - carry-over **PR #24 round-3 review fixes**: uninstall/suspend offboards ALL repos for the
    installation (`remove_all` → `deactivate_repos_by_installation`); MI token cache keyed by resource.
  - **OTel exporter DEFERRED** (operator decision): Log Analytics gives KQL-queryable structured logs
    now; the in-process traces/metrics exporter is a later upgrade. No `opentelemetry` deps added.
The live Azure deploy (operator) lands after part 2 merges.

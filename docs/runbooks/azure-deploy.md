# Azure deploy runbook (P8)

Operator guide for deploying Factory to Azure. The infra (`infra/`) + CD (`.github/workflows/cd.yml`)
are **authored and CI-validated** in the repo; the live `az` / CD steps below are **operator-run**
(they need a real subscription + secrets — the agent never runs them).

Everything lands in **one resource group** (a single self-contained, easily-torn-down unit).

---

## What gets deployed (single RG)

| Resource | Purpose |
|----------|---------|
| Log Analytics workspace | Container stdout/stderr sink (KQL-queryable). OTel exporter deferred. |
| Azure Container Registry | `factory-services` + `factory-worker` images. |
| Key Vault (RBAC) | App id/PEM, webhook secret, Claude token, `database-url`. |
| Storage (Blob, LRS, 30-day lifecycle) | `checkpoints` + `transcripts` containers. MI-only (no shared key). |
| Postgres Flexible Server (Burstable B1ms) | The record book. Dev: public access + firewall. |
| Container Apps environment | Hosts the services + jobs; logs → Log Analytics. |
| 5 service Container Apps | intake (external), tools (internal), coordinator + dispatcher (no ingress), dashboard-api (external). |
| Worker Container Apps **Job** | One execution per task, started by the dispatcher. |
| Migrate Container Apps **Job** | One-shot schema migration, run by CD. |
| Per-service user-assigned Managed Identities | AcrPull + Key Vault Secrets User; tools + worker also Blob Data Contributor; dispatcher can start the worker Job. |

---

## One-time setup

### 1. Pick the basics
- **Subscription**: a sandbox/personal sub. `az account set -s <SUB_ID>`.
- **Region**: e.g. `eastus`.
- **Resource group name**: e.g. `factory-dev`.

### 2. Create the OIDC app registration (CD auth, no client secret)
```bash
az ad app create --display-name factory-cd
APP_ID=$(az ad app list --display-name factory-cd --query '[0].appId' -o tsv)
az ad sp create --id "$APP_ID"
# Federated credential for this repo's CD workflow (environment: dev):
az ad app federated-credential create --id "$APP_ID" --parameters '{
  "name": "factory-cd-dev",
  "issuer": "https://token.actions.githubusercontent.com",
  "subject": "repo:prezaei/simplefactory:environment:dev",
  "audiences": ["api://AzureADTokenExchange"]
}'
# Grant the SP rights on the subscription (Contributor + User Access Administrator so it can
# create the role assignments in the template):
SP_OID=$(az ad sp show --id "$APP_ID" --query id -o tsv)
az role assignment create --assignee-object-id "$SP_OID" --assignee-principal-type ServicePrincipal \
  --role Contributor --scope /subscriptions/<SUB_ID>
az role assignment create --assignee-object-id "$SP_OID" --assignee-principal-type ServicePrincipal \
  --role "User Access Administrator" --scope /subscriptions/<SUB_ID>
# REQUIRED: the template WRITES Key Vault secrets, and the vault uses RBAC
# (enableRbacAuthorization). Writing a secret is a DATA-PLANE op that Contributor / Owner do
# NOT grant — without this the very first deploy fails on the secret writes. Grant it at
# SUBSCRIPTION scope (the vault doesn't exist yet; the grant is inherited when it's created,
# and propagates well before any deploy — no in-deployment RBAC race):
az role assignment create --assignee-object-id "$SP_OID" --assignee-principal-type ServicePrincipal \
  --role "Key Vault Secrets Officer" --scope /subscriptions/<SUB_ID>
```

> **Manual (non-CD) deploy:** the operator's OWN identity needs the same `Key Vault Secrets
> Officer` grant (Owner/Contributor are control-plane only under RBAC). Grant it to your user:
> `az role assignment create --assignee <your-upn> --role "Key Vault Secrets Officer" --scope /subscriptions/<SUB_ID>`.

### 3. Set GitHub repo secrets + vars
**Secrets** (`gh secret set <NAME> --repo prezaei/simplefactory`):

| Secret | Value |
|--------|-------|
| `AZURE_CLIENT_ID` | `$APP_ID` above |
| `AZURE_TENANT_ID` | `az account show --query tenantId -o tsv` |
| `AZURE_SUBSCRIPTION_ID` | the sub id |
| `FACTORY_GITHUB_APP_ID` | the Factory GitHub App's id |
| `FACTORY_GITHUB_APP_PRIVATE_KEY` | the App's PEM (full contents) |
| `FACTORY_GITHUB_WEBHOOK_SECRET` | the App's webhook secret |
| `FACTORY_CLAUDE_OAUTH_TOKEN` | `claude setup-token` output |
| `FACTORY_PG_ADMIN_PASSWORD` | a strong generated password |

**Variables** (`gh variable set <NAME> --repo prezaei/simplefactory`):

| Var | Value |
|-----|-------|
| `FACTORY_RESOURCE_GROUP` | e.g. `factory-dev` |
| `FACTORY_REGION` | e.g. `eastus` |

Also create a GitHub **Environment** named `dev` (Settings → Environments) so the federated
credential subject (`...:environment:dev`) matches.

---

## Deploy (CD — the normal path)

`Actions → CD (Azure) → Run workflow` → pick `environment: dev`, leave `image_tag` blank
(defaults to the commit SHA). The workflow:

1. Logs in via OIDC.
2. `az group create` (idempotent).
3. **Phase 1** — deploys infra + an empty ACR (`deployWorkloads=false`).
4. **`az acr build`** — builds + pushes `factory-services` and `factory-worker` to the ACR.
5. **Phase 2** — deploys the workloads (apps + jobs) now that the images exist.
6. Starts the **migrate Job** and waits for `Succeeded`.
7. Prints the intake webhook URL to the job summary.

The two-phase split exists because an app can't be created before its image exists, but the ACR
that holds the image is created by the same template.

---

## Deploy (manual `az`, no CD)

```bash
export FACTORY_GITHUB_APP_ID=... FACTORY_GITHUB_APP_PRIVATE_KEY="$(cat app.pem)" \
       FACTORY_GITHUB_WEBHOOK_SECRET=... FACTORY_CLAUDE_OAUTH_TOKEN=... \
       FACTORY_PG_ADMIN_PASSWORD=... FACTORY_IMAGE_TAG=$(git rev-parse --short=12 HEAD)
RG=factory-dev; REGION=eastus
az group create -n $RG -l $REGION

# Phase 1: infra + ACR
ACR=$(az deployment group create -g $RG -n factory-infra \
  -f infra/main.bicep -p infra/main.dev.bicepparam -p deployWorkloads=false \
  --query 'properties.outputs.acrName.value' -o tsv)

# Build + push images
az acr build --registry $ACR --image factory-services:$FACTORY_IMAGE_TAG --target services -f Dockerfile .
az acr build --registry $ACR --image factory-worker:$FACTORY_IMAGE_TAG   --target worker   -f Dockerfile .

# Phase 2: workloads
az deployment group create -g $RG -n factory-workloads \
  -f infra/main.bicep -p infra/main.dev.bicepparam -p deployWorkloads=true

# Run migrations
az containerapp job start -g $RG -n factory-migrate-dev
```

---

## Point the GitHub App at the deployment

1. Get the intake FQDN: `az containerapp show -g $RG -n intake --query properties.configuration.ingress.fqdn -o tsv`.
2. In the GitHub App settings, set the **Webhook URL** to `https://<intake-fqdn>/webhooks/github`
   and the **webhook secret** to `FACTORY_GITHUB_WEBHOOK_SECRET`.
3. Install the App on `prezaei/factory-smoke` (or your test repo). The `installation` webhook
   auto-onboards the repo (no manual DB seeding).

---

## Verify

```bash
# Logs (KQL) — last 30 min of service stdout:
az monitor log-analytics query -w <workspace-id> \
  --analytics-query "ContainerAppConsoleLogs_CL | where TimeGenerated > ago(30m) | order by TimeGenerated desc | take 100"

# A worker Job execution per dispatched task:
az containerapp job execution list -g $RG --name factory-worker-dev -o table
```

E2E: label an issue in the test repo → intake enqueues → dispatcher starts a worker Job →
PR opened. Watch the dispatcher logs for `dispatcher: Azure Container Apps Jobs runtime`.

---

## Prod hardening (follow-up — NOT in the dev exit)

`infra/main.prod.bicepparam` exists, but prod networking is intentionally not yet authored:
- Postgres `publicNetworkAccess: 'Disabled'` behind a private endpoint + private DNS zone.
- (tools is already internal-only.)
- ACR `Premium` + private endpoint; deny public network.
- Tighten the dispatcher's worker-Job-start role from `Contributor` (scoped to the Job) to a
  custom role granting only `Microsoft.App/jobs/start/action`.

---

## Teardown

`az group delete -n $RG --yes --no-wait` removes everything (single RG). Key Vault soft-delete
retains secrets for the purge-protection window — `az keyvault purge -n <kv-name>` to fully remove.

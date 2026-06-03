#!/usr/bin/env bash
# Factory Azure deploy — idempotent + REPEATABLE. Every run starts from a clean slate:
# `teardown` (and `redeploy`'s first step) deletes the resource group AND purges this RG's
# soft-deleted Key Vault (which otherwise blocks recreating a same-named vault), so the same RG
# name always redeploys cleanly. A deploy that fails partway is LEFT in place for diagnosis (it
# does NOT auto-teardown — that would destroy a good/partial deploy + its logs on a transient
# blip); re-run `redeploy` to retry from a clean slate.
#
# Two-phase (breaks the ACR/image bootstrap cycle): phase-1 infra + ACR → az acr build →
# phase-2 workloads → migration job → structural verify.
#
# Usage:
#   FACTORY_RG=factory FACTORY_REGION=eastus2 ./scripts/azure-deploy.sh            # redeploy (teardown+deploy) — default
#   FACTORY_RG=factory FACTORY_REGION=eastus2 ./scripts/azure-deploy.sh deploy     # deploy only (clean RG assumed)
#   FACTORY_RG=factory                         ./scripts/azure-deploy.sh teardown  # delete RG + purge soft-deleted KVs
#
# Env: FACTORY_RG (req); FACTORY_REGION (req for deploy); FACTORY_ENV=dev; FACTORY_NAME_PREFIX=factory;
#      FACTORY_SECRETS_FILE=~/.factory-deploy.env (sourced — the 5 FACTORY_* secrets);
#      FACTORY_IMAGE_TAG (default: git short sha); AZURE_CONFIG_DIR (default ~/.azure-factory).
set -euo pipefail

RG="${FACTORY_RG:?set FACTORY_RG=<resource-group>}"
ENVN="${FACTORY_ENV:-dev}"
PREFIX="${FACTORY_NAME_PREFIX:-factory}"
SECRETS="${FACTORY_SECRETS_FILE:-$HOME/.factory-deploy.env}"
export AZURE_CONFIG_DIR="${AZURE_CONFIG_DIR:-$HOME/.azure-factory}"
MODE="${1:-redeploy}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

log() { printf '\n\033[1m[%s]\033[0m %s\n' "$(date +%H:%M:%S)" "$*"; }

teardown() {
  if [ "$(az group exists -n "$RG")" = "true" ]; then
    log "deleting resource group $RG (waiting for completion)…"
    az group delete -n "$RG" --yes
  else
    log "resource group $RG does not exist — nothing to delete"
  fi
  # Purge the soft-deleted Key Vault that belonged to THIS resource group (a soft-deleted vault
  # keeps its globally-unique name reserved and blocks recreating it). Scope strictly to this RG
  # via the deleted vault's original resourceId (`properties.vaultId`) — NEVER a sub-wide name
  # prefix, which would irreversibly `purge` other environments'/projects' ${PREFIX}-kv* vaults.
  for v in $(az keyvault list-deleted --query "[?contains(properties.vaultId, '/resourceGroups/${RG}/')].name" -o tsv 2>/dev/null); do
    log "purging soft-deleted Key Vault $v (was in $RG)"
    az keyvault purge -n "$v" >/dev/null 2>&1 || true
  done
}

deploy() {
  local region="${FACTORY_REGION:?set FACTORY_REGION=<azure-region> for deploy}"
  set -a
  # shellcheck disable=SC1090
  . "$SECRETS"
  set +a
  : "${FACTORY_GITHUB_APP_ID:?secrets file is missing FACTORY_GITHUB_APP_ID}"
  : "${FACTORY_GITHUB_APP_PRIVATE_KEY:?secrets file is missing FACTORY_GITHUB_APP_PRIVATE_KEY}"
  : "${FACTORY_GITHUB_WEBHOOK_SECRET:?secrets file is missing FACTORY_GITHUB_WEBHOOK_SECRET}"
  : "${FACTORY_CLAUDE_OAUTH_TOKEN:?secrets file is missing FACTORY_CLAUDE_OAUTH_TOKEN}"
  : "${FACTORY_PG_ADMIN_PASSWORD:?secrets file is missing FACTORY_PG_ADMIN_PASSWORD}"
  export FACTORY_IMAGE_TAG="${FACTORY_IMAGE_TAG:-$(git -C "$ROOT" rev-parse --short=12 HEAD)}"
  export FACTORY_DEV_CLIENT_IP="${FACTORY_DEV_CLIENT_IP:-}"

  log "deploying RG=$RG region=$region env=$ENVN tag=$FACTORY_IMAGE_TAG"
  az group create -n "$RG" -l "$region" -o none

  log "phase 1 — infra + ACR (deployWorkloads=false)"
  local acr
  acr=$(az deployment group create -g "$RG" -n "${PREFIX}-infra-${ENVN}" \
    -f "$ROOT/infra/main.bicep" -p "$ROOT/infra/main.${ENVN}.bicepparam" \
    -p deployWorkloads=false --query properties.outputs.acrName.value -o tsv)
  log "ACR = $acr"

  log "building + pushing images (az acr build, from a clean source export)"
  # Build context = a `git archive` of HEAD (tracked files only). `az acr build` walks the WHOLE
  # context directory to build the upload tar even though .dockerignore excludes target/ from the
  # image — and this repo's target/ can be tens of GB (Rust artifacts), which makes packing the
  # live worktree pathologically slow (minutes at full CPU before any upload). A git-archive
  # export is only the tracked source → instant pack + tiny upload. (It also can't leak an
  # untracked .env.live, which isn't in git.)
  local ctx
  ctx="$(mktemp -d)"
  git -C "$ROOT" archive HEAD | tar -x -C "$ctx"
  az acr build --registry "$acr" --image "factory-services:$FACTORY_IMAGE_TAG" --target services -f "$ctx/Dockerfile" "$ctx"
  az acr build --registry "$acr" --image "factory-worker:$FACTORY_IMAGE_TAG"   --target worker   -f "$ctx/Dockerfile" "$ctx"
  rm -rf "$ctx"

  log "phase 2 — workloads (deployWorkloads=true)"
  az deployment group create -g "$RG" -n "${PREFIX}-workloads-${ENVN}" \
    -f "$ROOT/infra/main.bicep" -p "$ROOT/infra/main.${ENVN}.bicepparam" \
    -p deployWorkloads=true -o none

  log "running one-shot migration job"
  local job exec_name status
  job="${PREFIX}-migrate-${ENVN}"
  exec_name=$(az containerapp job start -g "$RG" -n "$job" --query name -o tsv)
  [ -n "$exec_name" ] || { echo "migration job did not return an execution name"; return 1; }
  status=""
  for _ in $(seq 1 60); do
    status=$(az containerapp job execution show -g "$RG" --name "$job" \
      --job-execution-name "$exec_name" --query properties.status -o tsv 2>/dev/null || echo "")
    log "  migration: ${status:-<pending>}"
    case "$status" in
      Succeeded) break ;;
      Failed|Cancelled|Degraded) echo "migration ended in status: $status"; return 1 ;;
    esac
    sleep 10
  done
  [ "$status" = "Succeeded" ] || { echo "migration did not succeed (last: ${status:-none})"; return 1; }

  log "structural verification"
  FACTORY_RG="$RG" FACTORY_ENV="$ENVN" FACTORY_IMAGE_TAG="$FACTORY_IMAGE_TAG" "$ROOT/scripts/verify-deploy.sh"

  local fqdn
  fqdn=$(az containerapp show -g "$RG" -n intake --query properties.configuration.ingress.fqdn -o tsv 2>/dev/null || echo "")
  log "DEPLOY COMPLETE. Point the GitHub App webhook at: https://${fqdn}/webhooks/github"
}

# On failure, LEAVE resources in place for diagnosis — do NOT auto-teardown. Auto-deleting on any
# transient error (an acr-build flake, a quota/region blip, a verify race, or a poll false-negative)
# would destroy a good or partially-good deploy AND its diagnostic logs. Repeatability is preserved
# via `redeploy`, which tears down + redeploys cleanly on the NEXT explicit run.
on_fail() {
  local rc=$?
  trap - ERR
  log "deploy FAILED (rc=$rc) — leaving $RG in place for diagnosis (NOT tearing down)"
  echo "  inspect:     az containerapp logs show -g $RG -n intake --type console   (or: dispatcher)"
  echo "  retry clean: FACTORY_RG=$RG FACTORY_REGION=${FACTORY_REGION:-<region>} $0 redeploy"
  echo "  tear down:   FACTORY_RG=$RG $0 teardown"
  exit "$rc"
}

case "$MODE" in
  teardown)
    teardown
    ;;
  deploy | redeploy)
    [ "$MODE" = redeploy ] && teardown
    trap on_fail ERR
    deploy
    trap - ERR
    log "done."
    ;;
  *)
    echo "usage: $0 [redeploy|deploy|teardown]" >&2
    exit 2
    ;;
esac

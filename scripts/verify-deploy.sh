#!/usr/bin/env bash
# Factory Azure post-deploy verification (P8). After `az deployment`/CD runs (see
# docs/runbooks/azure-deploy.md), this asserts the stack came up CORRECTLY — every
# resource exists, every service revision is actually Running (not crash-looping), the
# migration ran, secrets resolve, and the least-privilege roles are in place. It is the
# Layer-2 "does it boot" gate: it catches the deploy-time failure classes that compile-clean
# CI cannot (a bad container `command`, an unresolvable Key Vault secret, an image-pull RBAC
# race, a missing Postgres firewall rule). It does NOT prove the functional loop — that is the
# Layer-3 E2E (label an issue → PR), which this script prints the next steps for.
#
# READ-ONLY: every call is `az ... show/list`. Nothing is created, changed, or deleted.
#
# Usage:
#   FACTORY_RG=factory-dev ./scripts/verify-deploy.sh                 # env defaults to dev
#   FACTORY_RG=factory-dev FACTORY_ENV=dev FACTORY_IMAGE_TAG=abc123 ./scripts/verify-deploy.sh
#
# Requires: `az` logged in to the deployment's subscription, and read access to the RG +
# (for the secret/RBAC checks) Key Vault data-plane (Secrets Officer/User) and role-assignment
# read (User Access Administrator) — the same identity that deployed has these.
set -uo pipefail

RG="${FACTORY_RG:?set FACTORY_RG=<resource-group> (the single RG the stack deployed into)}"
ENVN="${FACTORY_ENV:-dev}"
PREFIX="${FACTORY_NAME_PREFIX:-factory}"
TAG="${FACTORY_IMAGE_TAG:-}"   # optional: assert this exact image tag is present in ACR

PASS=0; FAIL=0; WARN=0
ok()   { printf '  \033[32m✓\033[0m %s\n' "$1"; PASS=$((PASS+1)); }
no()   { printf '  \033[31m✗\033[0m %s\n' "$1"; FAIL=$((FAIL+1)); }
warn() { printf '  \033[33m!\033[0m %s\n' "$1"; WARN=$((WARN+1)); }
section() { printf '\n\033[1m== %s ==\033[0m\n' "$1"; }
# pass_if "ok-msg" "fail-msg" <command...> — runs the command; ok() on success, no() otherwise.
# Taking the command (vs. inspecting $?) keeps this shellcheck-clean and unambiguous.
pass_if() { local okmsg="$1" failmsg="$2"; shift 2; if "$@"; then ok "$okmsg"; else no "$failmsg"; fi; }

# az <args...> -> capture stdout, suppress stderr; returns az's exit code.
q() { az "$@" 2>/dev/null; }
# Silent variant for existence checks (suppress stdout too, so JSON doesn't leak when used
# inside pass_if, which doesn't capture the command's output).
# shellcheck disable=SC2329  # invoked indirectly as the command passed to pass_if "$@"
qq() { az "$@" >/dev/null 2>&1; }

SERVICES=(intake tools coordinator dispatcher dashboard-api)
WORKER_JOB="${PREFIX}-worker-${ENVN}"
MIGRATE_JOB="${PREFIX}-migrate-${ENVN}"
EXPECTED_SECRETS=(database-url github-app-id github-app-private-key github-webhook-secret claude-oauth-token)

printf '\033[1mFactory deploy verification\033[0m  RG=%s  env=%s\n' "$RG" "$ENVN"

# ---------------------------------------------------------------------------
section "Resource group"
if q group show -n "$RG" >/dev/null; then ok "resource group $RG exists"; else
  no "resource group $RG not found — nothing to verify"; exit 1
fi

# ---------------------------------------------------------------------------
section "Core infrastructure (discovered by type in the RG)"
ACR=$(q acr list -g "$RG" --query "[0].name" -o tsv)
KV=$(q keyvault list -g "$RG" --query "[0].name" -o tsv)
PG=$(q postgres flexible-server list -g "$RG" --query "[0].name" -o tsv)
SA=$(q storage account list -g "$RG" --query "[0].name" -o tsv)
LA=$(q monitor log-analytics workspace list -g "$RG" --query "[0].name" -o tsv)
CAE=$(q containerapp env list -g "$RG" --query "[0].name" -o tsv)
pass_if "ACR: $ACR" "ACR not found" test -n "$ACR"
pass_if "Key Vault: $KV" "Key Vault not found" test -n "$KV"
pass_if "Postgres: $PG" "Postgres flexible server not found" test -n "$PG"
pass_if "Storage: $SA" "Storage account not found" test -n "$SA"
pass_if "Log Analytics: $LA" "Log Analytics workspace not found" test -n "$LA"
pass_if "Container Apps env: $CAE" "Container Apps managed environment not found" test -n "$CAE"

# ---------------------------------------------------------------------------
section "ACR images"
if [ -n "$ACR" ]; then
  REPOS=$(q acr repository list -n "$ACR" -o tsv)
  for repo in factory-services factory-worker; do
    if ! printf '%s\n' "$REPOS" | grep -qx "$repo"; then
      no "$repo image repository missing from ACR (services/jobs can't pull)"
    elif [ -n "$TAG" ]; then
      pass_if "$repo:$TAG present" "$repo is missing tag $TAG" \
        grep -qx "$TAG" <<<"$(q acr repository show-tags -n "$ACR" --repository "$repo" -o tsv)"
    else
      ok "$repo present (no tag asserted; set FACTORY_IMAGE_TAG to check one)"
    fi
  done
fi

# ---------------------------------------------------------------------------
section "Key Vault secrets"
if [ -n "$KV" ]; then
  NAMES=$(q keyvault secret list --vault-name "$KV" --query "[].name" -o tsv)
  if [ -z "$NAMES" ]; then
    warn "could not list secrets (need Key Vault data-plane read: Secrets Officer/User)"
  else
    for s in "${EXPECTED_SECRETS[@]}"; do
      pass_if "secret $s present" "secret $s missing" grep -qx "$s" <<<"$NAMES"
    done
  fi
fi

# ---------------------------------------------------------------------------
section "Postgres"
if [ -n "$PG" ]; then
  STATE=$(q postgres flexible-server show -g "$RG" -n "$PG" --query state -o tsv)
  pass_if "server state: Ready" "server state: ${STATE:-unknown} (expected Ready)" test "$STATE" = "Ready"
  FW=$(q postgres flexible-server firewall-rule list -g "$RG" -n "$PG" --query "[?name=='AllowAzureServices'].name" -o tsv)
  pass_if "AllowAzureServices firewall rule present (Container Apps can reach the DB)" \
    "AllowAzureServices firewall rule MISSING — Container Apps cannot connect to Postgres" test -n "$FW"
  DB=$(q postgres flexible-server db list -g "$RG" -s "$PG" --query "[?name=='factory'].name" -o tsv)
  pass_if "database 'factory' exists" "database 'factory' missing" test -n "$DB"
fi

# ---------------------------------------------------------------------------
section "Storage (containers + lifecycle)"
if [ -n "$SA" ]; then
  SAID=$(q storage account show -g "$RG" -n "$SA" --query id -o tsv)
  for c in checkpoints transcripts; do
    # Control-plane check (ARM sub-resource) — avoids needing Blob data-plane permission.
    pass_if "container $c exists" "container $c missing" \
      qq resource show --ids "${SAID}/blobServices/default/containers/${c}"
  done
  LC=$(q storage account management-policy show -g "$RG" --account-name "$SA" --query "policy.rules[0].name" -o tsv)
  if [ -n "$LC" ]; then ok "lifecycle policy present ($LC)"; else warn "no blob lifecycle policy (30-day expiry not set)"; fi
fi

# ---------------------------------------------------------------------------
section "Service Container Apps (the crash-loop gate)"
for svc in "${SERVICES[@]}"; do
  if ! q containerapp show -g "$RG" -n "$svc" >/dev/null; then
    no "$svc: app not found"; continue
  fi
  PROV=$(q containerapp show -g "$RG" -n "$svc" --query "properties.provisioningState" -o tsv)
  pass_if "$svc: provisioned ($PROV)" "$svc: provisioningState=$PROV" test "$PROV" = "Succeeded"
  # Active revision running state — "Running" means it actually booted (not crash-looping on a
  # bad command / unresolved secret / failed image pull).
  RUN=$(q containerapp revision list -g "$RG" -n "$svc" --query "[?properties.active]|[0].properties.runningState" -o tsv)
  case "$RUN" in
    Running)            ok "$svc: active revision Running" ;;
    Processing|"")      warn "$svc: active revision state '${RUN:-none}' (still starting? re-run shortly)" ;;
    *)                  no "$svc: active revision state '$RUN' (NOT running — check: az containerapp logs show -g $RG -n $svc)" ;;
  esac
  # The shared services image has no ENTRYPOINT, so each app MUST set its own command.
  CMD=$(q containerapp show -g "$RG" -n "$svc" --query "properties.template.containers[0].command | [0]" -o tsv)
  case "$CMD" in
    *"factory-$svc") ok "$svc: command set ($CMD)" ;;
    "")              no "$svc: NO command set — container would run the base image default and exit" ;;
    *)               warn "$svc: unexpected command '$CMD' (expected …/factory-$svc)" ;;
  esac
done

# Dispatcher must be wired for the Azure runtime so it starts worker Jobs (not docker).
section "Dispatcher → Container Apps Jobs wiring"
if q containerapp show -g "$RG" -n dispatcher >/dev/null; then
  RT=$(q containerapp show -g "$RG" -n dispatcher --query "properties.template.containers[0].env[?name=='FACTORY_WORKER_RUNTIME'].value | [0]" -o tsv)
  pass_if "FACTORY_WORKER_RUNTIME=container_apps" "FACTORY_WORKER_RUNTIME='${RT:-unset}' (expected container_apps)" test "$RT" = "container_apps"
  JN=$(q containerapp show -g "$RG" -n dispatcher --query "properties.template.containers[0].env[?name=='FACTORY_WORKER_JOB_NAME'].value | [0]" -o tsv)
  pass_if "FACTORY_WORKER_JOB_NAME=$JN" "FACTORY_WORKER_JOB_NAME='${JN:-unset}' (expected $WORKER_JOB)" test "$JN" = "$WORKER_JOB"
fi

# ---------------------------------------------------------------------------
section "Jobs (worker + migrate)"
pass_if "worker job $WORKER_JOB exists" "worker job $WORKER_JOB missing" \
  qq containerapp job show -g "$RG" -n "$WORKER_JOB"
if q containerapp job show -g "$RG" -n "$MIGRATE_JOB" >/dev/null; then
  ok "migrate job $MIGRATE_JOB exists"
  MSTAT=$(q containerapp job execution list -g "$RG" --name "$MIGRATE_JOB" --query "[0].properties.status" -o tsv)
  case "$MSTAT" in
    Succeeded) ok "migrate job last execution: Succeeded" ;;
    "")        warn "migrate job has not run yet (CD runs it; or: az containerapp job start -g $RG -n $MIGRATE_JOB)" ;;
    *)         no "migrate job last execution: $MSTAT (expected Succeeded)" ;;
  esac
else
  no "migrate job $MIGRATE_JOB missing"
fi

# ---------------------------------------------------------------------------
section "Least-privilege role assignments (per Managed Identity)"
# check_roles <label> <identity-name> <expected role names...>
check_roles() {
  local label="$1" mi="$2"; shift 2
  local pid roles missing=""
  pid=$(q identity show -g "$RG" -n "$mi" --query principalId -o tsv)
  if [ -z "$pid" ]; then no "$label: managed identity $mi not found"; return; fi
  roles=$(q role assignment list --assignee "$pid" --all --query "[].roleDefinitionName" -o tsv)
  for want in "$@"; do
    grep -qx "$want" <<<"$roles" || missing="$missing $want"
  done
  if [ -z "$missing" ]; then ok "$label: $*"; else no "$label: MISSING role(s):$missing"; fi
}
for svc in intake coordinator dashboard-api; do
  check_roles "$svc MI" "${PREFIX}-${svc}-${ENVN}-id" AcrPull "Key Vault Secrets User"
done
check_roles "tools MI"      "${PREFIX}-tools-${ENVN}-id"      AcrPull "Key Vault Secrets User" "Storage Blob Data Contributor"
check_roles "dispatcher MI" "${PREFIX}-dispatcher-${ENVN}-id" AcrPull "Key Vault Secrets User" "Contributor"
check_roles "worker MI"     "${PREFIX}-worker-${ENVN}-id"     AcrPull "Key Vault Secrets User" "Storage Blob Data Contributor"

# ---------------------------------------------------------------------------
section "Next: functional E2E (Layer 3 — the real proof)"
INTAKE_FQDN=$(q containerapp show -g "$RG" -n intake --query "properties.configuration.ingress.fqdn" -o tsv)
if [ -n "$INTAKE_FQDN" ]; then
  echo "  • Point the GitHub App webhook URL at: https://$INTAKE_FQDN/webhooks/github"
else
  warn "intake has no external ingress FQDN yet"
fi
echo "  • Install the App on a throwaway repo, label an issue, and watch the loop run via KQL:"
echo "      az monitor log-analytics query -w <workspace-customer-id> \\"
echo "        --analytics-query \"ContainerAppConsoleLogs_CL | where TimeGenerated > ago(15m) | order by TimeGenerated desc | take 100\""
echo "  • Expect: intake enqueues → dispatcher 'Azure Container Apps Jobs runtime' → a worker"
echo "    job execution (az containerapp job execution list -g $RG --name $WORKER_JOB -o table) → a PR."

# ---------------------------------------------------------------------------
printf '\n\033[1m== Summary ==\033[0m\n'
printf '  passed: %d   failed: %d   warnings: %d\n' "$PASS" "$FAIL" "$WARN"
if [ "$FAIL" -gt 0 ]; then
  printf '\033[31mDeploy verification FAILED — fix the ✗ items above before the functional E2E.\033[0m\n'
  exit 1
fi
printf '\033[32mStructural checks passed.\033[0m Proceed to the functional E2E above.\n'
[ "$WARN" -gt 0 ] && printf '(%d warning(s) — usually "still starting" or "not run yet"; re-run shortly.)\n' "$WARN"
exit 0

// Factory — Azure productionization (P8). Deploys the WHOLE project into a SINGLE resource
// group (operator decision, overriding the spec's separate-RG sketch): one self-contained,
// easily-torn-down unit. Deploy at resource-group scope:
//   az group create -n <rg> -l <region>
//   az deployment group create -g <rg> -f infra/main.bicep -p @infra/main.dev.bicepparam \
//      -p githubAppPrivateKey=@app.pem.b64 githubAppId=… githubWebhookSecret=… \
//         claudeOauthToken=… postgresAdminPassword=…
// The live deploy is operator-run (the agent only authors this). See
// docs/runbooks/azure-deploy.md.
targetScope = 'resourceGroup'

@description('Name prefix for resources (lowercase, short).')
param namePrefix string = 'factory'
@description('Environment discriminator (dev | prod).')
@allowed([ 'dev', 'prod' ])
param env string = 'dev'
@description('Azure region. Defaults to the resource group location.')
param location string = resourceGroup().location
@description('Container image tag deployed for every service + the worker (e.g. a git sha).')
param imageTag string = 'latest'
@description('Dev only: a client IP allowed through the Postgres firewall (public access).')
param devClientIp string = ''
@description('''Whether to deploy the workloads (Container Apps + Jobs) that pull images from
this template's ACR. Set `false` for the FIRST deploy of a fresh resource group to stand up
infra + an empty ACR; CD then builds/pushes the images and re-deploys with `true`. This breaks
the bootstrap cycle (an app can't be created before its image exists, but the ACR that holds
the image is created here). Idempotent on subsequent deploys. Defaults to `false` so a
param-file-only deploy of a fresh resource group can't try to pull not-yet-pushed images;
CD/phase-2 passes `true` explicitly.''')
param deployWorkloads bool = false

// --- secrets (operator supplies at deploy; never committed) ---
@description('GitHub App id.')
@secure()
param githubAppId string
@description('GitHub App private key (PEM).')
@secure()
param githubAppPrivateKey string
@description('GitHub App webhook secret.')
@secure()
param githubWebhookSecret string
@description('Claude Code OAuth setup-token.')
@secure()
param claudeOauthToken string
@description('Postgres administrator password.')
@secure()
param postgresAdminPassword string

var tags = {
  project: 'factory'
  env: env
}
var suffix = uniqueString(resourceGroup().id)
// Globally-unique names (lowercased, length-capped).
var acrName = toLower('${namePrefix}acr${suffix}')
var kvName = take(toLower('${namePrefix}-kv-${suffix}'), 24)
var storageName = take(toLower('${namePrefix}st${suffix}'), 24)
var pgName = toLower('${namePrefix}-pg-${env}-${suffix}')
var logName = '${namePrefix}-logs-${env}'
var caeName = '${namePrefix}-cae-${env}'
var pgAdmin = 'factoryadmin'
var pgDatabase = 'factory'
var isDev = env == 'dev'
// The worker Job's name — referenced by the dispatcher (FACTORY_WORKER_JOB_NAME, so the
// launcher's ARM start URL targets it) and by the role assignment that lets the dispatcher
// start it. One var so the three sites can never drift.
var workerJobName = '${namePrefix}-worker-${env}'

// ---------------------------------------------------------------------------
// Observability: Log Analytics (the OTel exporter is deferred — Container Apps ships
// container stdout/stderr here, and the structured `tracing` JSON satisfies the
// KQL-queryable logging mandate).
// ---------------------------------------------------------------------------
resource logs 'Microsoft.OperationalInsights/workspaces@2023-09-01' = {
  name: logName
  location: location
  tags: tags
  properties: {
    sku: { name: 'PerGB2018' }
    retentionInDays: 30
  }
}

// --- ACR ---
resource acr 'Microsoft.ContainerRegistry/registries@2023-11-01-preview' = {
  name: acrName
  location: location
  tags: tags
  sku: { name: isDev ? 'Basic' : 'Standard' }
  properties: {
    adminUserEnabled: false
  }
}

// --- Key Vault (RBAC) + secrets ---
resource kv 'Microsoft.KeyVault/vaults@2023-07-01' = {
  name: kvName
  location: location
  tags: tags
  properties: {
    sku: { family: 'A', name: 'standard' }
    tenantId: subscription().tenantId
    enableRbacAuthorization: true
    enableSoftDelete: true
  }
}

// --- Storage (Blob, LRS hot) + containers + 30-day lifecycle ---
resource storage 'Microsoft.Storage/storageAccounts@2023-05-01' = {
  name: storageName
  location: location
  tags: tags
  sku: { name: 'Standard_LRS' }
  kind: 'StorageV2'
  properties: {
    accessTier: 'Hot'
    allowSharedKeyAccess: false // Managed Identity only (no shared key in prod)
    minimumTlsVersion: 'TLS1_2'
  }
}
resource blobSvc 'Microsoft.Storage/storageAccounts/blobServices@2023-05-01' = {
  parent: storage
  name: 'default'
}
resource checkpointsContainer 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = {
  parent: blobSvc
  name: 'checkpoints'
}
resource transcriptsContainer 'Microsoft.Storage/storageAccounts/blobServices/containers@2023-05-01' = {
  parent: blobSvc
  name: 'transcripts'
}
resource lifecycle 'Microsoft.Storage/storageAccounts/managementPolicies@2023-05-01' = {
  parent: storage
  name: 'default'
  properties: {
    policy: {
      rules: [
        {
          name: 'expire-30d'
          enabled: true
          type: 'Lifecycle'
          definition: {
            filters: { blobTypes: [ 'blockBlob', 'appendBlob' ] }
            actions: { baseBlob: { delete: { daysAfterModificationGreaterThan: 30 } } }
          }
        }
      ]
    }
  }
}

// --- Postgres Flexible Server (Burstable, no HA) + database ---
resource pg 'Microsoft.DBforPostgreSQL/flexibleServers@2023-12-01-preview' = {
  name: pgName
  location: location
  tags: tags
  sku: {
    name: 'Standard_B1ms'
    tier: 'Burstable'
  }
  properties: {
    version: '16'
    administratorLogin: pgAdmin
    administratorLoginPassword: postgresAdminPassword
    storage: { storageSizeGB: 32 }
    backup: { backupRetentionDays: 7, geoRedundantBackup: 'Disabled' }
    highAvailability: { mode: 'Disabled' }
    network: {
      // Public access + firewall (simpler). Prod hardening (deferred) = a private endpoint
      // with publicNetworkAccess 'Disabled', at which point the AllowAzureServices rule below
      // becomes unnecessary. See docs/runbooks/azure-deploy.md "Prod hardening".
      publicNetworkAccess: 'Enabled'
    }
  }
}
resource pgDb 'Microsoft.DBforPostgreSQL/flexibleServers/databases@2023-12-01-preview' = {
  parent: pg
  name: pgDatabase
  properties: {
    charset: 'UTF8'
    collation: 'en_US.utf8'
  }
}
// Allow Azure-origin traffic (the special 0.0.0.0 rule) whenever public access is on — this is
// what lets the Container Apps services + jobs reach Postgres. It must track the networking
// config, NOT the env: `publicNetworkAccess` is 'Enabled' for every env here, so a prod deploy
// needs this rule too (without it, prod Postgres is unreachable by Container Apps). It becomes
// unnecessary only once prod moves to a private endpoint (publicNetworkAccess 'Disabled').
resource pgAllowAzure 'Microsoft.DBforPostgreSQL/flexibleServers/firewallRules@2023-12-01-preview' = {
  parent: pg
  name: 'AllowAzureServices'
  properties: { startIpAddress: '0.0.0.0', endIpAddress: '0.0.0.0' }
}
// dev convenience: an optional operator IP through the firewall (e.g. for psql from a laptop).
resource pgAllowClient 'Microsoft.DBforPostgreSQL/flexibleServers/firewallRules@2023-12-01-preview' = if (isDev && !empty(devClientIp)) {
  parent: pg
  name: 'AllowClientIp'
  properties: { startIpAddress: devClientIp, endIpAddress: devClientIp }
}

// --- Key Vault secrets the services consume (env via secretRef) ---
var databaseUrl = 'postgres://${pgAdmin}:${postgresAdminPassword}@${pg.properties.fullyQualifiedDomainName}:5432/${pgDatabase}?sslmode=require'
// Param-derived secrets (known at deployment start, so they can drive a `for` loop).
var kvSecrets = [
  { name: 'github-app-id', value: githubAppId }
  { name: 'github-app-private-key', value: githubAppPrivateKey }
  { name: 'github-webhook-secret', value: githubWebhookSecret }
  { name: 'claude-oauth-token', value: claudeOauthToken }
]
resource secrets 'Microsoft.KeyVault/vaults/secrets@2023-07-01' = [
  for s in kvSecrets: {
    parent: kv
    name: s.name
    properties: { value: s.value }
  }
]
// database-url is built from the Postgres FQDN (a runtime value), so it can't live in the
// loop above (a `for` array must be computable at the start of the deployment).
resource databaseUrlSecret 'Microsoft.KeyVault/vaults/secrets@2023-07-01' = {
  parent: kv
  name: 'database-url'
  properties: { value: databaseUrl }
}
var kvUri = kv.properties.vaultUri
// Stable Key Vault secret URIs (version-less; Container Apps resolves the latest).
var secretUri = {
  databaseUrl: '${kvUri}secrets/database-url'
  githubAppId: '${kvUri}secrets/github-app-id'
  githubAppPrivateKey: '${kvUri}secrets/github-app-private-key'
  githubWebhookSecret: '${kvUri}secrets/github-webhook-secret'
  claudeOauthToken: '${kvUri}secrets/claude-oauth-token'
}

// --- Container Apps managed environment (logs → Log Analytics) ---
resource cae 'Microsoft.App/managedEnvironments@2024-03-01' = {
  name: caeName
  location: location
  tags: tags
  properties: {
    appLogsConfiguration: {
      destination: 'log-analytics'
      logAnalyticsConfiguration: {
        customerId: logs.properties.customerId
        sharedKey: logs.listKeys().primarySharedKey
      }
    }
  }
}

// --- Per-service user-assigned Managed Identities + least-privilege role assignments ---
var serviceNames = [ 'intake', 'tools', 'coordinator', 'dispatcher', 'dashboard-api' ]
resource serviceMis 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = [
  for s in serviceNames: {
    name: '${namePrefix}-${s}-${env}-id'
    location: location
    tags: tags
  }
]
resource workerMi 'Microsoft.ManagedIdentity/userAssignedIdentities@2023-01-31' = {
  name: '${namePrefix}-worker-${env}-id'
  location: location
  tags: tags
}

// Built-in role definition ids.
var roleAcrPull = subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '7f951dda-4ed3-4680-a7ca-43fe172d538d')
var roleKvSecretsUser = subscriptionResourceId('Microsoft.Authorization/roleDefinitions', '4633458b-17de-408a-b874-0445c86b69e6')
var roleBlobDataContributor = subscriptionResourceId('Microsoft.Authorization/roleDefinitions', 'ba92f5b4-2d11-453d-a403-e96b0029c9fe')
var roleContributor = subscriptionResourceId('Microsoft.Authorization/roleDefinitions', 'b24988ac-6180-42a0-ab88-20f7382dd24c')

// All service MIs + the worker MI get AcrPull + Key Vault Secrets User; tools + worker get
// Blob Data Contributor (they read/write checkpoints/transcripts).
resource acrPullForServices 'Microsoft.Authorization/roleAssignments@2022-04-01' = [
  for (s, i) in serviceNames: {
    name: guid(acr.id, serviceMis[i].id, 'acrpull')
    scope: acr
    properties: {
      roleDefinitionId: roleAcrPull
      principalId: serviceMis[i].properties.principalId
      principalType: 'ServicePrincipal'
    }
  }
]
resource kvForServices 'Microsoft.Authorization/roleAssignments@2022-04-01' = [
  for (s, i) in serviceNames: {
    name: guid(kv.id, serviceMis[i].id, 'kvsecrets')
    scope: kv
    properties: {
      roleDefinitionId: roleKvSecretsUser
      principalId: serviceMis[i].properties.principalId
      principalType: 'ServicePrincipal'
    }
  }
]
resource acrPullForWorker 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(acr.id, workerMi.id, 'acrpull')
  scope: acr
  properties: {
    roleDefinitionId: roleAcrPull
    principalId: workerMi.properties.principalId
    principalType: 'ServicePrincipal'
  }
}
resource kvForWorker 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(kv.id, workerMi.id, 'kvsecrets')
  scope: kv
  properties: {
    roleDefinitionId: roleKvSecretsUser
    principalId: workerMi.properties.principalId
    principalType: 'ServicePrincipal'
  }
}
resource blobForWorker 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(storage.id, workerMi.id, 'blob')
  scope: storage
  properties: {
    roleDefinitionId: roleBlobDataContributor
    principalId: workerMi.properties.principalId
    principalType: 'ServicePrincipal'
  }
}
// tools (index 1) gets Blob Data Contributor for transcripts.
resource blobForTools 'Microsoft.Authorization/roleAssignments@2022-04-01' = {
  name: guid(storage.id, serviceMis[1].id, 'blob')
  scope: storage
  properties: {
    roleDefinitionId: roleBlobDataContributor
    principalId: serviceMis[1].properties.principalId
    principalType: 'ServicePrincipal'
  }
}

// Image refs.
var acrLogin = acr.properties.loginServer
var servicesImage = '${acrLogin}/factory-services:${imageTag}'
var workerImage = '${acrLogin}/factory-worker:${imageTag}'
// Internal tools URL (Container Apps internal FQDN) the dispatcher passes to workers.
var toolsUrl = 'https://tools.internal.${cae.properties.defaultDomain}'
// Keyless (Managed Identity) blob connection string for tools + worker.
var blobConnTools = 'AccountName=${storageName};BlobEndpoint=https://${storageName}.blob.${environment().suffixes.storage}/;ManagedIdentityClientId=${serviceMis[1].properties.clientId}'
var blobConnWorker = 'AccountName=${storageName};BlobEndpoint=https://${storageName}.blob.${environment().suffixes.storage}/;ManagedIdentityClientId=${workerMi.properties.clientId}'

// --- Service apps ---
module intake 'modules/service-app.bicep' = if (deployWorkloads) {
  name: 'svc-intake'
  // Order the app AFTER its image-pull + secret-read grants and the secrets themselves, so
  // the first revision doesn't lose the race (image pull / KV secret resolution would 403).
  dependsOn: [ acrPullForServices, kvForServices, secrets, databaseUrlSecret ]
  params: {
    name: 'intake'
    location: location
    environmentId: cae.id
    image: servicesImage
    acrLoginServer: acrLogin
    identityId: serviceMis[0].id
    identityClientId: serviceMis[0].properties.clientId
    ingress: 'external' // GitHub delivers webhooks from the public internet
    targetPort: 8080
    tags: tags
    secretRefs: [
      { name: 'DATABASE_URL', keyVaultUri: secretUri.databaseUrl }
      { name: 'GITHUB_APP_ID', keyVaultUri: secretUri.githubAppId }
      { name: 'GITHUB_APP_PRIVATE_KEY', keyVaultUri: secretUri.githubAppPrivateKey }
      { name: 'GITHUB_WEBHOOK_SECRET', keyVaultUri: secretUri.githubWebhookSecret }
    ]
  }
}
module tools 'modules/service-app.bicep' = if (deployWorkloads) {
  name: 'svc-tools'
  dependsOn: [ acrPullForServices, kvForServices, blobForTools, secrets, databaseUrlSecret ]
  params: {
    name: 'tools'
    location: location
    environmentId: cae.id
    image: servicesImage
    acrLoginServer: acrLogin
    identityId: serviceMis[1].id
    identityClientId: serviceMis[1].properties.clientId
    ingress: 'internal' // called only by workers / other services inside the env (toolsUrl)
    targetPort: 8081
    tags: tags
    envVars: [ { name: 'FACTORY_BLOB_CONNECTION_STRING', value: blobConnTools } ]
    secretRefs: [
      { name: 'DATABASE_URL', keyVaultUri: secretUri.databaseUrl }
      { name: 'GITHUB_APP_ID', keyVaultUri: secretUri.githubAppId }
      { name: 'GITHUB_APP_PRIVATE_KEY', keyVaultUri: secretUri.githubAppPrivateKey }
    ]
  }
}
module coordinator 'modules/service-app.bicep' = if (deployWorkloads) {
  name: 'svc-coordinator'
  dependsOn: [ acrPullForServices, kvForServices, secrets, databaseUrlSecret ]
  params: {
    name: 'coordinator'
    location: location
    environmentId: cae.id
    image: servicesImage
    acrLoginServer: acrLogin
    identityId: serviceMis[2].id
    identityClientId: serviceMis[2].properties.clientId
    tags: tags
    secretRefs: [
      { name: 'DATABASE_URL', keyVaultUri: secretUri.databaseUrl }
      { name: 'GITHUB_APP_ID', keyVaultUri: secretUri.githubAppId }
      { name: 'GITHUB_APP_PRIVATE_KEY', keyVaultUri: secretUri.githubAppPrivateKey }
    ]
  }
}
module dispatcher 'modules/service-app.bicep' = if (deployWorkloads) {
  name: 'svc-dispatcher'
  dependsOn: [ acrPullForServices, kvForServices, secrets, databaseUrlSecret ]
  params: {
    name: 'dispatcher'
    location: location
    environmentId: cae.id
    image: servicesImage
    acrLoginServer: acrLogin
    identityId: serviceMis[3].id
    identityClientId: serviceMis[3].properties.clientId
    tags: tags
    envVars: [
      { name: 'FACTORY_WORKER_RUNTIME', value: 'container_apps' }
      { name: 'FACTORY_AZURE_SUBSCRIPTION_ID', value: subscription().subscriptionId }
      { name: 'FACTORY_AZURE_RESOURCE_GROUP', value: resourceGroup().name }
      { name: 'FACTORY_WORKER_JOB_NAME', value: workerJobName }
      { name: 'FACTORY_TOOLS_URL', value: toolsUrl }
    ]
    secretRefs: [
      { name: 'DATABASE_URL', keyVaultUri: secretUri.databaseUrl }
      { name: 'GITHUB_APP_ID', keyVaultUri: secretUri.githubAppId }
      { name: 'GITHUB_APP_PRIVATE_KEY', keyVaultUri: secretUri.githubAppPrivateKey }
    ]
  }
}
module dashboard 'modules/service-app.bicep' = if (deployWorkloads) {
  name: 'svc-dashboard'
  dependsOn: [ acrPullForServices, kvForServices, databaseUrlSecret ]
  params: {
    name: 'dashboard-api'
    location: location
    environmentId: cae.id
    image: servicesImage
    acrLoginServer: acrLogin
    identityId: serviceMis[4].id
    identityClientId: serviceMis[4].properties.clientId
    ingress: 'external' // operators view the dashboard
    targetPort: 8082
    tags: tags
    secretRefs: [
      { name: 'DATABASE_URL', keyVaultUri: secretUri.databaseUrl }
    ]
  }
}

// The dispatcher MI must be able to START the worker Job (ARM `jobs/.../start`).
// Contributor scoped to the single worker Job (narrow). Could be tightened to a custom
// role granting only Microsoft.App/jobs/start/action.
resource dispatcherStartsWorker 'Microsoft.Authorization/roleAssignments@2022-04-01' = if (deployWorkloads) {
  name: guid(workerJobRes.id, serviceMis[3].id, 'jobstart')
  scope: workerJobRes
  properties: {
    roleDefinitionId: roleContributor
    principalId: serviceMis[3].properties.principalId
    principalType: 'ServicePrincipal'
  }
  dependsOn: [ workerJob ] // the existing-ref resolves only after the module creates it
}
// Reference the worker Job resource for the role-assignment scope.
resource workerJobRes 'Microsoft.App/jobs@2024-03-01' existing = {
  name: workerJobName
}

// --- Worker Job (started per task by the dispatcher) ---
module workerJob 'modules/job.bicep' = if (deployWorkloads) {
  name: 'job-worker'
  dependsOn: [ acrPullForWorker, kvForWorker, blobForWorker, secrets ]
  params: {
    name: workerJobName
    location: location
    environmentId: cae.id
    image: workerImage
    acrLoginServer: acrLogin
    identityId: workerMi.id
    identityClientId: workerMi.properties.clientId
    containerName: 'worker'
    replicaTimeoutSeconds: 2100
    tags: tags
    envVars: [
      { name: 'FACTORY_BLOB_CONNECTION_STRING', value: blobConnWorker }
      { name: 'FACTORY_TOOLS_URL', value: toolsUrl }
    ]
    secretRefs: [
      { name: 'CLAUDE_CODE_OAUTH_TOKEN', keyVaultUri: secretUri.claudeOauthToken }
    ]
  }
}

// --- One-shot migration Job (CD runs it before rolling the service revisions) ---
module migrateJob 'modules/job.bicep' = if (deployWorkloads) {
  name: 'job-migrate'
  dependsOn: [ acrPullForServices, kvForServices, databaseUrlSecret ]
  params: {
    name: '${namePrefix}-migrate-${env}'
    location: location
    environmentId: cae.id
    image: servicesImage
    acrLoginServer: acrLogin
    identityId: serviceMis[0].id // reuse intake's MI (has KV Secrets User for DATABASE_URL)
    identityClientId: serviceMis[0].properties.clientId
    containerName: 'migrate'
    command: [ '/usr/local/bin/factory-migrate' ]
    replicaTimeoutSeconds: 600
    tags: tags
    secretRefs: [
      { name: 'DATABASE_URL', keyVaultUri: secretUri.databaseUrl }
    ]
  }
}

output acrLoginServer string = acrLogin
output acrName string = acrName
output intakeFqdn string = intake.?outputs.fqdn ?? ''
output dashboardFqdn string = dashboard.?outputs.fqdn ?? ''
output keyVaultName string = kvName

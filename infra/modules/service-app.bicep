// One long-lived Factory service as a Container App (N replicas). Reused for intake,
// tools, coordinator, dispatcher, dashboard-api. Secrets are Key Vault references resolved
// via the app's user-assigned Managed Identity; non-secret config is plain env.
@description('Service name (also the Container App name suffix).')
param name string
@description('Azure region.')
param location string
@description('Container Apps managed environment resource id.')
param environmentId string
@description('Full container image reference (ACR login server / repo:tag).')
param image string
@description('ACR login server (for the registry block).')
param acrLoginServer string
@description('User-assigned Managed Identity resource id for this service.')
param identityId string
@description('Managed Identity client id (passed to the app as FACTORY_MANAGED_IDENTITY_CLIENT_ID).')
param identityClientId string
@description('''Ingress mode:
- `external` — public HTTPS FQDN (`<name>.<envDomain>`), e.g. intake (GitHub webhooks) + dashboard.
- `internal` — env-only HTTPS FQDN (`<name>.internal.<envDomain>`), e.g. tools (called by
  workers/other services inside the environment, never public).
- `none` — no ingress (background workers: coordinator, dispatcher).''')
@allowed([ 'external', 'internal', 'none' ])
param ingress string = 'none'
@description('Container target port for ingress (ignored when ingress is "none").')
param targetPort int = 8080
@description('Min / max replicas.')
param minReplicas int = 1
param maxReplicas int = 3
@description('Plain (non-secret) environment variables: list of { name, value }.')
param envVars array = []
@description('Key Vault secret references: list of { name (env var), secretName, keyVaultUri }.')
param secretRefs array = []
@description('Common resource tags.')
param tags object = {}

// Container App secrets backed by Key Vault, pulled via this app's Managed Identity.
var caSecrets = [
  for s in secretRefs: {
    name: toLower(s.name)
    keyVaultUrl: s.keyVaultUri
    identity: identityId
  }
]
// Env vars that reference those secrets.
var secretEnv = [
  for s in secretRefs: {
    name: s.name
    secretRef: toLower(s.name)
  }
]

resource app 'Microsoft.App/containerApps@2024-03-01' = {
  name: name
  location: location
  tags: tags
  identity: {
    type: 'UserAssigned'
    userAssignedIdentities: {
      '${identityId}': {}
    }
  }
  properties: {
    managedEnvironmentId: environmentId
    configuration: {
      activeRevisionsMode: 'Single'
      secrets: caSecrets
      registries: [
        {
          server: acrLoginServer
          identity: identityId
        }
      ]
      ingress: ingress == 'none' ? null : {
        external: ingress == 'external'
        targetPort: targetPort
        transport: 'auto'
        traffic: [ { latestRevision: true, weight: 100 } ]
      }
    }
    template: {
      containers: [
        {
          name: name
          image: image
          // The shared `services` image has NO ENTRYPOINT/CMD (each service picks its binary —
          // see the Dockerfile + docker-compose). Every service binary is `factory-<name>`
          // (intake→factory-intake, dashboard-api→factory-dashboard-api, …), so derive it from
          // the service name. Without this the container starts the base image's default + exits.
          command: [ '/usr/local/bin/factory-${name}' ]
          resources: {
            cpu: json('0.5')
            memory: '1Gi'
          }
          env: concat(
            [ { name: 'FACTORY_MANAGED_IDENTITY_CLIENT_ID', value: identityClientId } ],
            envVars,
            secretEnv
          )
        }
      ]
      scale: {
        minReplicas: minReplicas
        maxReplicas: maxReplicas
      }
    }
  }
}

output fqdn string = ingress == 'none' ? '' : app.properties.configuration.ingress.fqdn

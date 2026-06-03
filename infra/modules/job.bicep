// A Container Apps Job (manual trigger, scale-to-zero). Reused for the worker Job (started
// per task by the dispatcher's ContainerAppsJobLauncher) and the one-shot migration Job
// (run by CD before rolling the service revisions).
@description('Job name.')
param name string
param location string
@description('Container Apps managed environment resource id.')
param environmentId string
@description('Full container image reference.')
param image string
@description('ACR login server (for the registry block).')
param acrLoginServer string
@description('User-assigned Managed Identity resource id.')
param identityId string
@description('Managed Identity client id (exposed as FACTORY_MANAGED_IDENTITY_CLIENT_ID).')
param identityClientId string
@description('Container name (the worker Job MUST be "worker" to match the launcher override).')
param containerName string
@description('Optional command override (e.g. the migrate bin); empty = image default.')
param command array = []
@description('Per-replica timeout (seconds) — the worker wall-clock cap, with headroom.')
param replicaTimeoutSeconds int = 2100
@description('Plain (non-secret) env vars: list of { name, value }.')
param envVars array = []
@description('Key Vault secret references: list of { name, secretName, keyVaultUri }.')
param secretRefs array = []
param tags object = {}

var caSecrets = [
  for s in secretRefs: {
    name: toLower(s.name)
    keyVaultUrl: s.keyVaultUri
    identity: identityId
  }
]
var secretEnv = [
  for s in secretRefs: {
    name: s.name
    secretRef: toLower(s.name)
  }
]

resource job 'Microsoft.App/jobs@2024-03-01' = {
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
    environmentId: environmentId
    configuration: {
      triggerType: 'Manual'
      replicaTimeout: replicaTimeoutSeconds
      replicaRetryLimit: 0
      manualTriggerConfig: {
        parallelism: 1
        replicaCompletionCount: 1
      }
      secrets: caSecrets
      registries: [
        {
          server: acrLoginServer
          identity: identityId
        }
      ]
    }
    template: {
      containers: [
        {
          name: containerName
          image: image
          command: empty(command) ? null : command
          resources: {
            cpu: json('1.0')
            memory: '2Gi'
          }
          env: concat(
            [ { name: 'FACTORY_MANAGED_IDENTITY_CLIENT_ID', value: identityClientId } ],
            envVars,
            secretEnv
          )
        }
      ]
    }
  }
}

output jobName string = job.name

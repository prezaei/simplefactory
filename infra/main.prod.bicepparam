// Prod parameters (authored for completeness; dev is this phase's deploy target).
// Same env-var-backed secret handling as dev.
//
// PROD HARDENING (follow-up, not in this template's dev exit): switch the Postgres
// `publicNetworkAccess` to 'Disabled' behind a private endpoint + private DNS zone, and set
// the tools Container App ingress to internal-only. Those resources are intentionally NOT
// authored here yet — see docs/runbooks/azure-deploy.md "Prod hardening".
using 'main.bicep'

param namePrefix = 'factory'
param env = 'prod'
param imageTag = readEnvironmentVariable('FACTORY_IMAGE_TAG', 'latest')

param githubAppId = readEnvironmentVariable('FACTORY_GITHUB_APP_ID', '')
param githubAppPrivateKey = readEnvironmentVariable('FACTORY_GITHUB_APP_PRIVATE_KEY', '')
param githubWebhookSecret = readEnvironmentVariable('FACTORY_GITHUB_WEBHOOK_SECRET', '')
param claudeOauthToken = readEnvironmentVariable('FACTORY_CLAUDE_OAUTH_TOKEN', '')
param postgresAdminPassword = readEnvironmentVariable('FACTORY_PG_ADMIN_PASSWORD', '')

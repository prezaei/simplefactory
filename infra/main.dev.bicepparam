// Dev parameters (the exit target this PR deploys). Public-access Postgres + firewall.
//
// The five secrets are read from ENVIRONMENT VARIABLES at param-compile time (never
// committed). CD / the operator exports them before `az deployment`; see
// docs/runbooks/azure-deploy.md. The `''` defaults let CI compile this file without the
// secrets present (a real deploy with empty values fails fast, which is the intent).
using 'main.bicep'

param namePrefix = 'factory'
param env = 'dev'
// CD overrides this with the built git sha; `latest` is only the manual-deploy default.
param imageTag = readEnvironmentVariable('FACTORY_IMAGE_TAG', 'latest')
// Optional: an operator IP allowed through the Postgres firewall (else only Azure services).
param devClientIp = readEnvironmentVariable('FACTORY_DEV_CLIENT_IP', '')

param githubAppId = readEnvironmentVariable('FACTORY_GITHUB_APP_ID', '')
param githubAppPrivateKey = readEnvironmentVariable('FACTORY_GITHUB_APP_PRIVATE_KEY', '')
param githubWebhookSecret = readEnvironmentVariable('FACTORY_GITHUB_WEBHOOK_SECRET', '')
param claudeOauthToken = readEnvironmentVariable('FACTORY_CLAUDE_OAUTH_TOKEN', '')
param postgresAdminPassword = readEnvironmentVariable('FACTORY_PG_ADMIN_PASSWORD', '')

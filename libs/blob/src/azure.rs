//! Azure Blob–backed stores (P7): the **production** [`CheckpointStore`] and
//! [`TranscriptStore`]. Exercised locally against **Azurite** (the Azure Blob
//! emulator in `docker-compose.yml`) and against real Azure Blob in P8 — the only
//! P8 delta is the credential/endpoint the client is constructed with, not the
//! read/write data path.
//!
//! Auth/endpoint come from a single Azure **connection string**
//! (`FACTORY_BLOB_CONNECTION_STRING`). Azurite uses the well-known dev account with a
//! `BlobEndpoint=` at the emulator host:port (e.g.
//! `http://host.docker.internal:10000/devstoreaccount1` from inside a worker container);
//! real-Azure shared-key uses `AccountName`/`AccountKey`. P8 added the **Managed Identity**
//! path: a **keyless** connection string (`AccountName`, no `AccountKey`) selects
//! `StorageCredentials::token_credential` (see `factory_azure_auth`) — the only dev/prod
//! divergence in this module, isolated to credential construction. (A future step may
//! adopt the SDK's own `ConnectionString` parser for sovereign-cloud `EndpointSuffix`/SAS;
//! this parser covers Azurite + public-cloud shared-key + MI, which is all Factory uses.)

use anyhow::Context;
use azure_core::request_options::IfMatchCondition;
use azure_core::{Error as AzureError, StatusCode};
use azure_storage::{prelude::*, CloudLocation, EMULATOR_ACCOUNT, EMULATOR_ACCOUNT_KEY};
use azure_storage_blobs::prelude::*;

use crate::{CheckpointStore, TranscriptStore};

/// Parsed Azure Blob connection details — the testable core of connection-string
/// handling, kept separate from the (network-touching) client construction so the
/// parse/location-mapping logic is unit-tested without Azurite.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BlobLocation {
    account: String,
    credential: BlobCredential,
    endpoint: BlobEndpoint,
}

/// Where the blob client points — exactly one of three, so the (previously two `Option`
/// fields) mutually-exclusive states can't both be set:
/// - `Emulator` ⇒ Azurite (shared-key + `BlobEndpoint`, or `UseDevelopmentStorage=true`);
///   an explicit endpoint lets a worker container reach host.docker.internal.
/// - `Custom` ⇒ a real-Azure custom base URL (Managed Identity + an explicit `BlobEndpoint`,
///   e.g. Private Link / sovereign) — honored, not dropped.
/// - `Public` ⇒ real Azure public cloud (`https://{account}.blob.core.windows.net`).
#[derive(Debug, Clone, PartialEq, Eq)]
enum BlobEndpoint {
    Emulator { address: String, port: u16 },
    Custom(String),
    Public,
}

/// How to authenticate to Blob — the one dev/prod fork (the rest of the data path is
/// identical). Azurite/dev (and any `AccountKey=` connection string) is **shared-key**;
/// a keyless prod connection string (`AccountName` + endpoint, no `AccountKey`) is
/// **Managed Identity** with an optional user-assigned `ManagedIdentityClientId`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BlobCredential {
    SharedKey(String),
    ManagedIdentity { client_id: Option<String> },
}

/// Parse an Azure Storage connection string into the bits we need. Supports the
/// `UseDevelopmentStorage=true` shortcut, an explicit Azurite `BlobEndpoint=`, a
/// real-Azure `AccountName`/`AccountKey` pair (shared-key), AND a keyless prod string
/// (`AccountName` with no `AccountKey`) which selects **Managed Identity** — optionally
/// with a user-assigned `ManagedIdentityClientId=<guid>`. Unknown segments (e.g.
/// `DefaultEndpointsProtocol`) are ignored.
fn parse_connection_string(conn: &str) -> anyhow::Result<BlobLocation> {
    let mut account: Option<String> = None;
    let mut key: Option<String> = None;
    let mut endpoint: Option<String> = None;
    let mut mi_client_id: Option<String> = None;
    let mut use_dev = false;

    for segment in conn.split(';') {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        // split_once: the value (e.g. a base64 AccountKey) may itself contain '='.
        let (k, v) = segment
            .split_once('=')
            .with_context(|| format!("malformed connection-string segment: {segment}"))?;
        match k.trim() {
            "AccountName" => account = Some(v.trim().to_string()),
            // An empty value (`AccountKey=;`, a blanked template) is treated as ABSENT,
            // not as an empty shared key — so it selects Managed Identity (the prod
            // default) rather than failing later with an opaque empty-key auth error.
            "AccountKey" => key = non_empty(v),
            "BlobEndpoint" => endpoint = non_empty(v),
            "ManagedIdentityClientId" => mi_client_id = non_empty(v),
            "UseDevelopmentStorage" => use_dev = v.trim().eq_ignore_ascii_case("true"),
            _ => {} // ignore the rest (DefaultEndpointsProtocol, QueueEndpoint, …)
        }
    }

    // Endpoint: a shared-key/dev string with a `BlobEndpoint` (or `UseDevelopmentStorage`)
    // is the Azurite emulator; Managed Identity targets real Azure, so a `BlobEndpoint`
    // with MI is a real CUSTOM base URL (Private Link / sovereign) — honored, not dropped;
    // no endpoint ⇒ public cloud.
    let endpoint = match (&endpoint, key.is_some() || use_dev) {
        (Some(ep), true) => {
            let (address, port) = parse_host_port(ep)?;
            BlobEndpoint::Emulator { address, port }
        }
        (Some(ep), false) => BlobEndpoint::Custom(ep.clone()),
        (None, _) if use_dev => BlobEndpoint::Emulator {
            address: "127.0.0.1".to_string(),
            port: 10000,
        },
        (None, _) => BlobEndpoint::Public,
    };

    // Credential + account in one pass: dev ⇒ shared-key with the well-known emulator
    // account/key fallbacks; a key ⇒ shared-key; no key ⇒ Managed Identity.
    let (account, credential) = if use_dev {
        (
            account.unwrap_or_else(|| EMULATOR_ACCOUNT.to_string()),
            BlobCredential::SharedKey(key.unwrap_or_else(|| EMULATOR_ACCOUNT_KEY.to_string())),
        )
    } else {
        let account = account.context("connection string missing AccountName")?;
        let credential = match key {
            Some(k) => BlobCredential::SharedKey(k),
            None => BlobCredential::ManagedIdentity {
                client_id: mi_client_id,
            },
        };
        (account, credential)
    };
    Ok(BlobLocation {
        account,
        credential,
        endpoint,
    })
}

/// A trimmed connection-string value, or `None` if empty — so a blank `Key=` segment is
/// treated as absent rather than an empty string.
fn non_empty(v: &str) -> Option<String> {
    let t = v.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Pull `(host, port)` out of a `BlobEndpoint` URL, defaulting the port by scheme.
fn parse_host_port(endpoint: &str) -> anyhow::Result<(String, u16)> {
    let url = azure_core::Url::parse(endpoint)
        .with_context(|| format!("BlobEndpoint is not a valid URL: {endpoint}"))?;
    let host = url
        .host_str()
        .with_context(|| format!("BlobEndpoint has no host: {endpoint}"))?
        .to_string();
    let port = url
        .port()
        .unwrap_or(if url.scheme() == "https" { 443 } else { 80 });
    Ok((host, port))
}

/// Build + ensure a [`ContainerClient`] for `container` from a connection string,
/// creating the container if absent. The only dev/prod fork lives here: emulator vs.
/// public-cloud location. Shared by both stores so the connect lifecycle is defined
/// once.
async fn connect_container(conn: &str, container: &str) -> anyhow::Result<ContainerClient> {
    let loc = parse_connection_string(conn)?;
    // The ONE dev/prod fork: shared-key (Azurite/dev) vs Managed Identity (prod). The
    // read/write data path below is identical for both — that's why P8 is a credential
    // swap, not new code.
    let credentials = match &loc.credential {
        BlobCredential::SharedKey(key) => {
            tracing::info!(account = %loc.account, "azure blob: connecting with shared key");
            StorageCredentials::access_key(loc.account.clone(), key.clone())
        }
        BlobCredential::ManagedIdentity { client_id } => {
            tracing::info!(account = %loc.account, client_id = ?client_id, "azure blob: connecting with managed identity");
            StorageCredentials::token_credential(std::sync::Arc::new(
                factory_azure_auth::ManagedIdentityCredential::new(client_id.clone()),
            ))
        }
    };
    let cloud = match loc.endpoint {
        BlobEndpoint::Emulator { address, port } => CloudLocation::Emulator { address, port },
        // A real-Azure custom base URL (MI + Private Link / sovereign endpoint) — honored.
        BlobEndpoint::Custom(uri) => CloudLocation::Custom {
            account: loc.account.clone(),
            uri,
        },
        BlobEndpoint::Public => CloudLocation::Public {
            account: loc.account.clone(),
        },
    };
    let client = ClientBuilder::with_location(cloud, credentials).container_client(container);
    ensure_container(&client).await?;
    Ok(client)
}

/// Create `container` if it does not already exist. One round-trip: attempt the
/// create and tolerate the already-exists conflict (the steady state once a prior run
/// made it, and the lost-race case if two replicas create concurrently). Mirrors the
/// append-blob conditional-create below.
async fn ensure_container(container: &ContainerClient) -> anyhow::Result<()> {
    match container.create().await {
        Ok(_) => {
            tracing::info!(
                container = container.container_name(),
                "azure blob: created container"
            );
            Ok(())
        }
        Err(e) if is_already_exists(&e) => Ok(()),
        Err(e) => Err(e).context("creating blob container"),
    }
}

/// Read a blob's full content, mapping a missing blob (404 `BlobNotFound`) to `None`.
/// Shared by both stores' reads so the 404-handling lives in one place.
async fn get_optional(container: &ContainerClient, uri: &str) -> anyhow::Result<Option<Vec<u8>>> {
    match container.blob_client(uri).get_content().await {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if is_blob_not_found(&e) => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading blob {uri}")),
    }
}

/// True when an Azure error is a 404 for a missing *blob* — mapped to the
/// `None`/empty read semantics the store traits promise. A 404 explicitly labelled
/// `BlobNotFound`, OR an unlabelled 404 (no `x-ms-error-code` header — some servers
/// omit it), is treated as "no blob yet". A 404 with a *different* explicit code
/// (e.g. `ContainerNotFound` — a misconfiguration, not a missing blob) is NOT
/// swallowed: it propagates so a real fault can't masquerade as an empty/absent value
/// (which would silently abandon a recoverable checkpoint). The unlabelled-404 case is
/// a deliberate trade-off: lenient enough to tolerate a header-omitting emulator,
/// while still surfacing any 404 the server *does* label with a non-blob code.
fn is_blob_not_found(err: &AzureError) -> bool {
    match err.as_http_error() {
        Some(h) if h.status() == StatusCode::NotFound => {
            matches!(h.error_code(), Some("BlobNotFound") | None)
        }
        _ => false,
    }
}

/// True when a create lost the race because the target ALREADY EXISTS: an explicit
/// `ContainerAlreadyExists`/`BlobAlreadyExists` 409, or a `412 PreconditionFailed`
/// from a conditional create (`If-None-Match: *`). Deliberately narrow: a bare 409
/// with a different code (e.g. `ContainerBeingDeleted`) is a REAL error and must NOT
/// be swallowed as "already exists" (which would make a broken connect look healthy).
fn is_already_exists(err: &AzureError) -> bool {
    match err.as_http_error() {
        Some(h) if h.status() == StatusCode::PreconditionFailed => true,
        Some(h) if h.status() == StatusCode::Conflict => {
            matches!(
                h.error_code(),
                Some("ContainerAlreadyExists") | Some("BlobAlreadyExists")
            )
        }
        _ => false,
    }
}

/// Azure Blob–backed [`CheckpointStore`]: each `uri` is a block blob, latest-wins.
pub struct AzureBlobCheckpoints {
    container: ContainerClient,
}

impl AzureBlobCheckpoints {
    /// Connect to `container` via the connection string, creating the container if
    /// absent so the first checkpoint write can't 404 on a missing container.
    pub async fn connect(connection_string: &str, container: &str) -> anyhow::Result<Self> {
        let container = connect_container(connection_string, container).await?;
        tracing::info!(
            container = container.container_name(),
            "azure blob: checkpoint store ready"
        );
        Ok(Self { container })
    }
}

#[async_trait::async_trait]
impl CheckpointStore for AzureBlobCheckpoints {
    async fn write(&self, uri: &str, data: &[u8]) -> anyhow::Result<()> {
        // Block blob put is whole-snapshot, latest-wins — exactly checkpoint semantics.
        self.container
            .blob_client(uri)
            .put_block_blob(data.to_vec())
            .await
            .with_context(|| format!("writing checkpoint blob {uri}"))?;
        tracing::info!(uri, bytes = data.len(), "azure blob: wrote checkpoint");
        Ok(())
    }

    async fn read(&self, uri: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let got = get_optional(&self.container, uri).await?;
        match &got {
            Some(b) => tracing::info!(uri, bytes = b.len(), "azure blob: read checkpoint"),
            None => tracing::info!(uri, "azure blob: no checkpoint at uri"),
        }
        Ok(got)
    }
}

/// Azure Blob–backed [`TranscriptStore`]: each `uri` is an **append blob**, so a
/// chunk is appended (not a read-modify-write of the whole transcript).
pub struct AzureBlobTranscripts {
    container: ContainerClient,
}

impl AzureBlobTranscripts {
    /// Connect to `container` via the connection string, creating it if absent.
    pub async fn connect(connection_string: &str, container: &str) -> anyhow::Result<Self> {
        let container = connect_container(connection_string, container).await?;
        tracing::info!(
            container = container.container_name(),
            "azure blob: transcript store ready"
        );
        Ok(Self { container })
    }
}

#[async_trait::async_trait]
impl TranscriptStore for AzureBlobTranscripts {
    async fn append(&self, uri: &str, chunk: &str) -> anyhow::Result<()> {
        let blob = self.container.blob_client(uri);
        // Steady state is a single request: append directly. Only the FIRST chunk for a
        // uri hits the 404 path and lazily creates the append blob. The create is
        // conditional (`If-None-Match: *`) so a concurrent first-append can't RESET an
        // append blob another writer just created (which would truncate its chunk).
        match blob.append_block(chunk.to_string()).await {
            Ok(_) => {}
            Err(e) if is_blob_not_found(&e) => {
                match blob
                    .put_append_blob()
                    .if_match(IfMatchCondition::NotMatch("*".to_string()))
                    .await
                {
                    Ok(_) => {}
                    // Another writer created it between our append and create — fine,
                    // the retry append below lands on the existing blob.
                    Err(e) if is_already_exists(&e) => {}
                    Err(e) => return Err(e).with_context(|| format!("creating append blob {uri}")),
                }
                blob.append_block(chunk.to_string())
                    .await
                    .with_context(|| format!("appending to transcript blob {uri}"))?;
            }
            Err(e) => return Err(e).with_context(|| format!("appending to transcript blob {uri}")),
        }
        tracing::info!(
            uri,
            bytes = chunk.len(),
            "azure blob: appended transcript chunk"
        );
        Ok(())
    }

    async fn read(&self, uri: &str) -> anyhow::Result<String> {
        // An unwritten transcript reads as empty (matches the in-memory double).
        Ok(get_optional(&self.container, uri)
            .await?
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- connection-string parsing (no Azurite needed) ----

    /// Extract the shared key, asserting the credential is shared-key (test helper).
    fn shared_key(loc: &BlobLocation) -> &str {
        match &loc.credential {
            BlobCredential::SharedKey(k) => k,
            other => panic!("expected shared key, got {other:?}"),
        }
    }

    /// The emulator `(host, port)` if the endpoint is `Emulator`, else `None` (test helper).
    fn emulator(loc: &BlobLocation) -> Option<(String, u16)> {
        match &loc.endpoint {
            BlobEndpoint::Emulator { address, port } => Some((address.clone(), *port)),
            _ => None,
        }
    }

    #[test]
    fn parses_azurite_connection_string_with_custom_blob_endpoint() {
        let conn = "DefaultEndpointsProtocol=http;AccountName=devstoreaccount1;\
            AccountKey=Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==;\
            BlobEndpoint=http://host.docker.internal:10000/devstoreaccount1;";
        let loc = parse_connection_string(conn).unwrap();
        assert_eq!(loc.account, "devstoreaccount1");
        assert!(
            shared_key(&loc).ends_with("GMGw=="),
            "the full base64 key (with '=') is kept"
        );
        assert_eq!(
            emulator(&loc),
            Some(("host.docker.internal".to_string(), 10000))
        );
    }

    #[test]
    fn use_development_storage_shortcut_maps_to_local_emulator() {
        let loc = parse_connection_string("UseDevelopmentStorage=true;").unwrap();
        assert_eq!(loc.account, EMULATOR_ACCOUNT);
        assert_eq!(shared_key(&loc), EMULATOR_ACCOUNT_KEY);
        assert_eq!(emulator(&loc), Some(("127.0.0.1".to_string(), 10000)));
    }

    #[test]
    fn keyless_connection_string_selects_managed_identity_and_honors_endpoint() {
        // Prod: AccountName + endpoint, NO AccountKey ⇒ Managed Identity (system-assigned).
        let loc = parse_connection_string(
            "AccountName=factoryprod;BlobEndpoint=https://factoryprod.privatelink.blob.core.windows.net/;",
        )
        .unwrap();
        assert_eq!(loc.account, "factoryprod");
        assert_eq!(
            loc.credential,
            BlobCredential::ManagedIdentity { client_id: None }
        );
        // MI never uses the emulator; an explicit endpoint is a real custom base URL
        // (Private Link / sovereign) and is HONORED, not dropped.
        assert_eq!(
            loc.endpoint,
            BlobEndpoint::Custom(
                "https://factoryprod.privatelink.blob.core.windows.net/".to_string()
            )
        );
    }

    #[test]
    fn keyless_no_endpoint_is_managed_identity_on_public_cloud() {
        let loc = parse_connection_string("AccountName=factoryprod;").unwrap();
        assert_eq!(
            loc.credential,
            BlobCredential::ManagedIdentity { client_id: None }
        );
        assert_eq!(loc.endpoint, BlobEndpoint::Public);
    }

    #[test]
    fn empty_account_key_is_treated_as_absent_and_selects_managed_identity() {
        // A blanked `AccountKey=` (template placeholder) must NOT become an empty shared
        // key — it's absent, so Managed Identity is selected.
        let loc = parse_connection_string("AccountName=factoryprod;AccountKey=;").unwrap();
        assert_eq!(
            loc.credential,
            BlobCredential::ManagedIdentity { client_id: None }
        );
    }

    #[test]
    fn keyless_connection_string_with_client_id_is_user_assigned_mi() {
        let loc =
            parse_connection_string("AccountName=factoryprod;ManagedIdentityClientId=abc-123;")
                .unwrap();
        assert_eq!(
            loc.credential,
            BlobCredential::ManagedIdentity {
                client_id: Some("abc-123".to_string())
            }
        );
        assert_eq!(loc.endpoint, BlobEndpoint::Public);
    }

    #[test]
    fn use_development_storage_honors_an_explicit_blob_endpoint() {
        // A worker container needs host.docker.internal, NOT 127.0.0.1 — the explicit
        // BlobEndpoint must win even with the dev-storage shortcut.
        let loc = parse_connection_string(
            "UseDevelopmentStorage=true;BlobEndpoint=http://host.docker.internal:10000/devstoreaccount1;",
        )
        .unwrap();
        assert_eq!(loc.account, EMULATOR_ACCOUNT);
        assert_eq!(
            emulator(&loc),
            Some(("host.docker.internal".to_string(), 10000))
        );
    }

    #[test]
    fn real_azure_connection_string_has_no_emulator_endpoint() {
        let conn =
            "DefaultEndpointsProtocol=https;AccountName=factoryprod;AccountKey=c2VjcmV0a2V5==;\
            EndpointSuffix=core.windows.net";
        let loc = parse_connection_string(conn).unwrap();
        assert_eq!(loc.account, "factoryprod");
        assert_eq!(shared_key(&loc), "c2VjcmV0a2V5==");
        assert_eq!(
            loc.endpoint,
            BlobEndpoint::Public,
            "no BlobEndpoint ⇒ public cloud, not emulator"
        );
    }

    #[test]
    fn missing_account_is_an_error() {
        assert!(parse_connection_string("AccountKey=abc;").is_err());
    }

    #[test]
    fn malformed_segment_is_an_error() {
        assert!(parse_connection_string("AccountName=x;garbage;").is_err());
    }

    #[test]
    fn parse_host_port_defaults_by_scheme() {
        assert_eq!(
            parse_host_port("https://acct.blob.core.windows.net").unwrap(),
            ("acct.blob.core.windows.net".to_string(), 443)
        );
        assert_eq!(
            parse_host_port("http://127.0.0.1:10000/devstoreaccount1").unwrap(),
            ("127.0.0.1".to_string(), 10000)
        );
    }

    // ---- live round-trips (gated on Azurite, mirroring the DATABASE_URL lane) ----

    /// The Azurite connection string for the integration lane, or `None` to skip.
    fn azurite_conn() -> Option<String> {
        std::env::var("FACTORY_BLOB_CONNECTION_STRING")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }

    /// A unique container name per test so the lane is parallel-safe. A process-local
    /// monotonic counter (plus the PID) guarantees uniqueness even when two tests run
    /// in the same nanosecond — `SystemTime` resolution is platform-dependent and can
    /// repeat under parallel execution, which would collide container names.
    fn unique_container(label: &str) -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("itest-{label}-{}-{n}", std::process::id())
    }

    #[tokio::test]
    async fn connecting_twice_to_the_same_container_is_idempotent() {
        // ensure_container creates-then-tolerates-409; every production restart re-connects
        // to an ALREADY-existing container, so the tolerate path (not just first-create)
        // must work — the other tests use a fresh container each run and never hit it.
        let Some(conn) = azurite_conn() else {
            eprintln!("skipping: FACTORY_BLOB_CONNECTION_STRING unset (Azurite not available)");
            return;
        };
        let name = unique_container("idem");
        AzureBlobCheckpoints::connect(&conn, &name)
            .await
            .expect("first connect creates the container");
        // Second connect must tolerate the already-existing container (the 409 path).
        let store = AzureBlobCheckpoints::connect(&conn, &name)
            .await
            .expect("second connect must tolerate the already-exists container");
        // And a write/read still works through the re-connected store.
        store.write("k", b"v").await.unwrap();
        assert_eq!(store.read("k").await.unwrap(), Some(b"v".to_vec()));
    }

    #[tokio::test]
    async fn checkpoint_roundtrip_overwrite_and_missing() {
        let Some(conn) = azurite_conn() else {
            eprintln!("skipping: FACTORY_BLOB_CONNECTION_STRING unset (Azurite not available)");
            return;
        };
        let store = AzureBlobCheckpoints::connect(&conn, &unique_container("cp"))
            .await
            .unwrap();
        assert_eq!(
            store.read("a/cp.bin").await.unwrap(),
            None,
            "missing ⇒ None"
        );
        store.write("a/cp.bin", b"one").await.unwrap();
        store.write("a/cp.bin", b"two").await.unwrap();
        // Latest-wins overwrite, not append.
        assert_eq!(store.read("a/cp.bin").await.unwrap(), Some(b"two".to_vec()));
    }

    #[tokio::test]
    async fn transcript_appends_accumulate_and_missing_is_empty() {
        let Some(conn) = azurite_conn() else {
            eprintln!("skipping: FACTORY_BLOB_CONNECTION_STRING unset (Azurite not available)");
            return;
        };
        let store = AzureBlobTranscripts::connect(&conn, &unique_container("tr"))
            .await
            .unwrap();
        assert_eq!(store.read("run/1").await.unwrap(), "", "missing ⇒ empty");
        store.append("run/1", "hello ").await.unwrap();
        store.append("run/1", "world").await.unwrap();
        assert_eq!(store.read("run/1").await.unwrap(), "hello world");
        // A second uri is independent (lazy create per uri works more than once).
        assert_eq!(store.read("run/2").await.unwrap(), "");
        store.append("run/2", "x").await.unwrap();
        assert_eq!(store.read("run/2").await.unwrap(), "x");
    }
}

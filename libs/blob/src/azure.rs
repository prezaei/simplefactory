//! Azure Blob–backed stores (P7): the **production** [`CheckpointStore`] and
//! [`TranscriptStore`]. Exercised locally against **Azurite** (the Azure Blob
//! emulator in `docker-compose.yml`) and against real Azure Blob in P8 — the only
//! P8 delta is the credential/endpoint the client is constructed with, not the
//! read/write data path.
//!
//! Auth/endpoint come from a single Azure **connection string**
//! (`FACTORY_BLOB_CONNECTION_STRING`). Azurite uses the well-known dev account with
//! a `BlobEndpoint=` pointing at the emulator host:port (e.g.
//! `http://host.docker.internal:10000/devstoreaccount1` from inside a worker
//! container); real Azure uses `AccountName`/`AccountKey` with no `BlobEndpoint`.
//! P8 will swap shared-key for managed identity (`TokenCredential`) — the documented
//! credential fork, the only dev/prod divergence in this module. (P8 may also adopt
//! the SDK's own `ConnectionString` parser for sovereign-cloud `EndpointSuffix`/SAS;
//! P7's parser covers Azurite + public-cloud shared-key, which is all P7 exercises.)

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
    key: String,
    /// `Some((host, port))` ⇒ the emulator/custom endpoint (Azurite); `None` ⇒ the
    /// real Azure public cloud (`https://{account}.blob.core.windows.net`).
    emulator: Option<(String, u16)>,
}

/// Parse an Azure Storage connection string into the bits we need. Supports the
/// `UseDevelopmentStorage=true` shortcut, an explicit Azurite `BlobEndpoint=`, and a
/// real-Azure `AccountName`/`AccountKey` pair. Unknown segments (e.g.
/// `DefaultEndpointsProtocol`) are ignored.
fn parse_connection_string(conn: &str) -> anyhow::Result<BlobLocation> {
    let mut account: Option<String> = None;
    let mut key: Option<String> = None;
    let mut endpoint: Option<String> = None;
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
            "AccountKey" => key = Some(v.trim().to_string()),
            "BlobEndpoint" => endpoint = Some(v.trim().to_string()),
            "UseDevelopmentStorage" => use_dev = v.trim().eq_ignore_ascii_case("true"),
            _ => {} // ignore the rest (DefaultEndpointsProtocol, QueueEndpoint, …)
        }
    }

    // The emulator endpoint: an explicit BlobEndpoint always wins (so a worker
    // container can point at host.docker.internal), with `UseDevelopmentStorage=true`
    // supplying the default 127.0.0.1:10000 + dev credentials when no endpoint is set.
    let emulator = match &endpoint {
        Some(ep) => Some(parse_host_port(ep)?),
        None if use_dev => Some(("127.0.0.1".to_string(), 10000)),
        None => None,
    };

    if use_dev {
        return Ok(BlobLocation {
            account: account.unwrap_or_else(|| EMULATOR_ACCOUNT.to_string()),
            key: key.unwrap_or_else(|| EMULATOR_ACCOUNT_KEY.to_string()),
            emulator,
        });
    }

    Ok(BlobLocation {
        account: account.context("connection string missing AccountName")?,
        key: key.context("connection string missing AccountKey")?,
        emulator,
    })
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
    let credentials = StorageCredentials::access_key(loc.account.clone(), loc.key.clone());
    let cloud = match loc.emulator {
        Some((address, port)) => CloudLocation::Emulator { address, port },
        None => CloudLocation::Public {
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
/// `None`/empty read semantics the store traits promise. A 404 with a *different*
/// error code (e.g. `ContainerNotFound` — a misconfiguration, not "no checkpoint
/// yet") is NOT swallowed: it propagates so a real fault can't masquerade as an
/// empty/absent value (which would silently abandon a recoverable checkpoint).
fn is_blob_not_found(err: &AzureError) -> bool {
    match err.as_http_error() {
        Some(h) if h.status() == StatusCode::NotFound => {
            matches!(h.error_code(), Some("BlobNotFound") | None)
        }
        _ => false,
    }
}

/// True when an error is a create-lost-the-race (the blob already exists): a
/// conditional create (`If-None-Match: *`) returns 409/412 when another writer
/// created it first. Tolerated so a concurrent first-append can't error spuriously.
fn is_already_exists(err: &AzureError) -> bool {
    matches!(
        err.as_http_error().map(|h| h.status()),
        Some(StatusCode::Conflict | StatusCode::PreconditionFailed)
    )
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

    #[test]
    fn parses_azurite_connection_string_with_custom_blob_endpoint() {
        let conn = "DefaultEndpointsProtocol=http;AccountName=devstoreaccount1;\
            AccountKey=Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==;\
            BlobEndpoint=http://host.docker.internal:10000/devstoreaccount1;";
        let loc = parse_connection_string(conn).unwrap();
        assert_eq!(loc.account, "devstoreaccount1");
        assert!(
            loc.key.ends_with("GMGw=="),
            "the full base64 key (with '=') is kept"
        );
        assert_eq!(
            loc.emulator,
            Some(("host.docker.internal".to_string(), 10000))
        );
    }

    #[test]
    fn use_development_storage_shortcut_maps_to_local_emulator() {
        let loc = parse_connection_string("UseDevelopmentStorage=true;").unwrap();
        assert_eq!(loc.account, EMULATOR_ACCOUNT);
        assert_eq!(loc.key, EMULATOR_ACCOUNT_KEY);
        assert_eq!(loc.emulator, Some(("127.0.0.1".to_string(), 10000)));
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
            loc.emulator,
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
        assert_eq!(loc.key, "c2VjcmV0a2V5==");
        assert_eq!(
            loc.emulator, None,
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

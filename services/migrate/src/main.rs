//! One-shot database migration runner (P8).
//!
//! Run to completion as a dedicated Container Apps **Job** in CD *before* rolling the
//! service revisions, so the N service replicas never race to migrate on startup. The
//! services keep their idempotent startup-migrate as a belt-and-suspenders no-op; this
//! bin makes the single-runner migration step explicit (the spec's expand/contract
//! discipline). Reuses the tested [`factory_record::migrate`]; it applies all pending
//! migrations and exits 0 (or non-zero on failure so CD can halt the rollout).
#![forbid(unsafe_code)]

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    factory_telemetry::init("migrate");

    let database_url =
        std::env::var("DATABASE_URL").map_err(|_| anyhow::anyhow!("DATABASE_URL is required"))?;

    tracing::info!("migrate: connecting to the database");
    let pool = factory_record::connect(&database_url).await?;
    tracing::info!("migrate: applying pending migrations");
    factory_record::migrate(&pool).await?;
    tracing::info!("migrate: all migrations applied; exiting 0");
    Ok(())
}

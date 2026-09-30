//! Browser integration fixture. Runs the real router and simulation against an
//! isolated temporary PostgreSQL database; never used by the application.

use std::{env, error::Error, net::SocketAddr, path::PathBuf, sync::Arc};

use ldw_server::{
    access::AccessStore,
    blob_store::BlobStore,
    http::{AppState, router},
    migrate, simulation,
};
use sqlx::PgPool;
use tokio::sync::watch;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let base_url = env::var("LDW_UI_BASE_DATABASE_URL")?;
    let password = env::var("LDW_UI_FIXTURE_PASSWORD")?;
    let public_origin = env::var("LDW_UI_PUBLIC_ORIGIN")?;
    let bind_address: SocketAddr = env::var("LDW_UI_BIND_ADDR")?.parse()?;
    // The CI PostgreSQL service is disposable; a literal identifier also keeps SQLx's
    // dynamic-SQL protection active for this test fixture.
    const DATABASE_NAME: &str = "ldw_ui_fixture";
    let (prefix, _) = base_url
        .rsplit_once('/')
        .ok_or("database URL needs a database name")?;
    let database_url = format!("{prefix}/{DATABASE_NAME}");

    let admin_pool = PgPool::connect(&base_url).await?;
    sqlx::query("CREATE DATABASE ldw_ui_fixture")
        .execute(&admin_pool)
        .await?;
    admin_pool.close().await;

    let pool = PgPool::connect(&database_url).await?;
    migrate(&pool).await?;
    let mut pin_key = [0u8; 32];
    getrandom::fill(&mut pin_key).map_err(|_| "fixture random key failed")?;
    let access = AccessStore::new(pool.clone(), pin_key);
    access
        .bootstrap_admin("ui-fixture-admin", &password)
        .await?;
    let admin = access
        .login("ui-fixture-admin", &password, "fixture")
        .await?;
    access
        .create_owner(&admin.token, "ui-fixture-owner", &password)
        .await?;

    let listener = tokio::net::TcpListener::bind(bind_address).await?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let hub = simulation::SimulationHub::default();
    let simulation_task = tokio::spawn(simulation::run(pool, shutdown_rx, hub.clone()));
    let provided_blob_directory = env::var_os("LDW_UI_BLOB_DIR").map(PathBuf::from);
    let blob_directory = provided_blob_directory
        .clone()
        .unwrap_or_else(|| env::temp_dir().join(format!("ldw-ui-blobs-{}", uuid::Uuid::new_v4())));
    let app = router(AppState {
        access,
        blob_store: BlobStore::create(blob_directory.clone())?,
        public_origin: Arc::from(public_origin),
        simulation_hub: hub,
    });
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    let _ = shutdown_tx.send(true);
    simulation_task.await??;
    if provided_blob_directory.is_none() {
        std::fs::remove_dir_all(blob_directory)?;
    }
    Ok(())
}

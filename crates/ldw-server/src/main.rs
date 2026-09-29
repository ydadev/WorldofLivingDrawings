use std::{env, error::Error, fs, net::SocketAddr, path::PathBuf, sync::Arc};

use ldw_server::{
    access::AccessStore,
    blob_gc,
    blob_store::BlobStore,
    http::{AppState, router},
    migrate, simulation,
};
use sqlx::PgPool;
use tokio::sync::watch;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args().skip(1);
    let command = arguments
        .next()
        .ok_or("expected serve, bootstrap-admin or gc-blobs")?;
    let database_url_file = PathBuf::from(env::var("LDW_DATABASE_URL_FILE")?);
    let database_url = fs::read_to_string(database_url_file)?;
    let pool = PgPool::connect(database_url.trim()).await?;
    migrate(&pool).await?;

    if command == "gc-blobs" {
        if arguments.next().is_some() {
            return Err("unexpected argument".into());
        }
        let store = BlobStore::new(PathBuf::from(env::var("LDW_BLOB_DIR")?))?;
        let outcome = blob_gc::collect(&pool, &store).await?;
        println!(
            "Blob GC completed: {} files, {} catalog rows",
            outcome.files_deleted, outcome.catalog_rows_deleted
        );
        return Ok(());
    }

    let key_file = PathBuf::from(env::var("LDW_PIN_KEY_FILE")?);
    let key_bytes = fs::read(key_file)?;
    let pin_key: [u8; 32] = key_bytes
        .try_into()
        .map_err(|_| "PIN key must be 32 bytes")?;
    let simulation_pool = pool.clone();
    let access = AccessStore::new(pool, pin_key);

    match command.as_str() {
        "bootstrap-admin" => {
            let login = arguments.next().ok_or("expected admin login")?;
            if arguments.next().is_some() {
                return Err("password must not be a CLI argument".into());
            }
            let password = rpassword::prompt_password("New Admin password: ")?;
            let confirm = rpassword::prompt_password("Repeat password: ")?;
            if password != confirm {
                return Err("passwords do not match".into());
            }
            access.bootstrap_admin(&login, &password).await?;
            println!("Admin account created");
        }
        "serve" => {
            if arguments.next().is_some() {
                return Err("unexpected argument".into());
            }
            let origin = env::var("LDW_PUBLIC_ORIGIN")?;
            let parsed: axum::http::Uri = origin.parse()?;
            if parsed.scheme_str() != Some("https")
                || parsed.authority().is_none()
                || parsed.path() != "/"
                || parsed.query().is_some()
                || origin.ends_with('/')
            {
                return Err("LDW_PUBLIC_ORIGIN must be a bare HTTPS origin".into());
            }
            let address: SocketAddr = env::var("LDW_BIND_ADDR")?.parse()?;
            let blob_store = BlobStore::new(PathBuf::from(env::var("LDW_BLOB_DIR")?))?;
            let listener = tokio::net::TcpListener::bind(address).await?;
            access.bump_active_epochs().await?;
            let (shutdown_tx, shutdown_rx) = watch::channel(false);
            let hub = simulation::SimulationHub::default();
            let simulation_task =
                tokio::spawn(simulation::run(simulation_pool, shutdown_rx, hub.clone()));
            let app = router(AppState {
                access,
                blob_store,
                public_origin: Arc::from(origin),
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
        }
        _ => return Err("expected serve, bootstrap-admin or gc-blobs".into()),
    }
    Ok(())
}

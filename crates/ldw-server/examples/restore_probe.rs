//! Verify that a restored scene still authorizes its Paint Textures and runs.

use std::{env, error::Error, path::PathBuf, sync::Arc};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use ldw_server::{
    access::AccessStore,
    blob_store::BlobStore,
    http::{AppState, router},
    simulation::{self, SimulationHub},
};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let database_url = env::var("LDW_RESTORE_DATABASE_URL")?;
    let blob_directory = PathBuf::from(env::var("LDW_RESTORE_BLOB_DIR")?);
    let session_id = Uuid::parse_str(&env::var("LDW_RESTORE_SESSION_ID")?)?;
    let blob_ids = [
        env::var("LDW_RESTORE_CORAL_BLOB")?,
        env::var("LDW_RESTORE_STREAM_BLOB")?,
    ];
    let password = env::var("LDW_RESTORE_OWNER_PASSWORD")?;
    let pool = PgPool::connect(&database_url).await?;
    let store = BlobStore::new(blob_directory)?;
    let access = AccessStore::new(pool.clone(), [0; 32]);
    let owner = access.login("ui-fixture-owner", &password).await?;
    let summary = access.owner_scene(&owner.token, session_id).await?;
    assert_eq!(summary.world_id, "underwater");
    let mut loaded = simulation::load_scene(&pool, summary.scene_id).await?;
    assert_eq!(loaded.world.fish().len(), 2);
    let previous_tick = loaded.world.tick_number();
    for _ in 0..100 {
        loaded.world.step();
    }
    assert_eq!(loaded.world.tick_number(), previous_tick + 100);
    assert!(simulation::save_checkpoint(&pool, summary.scene_id, &mut loaded).await?);
    let continued = simulation::load_scene(&pool, summary.scene_id).await?;
    assert_eq!(continued.world.tick_number(), previous_tick + 100);
    assert_eq!(continued.world.fish().len(), 2);

    let app = router(AppState {
        access,
        blob_store: store,
        public_origin: Arc::from("https://127.0.0.1:9443"),
        simulation_hub: SimulationHub::default(),
    });
    let cookie = format!("__Host-ldw-owner={}", owner.token);
    let scene_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/sessions/{session_id}/scene"))
                .header(header::COOKIE, cookie.as_str())
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(scene_response.status(), StatusCode::OK);
    let scene_bytes = to_bytes(scene_response.into_body(), 4096).await?;
    let scene: serde_json::Value = serde_json::from_slice(&scene_bytes)?;
    assert_eq!(scene["sceneId"], summary.scene_id.to_string());

    for id in &blob_ids {
        let uri = format!("/api/sessions/{session_id}/paint/{id}");
        let denied = app
            .clone()
            .oneshot(Request::builder().uri(uri.as_str()).body(Body::empty())?)
            .await?;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri.as_str())
                    .header(header::COOKIE, cookie.as_str())
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
        let png = to_bytes(response.into_body(), 3 * 1024 * 1024).await?;
        assert_eq!(BlobStore::id_for_normalized(&png)?, id.as_str());
    }
    pool.close().await;
    println!("Restored scene, private textures, and continued checkpoint: PASS");
    Ok(())
}

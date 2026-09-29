//! Durable, authenticated reservation for a finished drawing. Pixel upload and
//! finalization are separate steps; creating this row never publishes a fish.

use ldw_sim::Point;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    access::{GrantKind, SceneAccess},
    simulation,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadIntentRequest {
    pub scene_epoch: i64,
    pub definition_id: String,
    pub template_id: String,
    pub template_version: i32,
    pub layout_hash: String,
    pub source_kind: String,
    pub color_space: String,
    pub position: Point,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadIntentResponse {
    pub intent_id: Uuid,
    pub expires_in_seconds: u32,
    pub reservation_seconds: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("access denied")]
    Forbidden,
    #[error("unsupported PaintResult or fish position")]
    InvalidPaint,
    #[error("scene changed or stopped")]
    StaleScene,
    #[error("scene has 100 drawings")]
    SceneFull,
    #[error("too many drawing uploads")]
    IntentLimit,
    #[error("invalid persisted scene state")]
    InvalidScene,
    #[error("database operation failed: {0}")]
    Database(#[from] sqlx::Error),
}

fn valid_template(input: &UploadIntentRequest) -> bool {
    let layout = match (input.definition_id.as_str(), input.template_id.as_str()) {
        ("coral-fish", "coral") => {
            include_str!("../../../content/underwater/assets/coral.layout.json")
        }
        ("stream-fish", "stream") => {
            include_str!("../../../content/underwater/assets/stream.layout.json")
        }
        _ => return false,
    };
    let Ok(layout) = serde_json::from_str::<Value>(layout) else {
        return false;
    };
    input.template_version == layout["templateVersion"].as_i64().unwrap_or_default() as i32
        && input.layout_hash == layout["contentHash"].as_str().unwrap_or_default()
        && matches!(input.source_kind.as_str(), "paper" | "browser")
        && input.color_space == "sRGB"
}

/// The caller first validates the cookie, Origin and CSRF, then supplies the
/// freshly resolved SceneAccess. We recheck the grant inside the transaction.
pub async fn create_upload_intent(
    pool: &PgPool,
    kind: GrantKind,
    access: &SceneAccess,
    input: &UploadIntentRequest,
) -> Result<UploadIntentResponse, UploadError> {
    if kind == GrantKind::Viewer {
        return Err(UploadError::Forbidden);
    }
    if !valid_template(input)
        || access.scene.world_id != "underwater"
        || access.scene.world_version != 1
    {
        return Err(UploadError::InvalidPaint);
    }
    if input.scene_epoch != access.scene.scene_epoch {
        return Err(UploadError::StaleScene);
    }
    let intent_id = Uuid::new_v4();
    let fish_id = Uuid::new_v4();
    let mut probe =
        simulation::initial_world(access.scene.scene_id).map_err(|_| UploadError::InvalidScene)?;
    probe
        .spawn_fish(fish_id.as_u128(), input.position, 1.2)
        .map_err(|_| UploadError::InvalidPaint)?;

    let mut tx = pool.begin().await?;
    // Every competing publication or reservation locks the scene; the session
    // lock also serializes the ten-intent quota when scenes change.
    let row: Option<(i64, String, i32, Value, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT c.scene_epoch, c.world_id, c.world_version, c.state, s.status, s.active_scene_id \
         FROM sessions s JOIN scenes c ON c.session_id = s.id \
         WHERE s.id = $1 AND c.id = $2 FOR UPDATE OF s, c",
    )
    .bind(access.scene.session_id)
    .bind(access.scene.scene_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((epoch, world_id, version, state, status, active_scene)) = row else {
        return Err(UploadError::StaleScene);
    };
    if epoch != input.scene_epoch
        || world_id != "underwater"
        || version != 1
        || status != "running"
        || active_scene != Some(access.scene.scene_id)
    {
        return Err(UploadError::StaleScene);
    }
    let principal: Option<Uuid> = match kind {
        GrantKind::Owner => {
            sqlx::query_scalar(
                "SELECT account_id FROM owner_grants \
             WHERE id = $1 AND revoked_at IS NULL AND expires_at > now()",
            )
            .bind(access.grant_id)
            .fetch_optional(&mut *tx)
            .await?
        }
        GrantKind::Controller => {
            sqlx::query_scalar(
                "SELECT participant_id FROM device_grants \
             WHERE id = $1 AND session_id = $2 AND role = 'controller' \
             AND revoked_at IS NULL AND expires_at > now() \
             AND last_activity_at > now() - interval '2 hours'",
            )
            .bind(access.grant_id)
            .bind(access.scene.session_id)
            .fetch_optional(&mut *tx)
            .await?
        }
        GrantKind::Viewer => None,
    };
    let principal = principal.ok_or(UploadError::Forbidden)?;
    let entity_count = match state.get("entities") {
        None => 0,
        Some(Value::Array(items)) => items.len(),
        _ => return Err(UploadError::InvalidScene),
    };
    let pending: i64 =
        sqlx::query_scalar("SELECT count(*) FROM fish_publications WHERE scene_id = $1")
            .bind(access.scene.scene_id)
            .fetch_one(&mut *tx)
            .await?;
    let (reserved, session_intents, principal_intents): (i64, i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE scene_id = $1), count(*), \
         count(*) FILTER (WHERE principal_kind = 'controller' AND principal_id = $3) \
         FROM upload_intents WHERE session_id = $2 AND status <> 'finalized' \
         AND reservation_until > now()",
    )
    .bind(access.scene.scene_id)
    .bind(access.scene.session_id)
    .bind(principal)
    .fetch_one(&mut *tx)
    .await?;
    if entity_count + pending as usize + reserved as usize >= ldw_sim::MAX_FISH {
        return Err(UploadError::SceneFull);
    }
    if session_intents >= 10 || (kind == GrantKind::Controller && principal_intents >= 1) {
        return Err(UploadError::IntentLimit);
    }
    sqlx::query(
        "INSERT INTO upload_intents \
         (id, session_id, scene_id, scene_epoch, principal_kind, principal_id, fish_id, \
          definition_id, template_id, template_version, layout_hash, source_kind, \
          position_x, position_y, reservation_until, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, \
                 now() + interval '2 minutes', now() + interval '10 minutes')",
    )
    .bind(intent_id)
    .bind(access.scene.session_id)
    .bind(access.scene.scene_id)
    .bind(epoch)
    .bind(if kind == GrantKind::Owner {
        "owner"
    } else {
        "controller"
    })
    .bind(principal)
    .bind(fish_id)
    .bind(&input.definition_id)
    .bind(&input.template_id)
    .bind(input.template_version)
    .bind(&input.layout_hash)
    .bind(&input.source_kind)
    .bind(input.position.x)
    .bind(input.position.y)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(UploadIntentResponse {
        intent_id,
        expires_in_seconds: 600,
        reservation_seconds: 120,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use argon2::{Argon2, password_hash::PasswordHasher};
    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use tower::ServiceExt;

    use super::*;
    use crate::{
        access::{AccessStore, PairCode},
        http::{AppState, router},
    };

    fn request(epoch: i64) -> UploadIntentRequest {
        let layout: Value = serde_json::from_str(include_str!(
            "../../../content/underwater/assets/coral.layout.json"
        ))
        .unwrap();
        UploadIntentRequest {
            scene_epoch: epoch,
            definition_id: "coral-fish".into(),
            template_id: "coral".into(),
            template_version: 1,
            layout_hash: layout["contentHash"].as_str().unwrap().into(),
            source_kind: "browser".into(),
            color_space: "sRGB".into(),
            position: Point { x: 0.0, y: 0.0 },
        }
    }

    #[tokio::test]
    async fn upload_intent_is_private_versioned_and_reserves_the_last_session_slot() {
        let pool = crate::test_pool().await;
        let store = AccessStore::new(pool.clone(), [29u8; 32]);
        let owner_id = Uuid::new_v4();
        let login = format!("upload-{}", owner_id);
        let password = format!("upload-password-{}", owner_id);
        let hash = Argon2::default()
            .hash_password(password.as_bytes())
            .unwrap()
            .to_string();
        sqlx::query(
            "INSERT INTO accounts (id, login, role, password_hash) VALUES ($1, $2, 'owner', $3)",
        )
        .bind(owner_id)
        .bind(&login)
        .bind(hash)
        .execute(&pool)
        .await
        .unwrap();
        let owner = store.login(&login, &password).await.unwrap();
        let scene = store
            .create_session(&owner.token, &owner.csrf)
            .await
            .unwrap();
        let access = store
            .scene_access(GrantKind::Owner, &owner.token, scene.session_id)
            .await
            .unwrap();
        let payload = request(access.scene.scene_epoch);
        let url = format!("/api/sessions/{}/upload-intents", scene.session_id);
        let app = router(AppState {
            access: store.clone(),
            public_origin: Arc::from("https://example.test"),
            simulation_hub: simulation::SimulationHub::default(),
        });
        let http_request = |origin: &str, csrf: &str, cookie: &str| {
            Request::builder()
                .method("POST")
                .uri(&url)
                .header(header::ORIGIN, origin)
                .header("x-csrf-token", csrf)
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap()
        };
        let owner_cookie = format!("__Host-ldw-owner={}", owner.token);
        let rejected = app
            .clone()
            .oneshot(http_request(
                "https://wrong.test",
                &owner.csrf,
                &owner_cookie,
            ))
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
        let rejected = app
            .clone()
            .oneshot(http_request(
                "https://example.test",
                "wrong-csrf",
                &owner_cookie,
            ))
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
        let created = app
            .clone()
            .oneshot(http_request(
                "https://example.test",
                &owner.csrf,
                &owner_cookie,
            ))
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let persisted: i64 =
            sqlx::query_scalar("SELECT count(*) FROM upload_intents WHERE scene_id = $1")
                .bind(scene.scene_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(persisted, 1);
        let entities: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
            .bind(scene.scene_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(
            entities.get("entities").is_none(),
            "intent is not a fish yet"
        );

        let viewer = store
            .create_viewer(&owner.token, &owner.csrf, scene.session_id, true)
            .await
            .unwrap();
        let viewer_cookie = format!("__Host-ldw-viewer={}", viewer.token);
        let rejected = app
            .clone()
            .oneshot(http_request(
                "https://example.test",
                &viewer.csrf,
                &viewer_cookie,
            ))
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
        let other_id = Uuid::new_v4();
        let other_login = format!("upload-{}", other_id);
        let other_password = format!("upload-password-{}", other_id);
        let other_hash = Argon2::default()
            .hash_password(other_password.as_bytes())
            .unwrap()
            .to_string();
        sqlx::query(
            "INSERT INTO accounts (id, login, role, password_hash) VALUES ($1, $2, 'owner', $3)",
        )
        .bind(other_id)
        .bind(&other_login)
        .bind(other_hash)
        .execute(&pool)
        .await
        .unwrap();
        let other = store.login(&other_login, &other_password).await.unwrap();
        let other_cookie = format!("__Host-ldw-owner={}", other.token);
        let rejected = app
            .clone()
            .oneshot(http_request(
                "https://example.test",
                &other.csrf,
                &other_cookie,
            ))
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
        let viewer_access = store
            .scene_access(GrantKind::Viewer, &viewer.token, scene.session_id)
            .await
            .unwrap();
        assert!(matches!(
            create_upload_intent(&pool, GrantKind::Viewer, &viewer_access, &payload).await,
            Err(UploadError::Forbidden)
        ));

        let mut stale = payload.clone();
        stale.scene_epoch += 1;
        assert!(matches!(
            create_upload_intent(&pool, GrantKind::Owner, &access, &stale).await,
            Err(UploadError::StaleScene)
        ));
        let mut bad = payload.clone();
        bad.layout_hash = "0".repeat(64);
        assert!(matches!(
            create_upload_intent(&pool, GrantKind::Owner, &access, &bad).await,
            Err(UploadError::InvalidPaint)
        ));
        let mut bad = payload.clone();
        bad.position.x = 20.0;
        assert!(matches!(
            create_upload_intent(&pool, GrantKind::Owner, &access, &bad).await,
            Err(UploadError::InvalidPaint)
        ));

        let invitation = store
            .open_invitation(&owner.token, &owner.csrf, scene.session_id)
            .await
            .unwrap();
        let controller = store
            .pair_controller(
                scene.session_id,
                &Uuid::new_v4().to_string(),
                "127.0.0.1",
                PairCode::Pin(&invitation.pin),
            )
            .await
            .unwrap();
        let controller_access = store
            .scene_access(GrantKind::Controller, &controller.token, scene.session_id)
            .await
            .unwrap();
        create_upload_intent(&pool, GrantKind::Controller, &controller_access, &payload)
            .await
            .unwrap();
        assert!(matches!(
            create_upload_intent(&pool, GrantKind::Controller, &controller_access, &payload).await,
            Err(UploadError::IntentLimit)
        ));

        for _ in 0..7 {
            create_upload_intent(&pool, GrantKind::Owner, &access, &payload)
                .await
                .unwrap();
        }
        let (left, right) = tokio::join!(
            create_upload_intent(&pool, GrantKind::Owner, &access, &payload),
            create_upload_intent(&pool, GrantKind::Owner, &access, &payload),
        );
        assert_eq!(left.is_ok() as u8 + right.is_ok() as u8, 1);
        let reserved: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM upload_intents WHERE scene_id = $1 AND reservation_until > now()",
        )
        .bind(scene.scene_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(reserved, 10);
        sqlx::query(
            "UPDATE upload_intents SET reservation_until = now() - interval '1 second' \
             WHERE id = (SELECT id FROM upload_intents WHERE scene_id = $1 ORDER BY created_at LIMIT 1)",
        )
        .bind(scene.scene_id)
        .execute(&pool)
        .await
        .unwrap();
        create_upload_intent(&pool, GrantKind::Owner, &access, &payload)
            .await
            .expect("expired reservation frees a scene slot");
    }
}

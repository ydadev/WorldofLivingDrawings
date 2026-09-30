//! Durable admission for removing and restoring published fish. The scene
//! worker applies accepted operations together with its current checkpoint.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::access::{GrantKind, SceneAccess};

#[derive(Clone, Copy, Debug)]
pub enum FishOperation {
    Delete,
    Restore,
}

impl FishOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Delete => "delete",
            Self::Restore => "restore",
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FishMutationRequest {
    pub command_id: Uuid,
    pub scene_epoch: i64,
    pub expires_at: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashedFish {
    pub fish_id: Uuid,
    pub entity: Value,
    pub deleted_at: String,
    pub expires_at: String,
}

#[derive(Debug, thiserror::Error)]
pub enum FishTrashError {
    #[error("access denied")]
    Forbidden,
    #[error("invalid persisted scene state")]
    InvalidScene,
    #[error("database operation failed: {0}")]
    Database(#[from] sqlx::Error),
}

async fn principal_id(
    tx: &mut Transaction<'_, Postgres>,
    kind: GrantKind,
    access: &SceneAccess,
) -> Result<Uuid, FishTrashError> {
    let principal = match kind {
        GrantKind::Owner => {
            sqlx::query_scalar(
                "SELECT g.account_id FROM owner_grants g JOIN accounts a ON a.id = g.account_id \
                 WHERE g.id = $1 AND g.revoked_at IS NULL AND g.expires_at > now() \
                 AND a.disabled_at IS NULL",
            )
            .bind(access.grant_id)
            .fetch_optional(&mut **tx)
            .await?
        }
        GrantKind::Controller => {
            sqlx::query_scalar(
                "SELECT participant_id FROM device_grants WHERE id = $1 AND session_id = $2 \
                 AND role = 'controller' AND revoked_at IS NULL AND expires_at > now() \
                 AND last_activity_at > now() - interval '2 hours'",
            )
            .bind(access.grant_id)
            .bind(access.scene.session_id)
            .fetch_optional(&mut **tx)
            .await?
        }
        GrantKind::Viewer => None,
    };
    principal.ok_or(FishTrashError::Forbidden)
}

fn entity_id(fish_id: Uuid) -> String {
    format!("fish-{:032x}", fish_id.as_u128())
}

/// A known command outcome is returned before checking the current epoch or
/// expiry. A new request cannot reuse an ID with a different path or body.
pub async fn submit(
    pool: &PgPool,
    kind: GrantKind,
    access: &SceneAccess,
    fish_id: Uuid,
    operation: FishOperation,
    input: &FishMutationRequest,
) -> Result<Value, FishTrashError> {
    if kind == GrantKind::Viewer {
        return Err(FishTrashError::Forbidden);
    }
    let body_hash: [u8; 32] = Sha256::digest(
        serde_json::to_vec(&json!({
            "fishId":fish_id, "operation":operation.as_str(), "request":input
        }))
        .map_err(|_| FishTrashError::InvalidScene)?,
    )
    .into();
    let mut tx = pool.begin().await?;
    let scene: Option<(i64, i64, Value, String)> = sqlx::query_as(
        "SELECT c.scene_epoch, c.revision, c.state, s.status FROM sessions s \
         JOIN scenes c ON c.id = s.active_scene_id WHERE s.id = $1 AND c.id = $2 \
         FOR UPDATE OF s, c",
    )
    .bind(access.scene.session_id)
    .bind(access.scene.scene_id)
    .fetch_optional(&mut *tx)
    .await?;
    let (epoch, revision, state, status) = scene.ok_or(FishTrashError::Forbidden)?;
    let principal = principal_id(&mut tx, kind, access).await?;
    let previous: Option<(Vec<u8>, Value)> = sqlx::query_as(
        "SELECT body_hash, outcome FROM scene_commands \
         WHERE session_id = $1 AND grant_id = $2 AND command_id = $3",
    )
    .bind(access.scene.session_id)
    .bind(access.grant_id)
    .bind(input.command_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((hash, outcome)) = previous {
        return Ok(if hash == body_hash {
            outcome
        } else {
            json!({"type":"error", "code":"COMMAND_CONFLICT", "commandId":input.command_id})
        });
    }
    let now_ms: i64 =
        sqlx::query_scalar("SELECT (extract(epoch FROM clock_timestamp()) * 1000)::bigint")
            .fetch_one(&mut *tx)
            .await?;
    let busy: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM fish_mutations WHERE scene_id = $1 AND fish_id = $2)",
    )
    .bind(access.scene.scene_id)
    .bind(fish_id)
    .fetch_one(&mut *tx)
    .await?;
    let mut code = if input.command_id.is_nil() || fish_id.is_nil() {
        Some("INVALID_COMMAND")
    } else if input.scene_epoch != epoch {
        Some("STALE_SCENE")
    } else if input.expires_at < now_ms || input.expires_at > now_ms + 60_000 {
        Some("EXPIRED_COMMAND")
    } else if status != "running" && status != "paused" {
        Some("SCENE_CLOSED")
    } else if busy {
        Some("FISH_BUSY")
    } else {
        None
    };
    if code.is_none() {
        let author: Option<(String, Uuid)> = sqlx::query_as(
            "SELECT principal_kind, principal_id FROM upload_intents \
             WHERE scene_id = $1 AND fish_id = $2 AND status = 'finalized'",
        )
        .bind(access.scene.scene_id)
        .bind(fish_id)
        .fetch_optional(&mut *tx)
        .await?;
        code = match author {
            None => Some("FISH_NOT_FOUND"),
            Some((author_kind, author_id))
                if kind == GrantKind::Controller
                    && (author_kind != "controller" || author_id != principal) =>
            {
                Some("NOT_AUTHOR")
            }
            _ => None,
        };
    }
    if code.is_none() {
        code = match operation {
            FishOperation::Delete => {
                let entities = state
                    .get("entities")
                    .and_then(Value::as_array)
                    .ok_or(FishTrashError::InvalidScene)?;
                (!entities.iter().any(|entity| {
                    entity.get("id").and_then(Value::as_str) == Some(entity_id(fish_id).as_str())
                }))
                .then_some("FISH_NOT_ACTIVE")
            }
            FishOperation::Restore => {
                let live: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM fish_trash WHERE scene_id = $1 AND fish_id = $2 \
                     AND expires_at > now())",
                )
                .bind(access.scene.scene_id)
                .bind(fish_id)
                .fetch_one(&mut *tx)
                .await?;
                if !live {
                    Some("FISH_NOT_IN_TRASH")
                } else {
                    let queued: i64 = sqlx::query_scalar(
                        "SELECT count(*) FROM fish_publications WHERE scene_id = $1",
                    )
                    .bind(access.scene.scene_id)
                    .fetch_one(&mut *tx)
                    .await?;
                    let reserved: i64 = sqlx::query_scalar(
                        "SELECT count(*) FROM upload_intents WHERE scene_id = $1 \
                         AND status <> 'finalized' AND reservation_until > now()",
                    )
                    .bind(access.scene.scene_id)
                    .fetch_one(&mut *tx)
                    .await?;
                    let returning: i64 = sqlx::query_scalar(
                        "SELECT count(*) FROM fish_mutations WHERE scene_id = $1 AND operation = 'restore'",
                    )
                    .bind(access.scene.scene_id)
                    .fetch_one(&mut *tx)
                    .await?;
                    let active = state
                        .get("entities")
                        .and_then(Value::as_array)
                        .ok_or(FishTrashError::InvalidScene)?
                        .len() as i64;
                    (active + queued + reserved + returning >= ldw_sim::MAX_FISH as i64)
                        .then_some("SCENE_FULL")
                }
            }
        };
    }
    let outcome = json!({"type":"ack", "commandId":input.command_id,
        "accepted":code.is_none(), "code":code.unwrap_or("ACCEPTED"),
        "sceneId":access.scene.scene_id, "sceneEpoch":epoch, "revision":revision});
    if code.is_none() {
        sqlx::query(
            "INSERT INTO fish_mutations (scene_id, fish_id, command_id, operation) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(access.scene.scene_id)
        .bind(fish_id)
        .bind(input.command_id)
        .bind(operation.as_str())
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "INSERT INTO scene_commands (session_id, grant_id, command_id, body_hash, outcome) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(access.scene.session_id)
    .bind(access.grant_id)
    .bind(input.command_id)
    .bind(body_hash.to_vec())
    .bind(&outcome)
    .execute(&mut *tx)
    .await?;
    if kind == GrantKind::Controller {
        sqlx::query("UPDATE device_grants SET last_activity_at = now() WHERE id = $1")
            .bind(access.grant_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(outcome)
}

pub async fn list(
    pool: &PgPool,
    kind: GrantKind,
    access: &SceneAccess,
) -> Result<Vec<TrashedFish>, FishTrashError> {
    if kind == GrantKind::Viewer {
        return Err(FishTrashError::Forbidden);
    }
    let mut tx = pool.begin().await?;
    let principal = principal_id(&mut tx, kind, access).await?;
    let rows: Vec<(Uuid, Value, String, String)> = sqlx::query_as(
        "SELECT t.fish_id, t.entity, \
         to_char(t.deleted_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"'), \
         to_char(t.expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') \
         FROM fish_trash t JOIN upload_intents i ON i.scene_id = t.scene_id AND i.fish_id = t.fish_id \
         WHERE t.scene_id = $1 AND t.expires_at > now() \
         AND ($2 = 'owner' OR (i.principal_kind = 'controller' AND i.principal_id = $3)) \
         ORDER BY t.deleted_at DESC LIMIT 100",
    )
    .bind(access.scene.scene_id)
    .bind(if kind == GrantKind::Owner {
        "owner"
    } else {
        "controller"
    })
    .bind(principal)
    .fetch_all(&mut *tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(fish_id, entity, deleted_at, expires_at)| TrashedFish {
            fish_id,
            entity,
            deleted_at,
            expires_at,
        })
        .collect())
}

pub async fn paint_blob(
    pool: &PgPool,
    kind: GrantKind,
    access: &SceneAccess,
    fish_id: Uuid,
) -> Result<Option<String>, FishTrashError> {
    if kind == GrantKind::Viewer {
        return Err(FishTrashError::Forbidden);
    }
    let mut tx = pool.begin().await?;
    let principal = principal_id(&mut tx, kind, access).await?;
    let blob = sqlx::query_scalar(
        "SELECT t.paint_blob_id FROM fish_trash t \
         JOIN upload_intents i ON i.scene_id = t.scene_id AND i.fish_id = t.fish_id \
         WHERE t.scene_id = $1 AND t.fish_id = $2 AND t.expires_at > now() \
         AND ($3 = 'owner' OR (i.principal_kind = 'controller' AND i.principal_id = $4))",
    )
    .bind(access.scene.scene_id)
    .bind(fish_id)
    .bind(if kind == GrantKind::Owner {
        "owner"
    } else {
        "controller"
    })
    .bind(principal)
    .fetch_optional(&mut *tx)
    .await?;
    Ok(blob)
}

#[cfg(test)]
mod tests {
    use argon2::{Argon2, password_hash::PasswordHasher};
    use ldw_sim::Point;

    use super::*;
    use crate::{
        access::{AccessStore, PairCode},
        simulation,
    };

    #[tokio::test]
    async fn controller_delete_restore_is_durable_private_and_idempotent() {
        let pool = crate::test_pool().await;
        let store = AccessStore::new(pool.clone(), [41; 32]);
        let owner_id = Uuid::new_v4();
        let login = format!("trash-{owner_id}");
        let password = Uuid::new_v4().to_string();
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
        let owner = store.login(&login, &password, "test-peer").await.unwrap();
        let scene = store
            .create_session(&owner.token, &owner.csrf)
            .await
            .unwrap();
        let invitation = store
            .open_invitation(&owner.token, &owner.csrf, scene.session_id)
            .await
            .unwrap();
        let author = store
            .pair_controller(
                scene.session_id,
                "trash-author",
                "test-peer",
                PairCode::Pin(&invitation.pin),
            )
            .await
            .unwrap();
        let other = store
            .pair_controller(
                scene.session_id,
                "trash-other",
                "test-peer",
                PairCode::Pin(&invitation.pin),
            )
            .await
            .unwrap();
        let fish_id = Uuid::new_v4();
        let blob_id = "a".repeat(64);
        sqlx::query("INSERT INTO paint_blobs (id, byte_size) VALUES ($1, 128) ON CONFLICT DO NOTHING")
            .bind(&blob_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO scene_paint_blobs (scene_id, blob_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(scene.scene_id)
        .bind(&blob_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO upload_intents (id, session_id, scene_id, scene_epoch, principal_kind, \
             principal_id, fish_id, definition_id, template_id, template_version, layout_hash, \
             source_kind, position_x, position_y, status, paint_blob_id, result_entity_id, \
             reservation_until, expires_at) VALUES \
             ($1, $2, $3, 1, 'controller', $4, $5, 'coral-fish', 'coral', 1, $6, \
              'browser', 0, 0, 'finalized', $7, $5, now() + interval '1 hour', \
              now() + interval '1 hour')",
        )
        .bind(Uuid::new_v4())
        .bind(scene.session_id)
        .bind(scene.scene_id)
        .bind(author.participant_id)
        .bind(fish_id)
        .bind("b".repeat(64))
        .bind(&blob_id)
        .execute(&pool)
        .await
        .unwrap();
        simulation::publish_first_fish(
            &pool,
            scene.scene_id,
            fish_id,
            "coral-fish",
            &blob_id,
            Point { x: 0.0, y: 0.0 },
        )
        .await
        .unwrap();
        let author_access = store
            .scene_access(GrantKind::Controller, &author.token, scene.session_id)
            .await
            .unwrap();
        let other_access = store
            .scene_access(GrantKind::Controller, &other.token, scene.session_id)
            .await
            .unwrap();
        let owner_access = store
            .scene_access(GrantKind::Owner, &owner.token, scene.session_id)
            .await
            .unwrap();
        let now_ms: i64 =
            sqlx::query_scalar("SELECT (extract(epoch FROM clock_timestamp()) * 1000)::bigint")
                .fetch_one(&pool)
                .await
                .unwrap();
        let delete = FishMutationRequest {
            command_id: Uuid::new_v4(),
            scene_epoch: author_access.scene.scene_epoch,
            expires_at: now_ms + 30_000,
        };
        let denied = submit(
            &pool,
            GrantKind::Controller,
            &other_access,
            fish_id,
            FishOperation::Delete,
            &delete,
        )
        .await
        .unwrap();
        assert_eq!(denied["code"], "NOT_AUTHOR");
        let accepted = submit(
            &pool,
            GrantKind::Controller,
            &author_access,
            fish_id,
            FishOperation::Delete,
            &delete,
        )
        .await
        .unwrap();
        assert_eq!(accepted["accepted"], true);
        assert_eq!(
            submit(
                &pool,
                GrantKind::Controller,
                &author_access,
                fish_id,
                FishOperation::Delete,
                &delete,
            )
            .await
            .unwrap(),
            accepted
        );
        assert_eq!(
            submit(
                &pool,
                GrantKind::Controller,
                &author_access,
                fish_id,
                FishOperation::Restore,
                &delete,
            )
            .await
            .unwrap()["code"],
            "COMMAND_CONFLICT"
        );
        let mut loaded = simulation::load_scene(&pool, scene.scene_id).await.unwrap();
        assert!(simulation::apply_pending_fish_mutations(&pool, scene.scene_id, &mut loaded)
            .await
            .unwrap());
        assert!(loaded.world.fish().is_empty());
        assert!(simulation::load_scene(&pool, scene.scene_id)
            .await
            .unwrap()
            .world
            .fish()
            .is_empty());
        assert!(list(&pool, GrantKind::Controller, &other_access)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(list(&pool, GrantKind::Controller, &author_access)
            .await
            .unwrap()
            .len(), 1);
        assert_eq!(
            paint_blob(&pool, GrantKind::Controller, &other_access, fish_id)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            paint_blob(&pool, GrantKind::Owner, &owner_access, fish_id)
                .await
                .unwrap(),
            Some(blob_id)
        );
        let restore = FishMutationRequest {
            command_id: Uuid::new_v4(),
            scene_epoch: author_access.scene.scene_epoch,
            expires_at: now_ms + 30_000,
        };
        assert_eq!(
            submit(
                &pool,
                GrantKind::Controller,
                &author_access,
                fish_id,
                FishOperation::Restore,
                &restore,
            )
            .await
            .unwrap()["accepted"],
            true
        );
        assert!(simulation::apply_pending_fish_mutations(&pool, scene.scene_id, &mut loaded)
            .await
            .unwrap());
        assert_eq!(loaded.world.fish().len(), 1);
        assert!(list(&pool, GrantKind::Owner, &owner_access)
            .await
            .unwrap()
            .is_empty());
    }
}

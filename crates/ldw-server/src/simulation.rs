//! Persisted boundary for the server-owned simulation. The timer and broadcaster
//! are separate; this module never writes a frame to PostgreSQL.

use ldw_sim::{Bounds, World, WorldCheckpoint};
use serde_json::Value;
use sqlx::PgPool;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum SimulationError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("unsupported or inconsistent scene checkpoint")]
    InvalidScene,
    #[error("invalid underwater world package")]
    InvalidPackage,
    #[error("invalid simulation checkpoint: {0}")]
    Checkpoint(#[from] serde_json::Error),
}

pub struct LoadedScene {
    pub epoch: i64,
    pub persisted_tick: i64,
    pub world: World,
}

fn underwater_bounds() -> Result<Bounds, SimulationError> {
    let package: Value =
        serde_json::from_str(include_str!("../../../content/underwater/world.json"))
            .map_err(|_| SimulationError::InvalidPackage)?;
    let bounds: [f32; 4] = serde_json::from_value(package["zones"][0]["bounds"].clone())
        .map_err(|_| SimulationError::InvalidPackage)?;
    Ok(Bounds {
        min_x: bounds[0],
        max_x: bounds[1],
        min_y: bounds[2],
        max_y: bounds[3],
    })
}

pub async fn load_scene(pool: &PgPool, scene_id: Uuid) -> Result<LoadedScene, SimulationError> {
    let (world_id, world_version, epoch, tick, state): (String, i32, i64, i64, Value) =
        sqlx::query_as(
            "SELECT world_id, world_version, scene_epoch, simulation_tick, state \
             FROM scenes WHERE id = $1",
        )
        .bind(scene_id)
        .fetch_one(pool)
        .await?;
    if world_id != "underwater" || world_version != 1 || tick < 0 {
        return Err(SimulationError::InvalidScene);
    }
    let bounds = underwater_bounds()?;
    let world = if let Some(value) = state.get("simulation") {
        let checkpoint: WorldCheckpoint = serde_json::from_value(value.clone())?;
        if checkpoint.tick != tick as u64 || checkpoint.bounds != bounds {
            return Err(SimulationError::InvalidScene);
        }
        World::restore(checkpoint).map_err(|_| SimulationError::InvalidScene)?
    } else {
        if tick != 0
            || state
                .get("entities")
                .and_then(Value::as_array)
                .is_some_and(|entities| !entities.is_empty())
        {
            return Err(SimulationError::InvalidScene);
        }
        let seed = (scene_id.as_u128() as u64) ^ ((scene_id.as_u128() >> 64) as u64);
        World::new(bounds, seed).map_err(|_| SimulationError::InvalidPackage)?
    };
    Ok(LoadedScene {
        epoch,
        persisted_tick: tick,
        world,
    })
}

/// Save only after a complete 100-tick (5-second) interval or controlled stop.
/// Epoch and previous tick make an older worker unable to overwrite a newer run.
pub async fn save_checkpoint(
    pool: &PgPool,
    scene_id: Uuid,
    scene: &mut LoadedScene,
) -> Result<bool, SimulationError> {
    let tick =
        i64::try_from(scene.world.tick_number()).map_err(|_| SimulationError::InvalidScene)?;
    if tick <= scene.persisted_tick {
        return Err(SimulationError::InvalidScene);
    }
    let checkpoint = serde_json::to_value(scene.world.checkpoint())?;
    let saved = sqlx::query(
        "UPDATE scenes SET state = jsonb_set(state, '{simulation}', $1::jsonb, true), \
         simulation_tick = $2, updated_at = now() \
         WHERE id = $3 AND scene_epoch = $4 AND simulation_tick = $5",
    )
    .bind(checkpoint)
    .bind(tick)
    .bind(scene_id)
    .bind(scene.epoch)
    .bind(scene.persisted_tick)
    .execute(pool)
    .await?;
    if saved.rows_affected() == 1 {
        scene.persisted_tick = tick;
        Ok(true)
    } else {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ldw_sim::Point;

    #[tokio::test]
    async fn checkpoint_survives_database_roundtrip_and_rejects_stale_worker() {
        let pool = PgPool::connect(&std::env::var("DATABASE_URL").expect("isolated test database"))
            .await
            .unwrap();
        crate::migrate(&pool).await.unwrap();
        let owner = Uuid::new_v4();
        let session = Uuid::new_v4();
        let scene_id = Uuid::new_v4();
        sqlx::query("INSERT INTO accounts (id, login, role, password_hash) VALUES ($1, $2, 'owner', 'test-hash')")
            .bind(owner).bind(format!("sim-{owner}")).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO sessions (id, owner_id) VALUES ($1, $2)")
            .bind(session)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO scenes (id, session_id, world_id, world_version) VALUES ($1, $2, 'underwater', 1)")
            .bind(scene_id).bind(session).execute(&pool).await.unwrap();

        let mut worker = load_scene(&pool, scene_id).await.unwrap();
        let mut stale = load_scene(&pool, scene_id).await.unwrap();
        sqlx::query("UPDATE scenes SET state = '{\"pendingInteractions\":[{\"type\":\"interaction_requested\"}]}'::jsonb WHERE id = $1")
            .bind(scene_id).execute(&pool).await.unwrap();
        worker
            .world
            .spawn_fish(scene_id.as_u128(), Point { x: -3.0, y: 0.0 }, 1.2)
            .unwrap();
        stale
            .world
            .spawn_fish(scene_id.as_u128(), Point { x: 3.0, y: 0.0 }, 1.2)
            .unwrap();
        for _ in 0..100 {
            worker.world.step();
            stale.world.step();
        }
        assert!(save_checkpoint(&pool, scene_id, &mut worker).await.unwrap());
        assert!(!save_checkpoint(&pool, scene_id, &mut stale).await.unwrap());
        let restored = load_scene(&pool, scene_id).await.unwrap();
        assert_eq!(restored.world.checkpoint(), worker.world.checkpoint());
        assert_eq!(restored.persisted_tick, 100);
        let state: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
            .bind(scene_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            state["simulation"]["fish"][0]["id"],
            format!("{:032x}", scene_id.as_u128())
        );
        assert_eq!(state["pendingInteractions"].as_array().unwrap().len(), 1);

        sqlx::query("UPDATE scenes SET scene_epoch = scene_epoch + 1 WHERE id = $1")
            .bind(scene_id)
            .execute(&pool)
            .await
            .unwrap();
        worker.world.step();
        assert!(!save_checkpoint(&pool, scene_id, &mut worker).await.unwrap());

        let incomplete = Uuid::new_v4();
        sqlx::query("INSERT INTO scenes (id, session_id, world_id, world_version, state) VALUES ($1, $2, 'underwater', 1, '{\"entities\":[{\"id\":\"fish-1\"}]}'::jsonb)")
            .bind(incomplete).bind(session).execute(&pool).await.unwrap();
        assert!(matches!(
            load_scene(&pool, incomplete).await,
            Err(SimulationError::InvalidScene)
        ));
    }
}

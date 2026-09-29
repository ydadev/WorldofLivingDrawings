//! Persisted boundary for the server-owned simulation. The timer and broadcaster
//! are separate; this module never writes a frame to PostgreSQL.

use ldw_sim::{Bounds, Point, World, WorldCheckpoint};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{collections::HashSet, time::Duration};
use thiserror::Error;
use tokio::{
    sync::{broadcast, watch},
    task::JoinSet,
    time::{MissedTickBehavior, interval},
};
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
    #[error("invalid first fish publication")]
    InvalidPublication,
}

pub struct LoadedScene {
    pub epoch: i64,
    pub revision: i64,
    pub persisted_tick: i64,
    pub world: World,
}

#[derive(Clone)]
pub struct SimulationHub {
    sender: broadcast::Sender<PositionFrame>,
}

impl Default for SimulationHub {
    fn default() -> Self {
        let (sender, _) = broadcast::channel(32);
        Self { sender }
    }
}

impl SimulationHub {
    pub fn subscribe(&self) -> broadcast::Receiver<PositionFrame> {
        self.sender.subscribe()
    }

    pub fn publish(&self, frame: PositionFrame) {
        let _ = self.sender.send(frame);
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PositionFrame {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub schema_version: u8,
    pub scene_id: Uuid,
    pub scene_epoch: i64,
    pub revision: i64,
    pub simulation_tick: u64,
    pub positions: Vec<EntityPosition>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EntityPosition {
    pub id: String,
    pub position: ldw_sim::Point,
    pub heading: ldw_sim::Point,
}

fn position_frame(scene_id: Uuid, scene: &LoadedScene) -> PositionFrame {
    PositionFrame {
        kind: "positions",
        schema_version: 1,
        scene_id,
        scene_epoch: scene.epoch,
        revision: scene.revision,
        simulation_tick: scene.world.tick_number(),
        positions: scene
            .world
            .fish()
            .iter()
            .map(|fish| EntityPosition {
                id: format!("fish-{:032x}", fish.id),
                position: fish.position,
                heading: fish.heading,
            })
            .collect(),
    }
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

pub(crate) fn initial_world(scene_id: Uuid) -> Result<World, SimulationError> {
    let seed = (scene_id.as_u128() as u64) ^ ((scene_id.as_u128() >> 64) as u64);
    World::new(underwater_bounds()?, seed).map_err(|_| SimulationError::InvalidPackage)
}

fn feed_actions(world: &World) -> Value {
    json!(
        world
            .feed_sources()
            .iter()
            .map(|source| json!({
                "id":format!("feed-{}", source.id),
                "interactionId":"feed", "point":source.position,
                "remaining":source.remaining, "expiresAtTick":source.expires_at_tick,
            }))
            .collect::<Vec<_>>()
    )
}

fn interaction_state_event(world: &World, applied: &[Uuid]) -> Value {
    json!({"type":"interaction_state", "activeActions":feed_actions(world),
        "appliedCommandIds":applied, "simulationTick":world.tick_number()})
}

fn valid_publication(definition_id: &str, paint_blob_id: &str) -> bool {
    matches!(definition_id, "coral-fish" | "stream-fish")
        && (2..=64).contains(&paint_blob_id.len())
        && paint_blob_id
            .bytes()
            .next()
            .is_some_and(|ch| ch.is_ascii_lowercase())
        && paint_blob_id
            .bytes()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == b'-')
}

/// Trusted publication boundary: the caller must already own and validate the
/// PaintResult blob. This transaction establishes the first Entity and tick-0
/// checkpoint together; the supervisor discovers the new scene on its next scan.
pub(crate) async fn publish_first_fish(
    pool: &PgPool,
    scene_id: Uuid,
    fish_id: Uuid,
    definition_id: &str,
    paint_blob_id: &str,
    position: Point,
) -> Result<Value, SimulationError> {
    if !valid_publication(definition_id, paint_blob_id) {
        return Err(SimulationError::InvalidPublication);
    }
    let mut tx = pool.begin().await?;
    let row: Option<(String, i32, i64, i64, i64, Value, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT c.world_id, c.world_version, c.scene_epoch, c.revision, c.simulation_tick, \
         c.state, s.status, s.active_scene_id FROM scenes c JOIN sessions s ON s.id = c.session_id \
         WHERE c.id = $1 FOR UPDATE OF c",
    )
    .bind(scene_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((world_id, world_version, epoch, revision, tick, state, status, active_scene)) = row
    else {
        return Err(SimulationError::InvalidScene);
    };
    if world_id != "underwater"
        || world_version != 1
        || status != "running"
        || active_scene != Some(scene_id)
        || tick != 0
        || state.get("simulation").is_some()
        || state
            .get("entities")
            .is_some_and(|value| !matches!(value, Value::Array(entities) if entities.is_empty()))
    {
        return Err(SimulationError::InvalidScene);
    }
    let mut world = initial_world(scene_id)?;
    world
        .spawn_fish(fish_id.as_u128(), position, 1.2)
        .map_err(|_| SimulationError::InvalidPublication)?;
    let entity = json!({
        "id": format!("fish-{:032x}", fish_id.as_u128()),
        "definitionId": definition_id,
        "definitionVersion": 1,
        "paintBlobId": paint_blob_id,
        "position": position,
    });
    let event = json!({"type":"entity_published", "entity":entity});
    let new_revision = revision
        .checked_add(1)
        .ok_or(SimulationError::InvalidScene)?;
    sqlx::query(
        "UPDATE scenes SET revision = $2, updated_at = now(), \
         state = jsonb_set(jsonb_set(state, '{simulation}', $3::jsonb, true), \
         '{entities}', $4::jsonb, true) WHERE id = $1",
    )
    .bind(scene_id)
    .bind(new_revision)
    .bind(serde_json::to_value(world.checkpoint())?)
    .bind(json!([entity]))
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO scene_events (scene_id, revision, scene_epoch, event) VALUES ($1, $2, $3, $4)",
    )
    .bind(scene_id)
    .bind(new_revision)
    .bind(epoch)
    .bind(&event)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(event)
}

/// Caller must validate blob ownership and the PaintResult before this boundary.
/// A queued fish is not yet visible; only the scene worker can publish it.
pub(crate) async fn queue_fish(
    pool: &PgPool,
    scene_id: Uuid,
    fish_id: Uuid,
    definition_id: &str,
    paint_blob_id: &str,
    position: Point,
) -> Result<(), SimulationError> {
    if !valid_publication(definition_id, paint_blob_id) {
        return Err(SimulationError::InvalidPublication);
    }
    let mut tx = pool.begin().await?;
    let row: Option<(String, i32, i64, Value, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT c.world_id, c.world_version, c.simulation_tick, c.state, s.status, s.active_scene_id \
         FROM scenes c JOIN sessions s ON s.id = c.session_id WHERE c.id = $1 FOR UPDATE OF c",
    )
    .bind(scene_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((world_id, world_version, tick, state, status, active_scene)) = row else {
        return Err(SimulationError::InvalidScene);
    };
    if world_id != "underwater"
        || world_version != 1
        || status != "running"
        || active_scene != Some(scene_id)
        || tick < 0
    {
        return Err(SimulationError::InvalidScene);
    }
    let checkpoint: WorldCheckpoint = serde_json::from_value(
        state
            .get("simulation")
            .cloned()
            .ok_or(SimulationError::InvalidScene)?,
    )?;
    if checkpoint.tick != tick as u64 || checkpoint.bounds != underwater_bounds()? {
        return Err(SimulationError::InvalidScene);
    }
    let mut world = World::restore(checkpoint).map_err(|_| SimulationError::InvalidScene)?;
    let entities = state
        .get("entities")
        .and_then(Value::as_array)
        .ok_or(SimulationError::InvalidScene)?;
    if entities.len() != world.fish().len() {
        return Err(SimulationError::InvalidScene);
    }
    let queued: i64 =
        sqlx::query_scalar("SELECT count(*) FROM fish_publications WHERE scene_id = $1")
            .bind(scene_id)
            .fetch_one(&mut *tx)
            .await?;
    if entities.len() + queued as usize >= ldw_sim::MAX_FISH {
        return Err(SimulationError::InvalidPublication);
    }
    world
        .spawn_fish(fish_id.as_u128(), position, 1.2)
        .map_err(|_| SimulationError::InvalidPublication)?;
    sqlx::query(
        "INSERT INTO fish_publications (scene_id, fish_id, definition_id, paint_blob_id, position_x, position_y) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(scene_id)
    .bind(fish_id)
    .bind(definition_id)
    .bind(paint_blob_id)
    .bind(position.x)
    .bind(position.y)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn load_scene(pool: &PgPool, scene_id: Uuid) -> Result<LoadedScene, SimulationError> {
    let (world_id, world_version, epoch, revision, tick, state): (
        String,
        i32,
        i64,
        i64,
        i64,
        Value,
    ) = sqlx::query_as(
        "SELECT world_id, world_version, scene_epoch, revision, simulation_tick, state \
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
        initial_world(scene_id)?
    };
    Ok(LoadedScene {
        epoch,
        revision,
        persisted_tick: tick,
        world,
    })
}

/// Apply the durable inbox to the worker-owned world. PostgreSQL receives the
/// matching checkpoint, Entity list, ordered events and queue deletion before
/// the in-memory world changes; a crash leaves either all or none of the batch.
async fn apply_pending_fish(
    pool: &PgPool,
    scene_id: Uuid,
    scene: &mut LoadedScene,
) -> Result<bool, SimulationError> {
    let mut tx = pool.begin().await?;
    let row: Option<(String, i32, i64, i64, i64, Value, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT c.world_id, c.world_version, c.scene_epoch, c.revision, c.simulation_tick, \
         c.state, s.status, s.active_scene_id FROM scenes c JOIN sessions s ON s.id = c.session_id \
         WHERE c.id = $1 FOR UPDATE OF c",
    )
    .bind(scene_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((world_id, world_version, epoch, revision, persisted_tick, state, status, active)) =
        row
    else {
        return Ok(false);
    };
    if world_id != "underwater"
        || world_version != 1
        || epoch != scene.epoch
        || persisted_tick != scene.persisted_tick
        || status != "running"
        || active != Some(scene_id)
    {
        return Ok(false);
    }
    let pending: Vec<(Uuid, String, String, f32, f32)> = sqlx::query_as(
        "SELECT fish_id, definition_id, paint_blob_id, position_x, position_y \
         FROM fish_publications WHERE scene_id = $1 ORDER BY created_at, fish_id LIMIT 100",
    )
    .bind(scene_id)
    .fetch_all(&mut *tx)
    .await?;
    if pending.is_empty() {
        return Ok(true);
    }
    let mut candidate = scene.world.clone();
    let mut entities = state
        .get("entities")
        .and_then(Value::as_array)
        .cloned()
        .ok_or(SimulationError::InvalidScene)?;
    if entities.len() != candidate.fish().len() {
        return Err(SimulationError::InvalidScene);
    }
    let mut new_revision = revision;
    let mut events = Vec::with_capacity(pending.len());
    for (fish_id, definition_id, paint_blob_id, x, y) in &pending {
        if !valid_publication(definition_id, paint_blob_id) {
            return Err(SimulationError::InvalidPublication);
        }
        let position = Point { x: *x, y: *y };
        candidate
            .spawn_fish(fish_id.as_u128(), position, 1.2)
            .map_err(|_| SimulationError::InvalidPublication)?;
        let entity = json!({
            "id": format!("fish-{:032x}", fish_id.as_u128()),
            "definitionId": definition_id,
            "definitionVersion": 1,
            "paintBlobId": paint_blob_id,
            "position": position,
        });
        entities.push(entity.clone());
        new_revision = new_revision
            .checked_add(1)
            .ok_or(SimulationError::InvalidScene)?;
        events.push((
            new_revision,
            json!({"type":"entity_published", "entity":entity}),
        ));
    }
    let tick = i64::try_from(candidate.tick_number()).map_err(|_| SimulationError::InvalidScene)?;
    let updated = sqlx::query(
        "UPDATE scenes SET state = jsonb_set(jsonb_set(state, '{simulation}', $1::jsonb, true), \
         '{entities}', $2::jsonb, true), simulation_tick = $3, revision = $4, updated_at = now() \
         WHERE id = $5 AND scene_epoch = $6 AND simulation_tick = $7",
    )
    .bind(serde_json::to_value(candidate.checkpoint())?)
    .bind(json!(entities))
    .bind(tick)
    .bind(new_revision)
    .bind(scene_id)
    .bind(scene.epoch)
    .bind(scene.persisted_tick)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Ok(false);
    }
    for (revision, event) in events {
        sqlx::query(
            "INSERT INTO scene_events (scene_id, revision, scene_epoch, event) VALUES ($1, $2, $3, $4)",
        )
        .bind(scene_id)
        .bind(revision)
        .bind(scene.epoch)
        .bind(event)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("DELETE FROM fish_publications WHERE scene_id = $1")
        .bind(scene_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    scene.world = candidate;
    scene.persisted_tick = tick;
    scene.revision = new_revision;
    Ok(true)
}

/// Accepted commands remain in the scene until the worker commits their effect.
/// The checkpoint, projection, queue removal and revisioned event are atomic.
async fn apply_pending_interactions(
    pool: &PgPool,
    scene_id: Uuid,
    scene: &mut LoadedScene,
) -> Result<bool, SimulationError> {
    let mut tx = pool.begin().await?;
    let row: Option<(String, i32, i64, i64, i64, Value, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT c.world_id, c.world_version, c.scene_epoch, c.revision, c.simulation_tick, \
         c.state, s.status, s.active_scene_id FROM scenes c JOIN sessions s ON s.id = c.session_id \
         WHERE c.id = $1 FOR UPDATE OF c",
    )
    .bind(scene_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((world_id, world_version, epoch, revision, persisted_tick, mut state, status, active)) =
        row
    else {
        return Ok(false);
    };
    if world_id != "underwater"
        || world_version != 1
        || epoch != scene.epoch
        || persisted_tick != scene.persisted_tick
        || status != "running"
        || active != Some(scene_id)
    {
        return Ok(false);
    }
    let pending = state
        .get("pendingInteractions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut candidate = scene.world.clone();
    let mut remaining = Vec::new();
    let mut applied = Vec::new();
    for entry in pending {
        if entry.get("type").and_then(Value::as_str) != Some("interaction_requested")
            || entry.get("interactionId").and_then(Value::as_str) != Some("feed")
        {
            remaining.push(entry);
            continue;
        }
        let id = entry
            .get("commandId")
            .and_then(Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok())
            .ok_or(SimulationError::InvalidScene)?;
        let point: Point = serde_json::from_value(
            entry
                .get("point")
                .cloned()
                .ok_or(SimulationError::InvalidScene)?,
        )?;
        candidate
            .start_feed(&id.simple().to_string(), point)
            .map_err(|_| SimulationError::InvalidScene)?;
        applied.push(id);
    }
    if applied.is_empty() {
        return Ok(true);
    }
    let tick = i64::try_from(candidate.tick_number()).map_err(|_| SimulationError::InvalidScene)?;
    let new_revision = revision
        .checked_add(1)
        .ok_or(SimulationError::InvalidScene)?;
    let event = interaction_state_event(&candidate, &applied);
    let object = state.as_object_mut().ok_or(SimulationError::InvalidScene)?;
    object.insert(
        "simulation".into(),
        serde_json::to_value(candidate.checkpoint())?,
    );
    object.insert("pendingInteractions".into(), json!(remaining));
    object.insert("activeActions".into(), feed_actions(&candidate));
    let updated = sqlx::query(
        "UPDATE scenes SET state = $1::jsonb, simulation_tick = $2, revision = $3, updated_at = now() \
         WHERE id = $4 AND scene_epoch = $5 AND simulation_tick = $6",
    )
    .bind(state)
    .bind(tick)
    .bind(new_revision)
    .bind(scene_id)
    .bind(scene.epoch)
    .bind(scene.persisted_tick)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Ok(false);
    }
    sqlx::query(
        "INSERT INTO scene_events (scene_id, revision, scene_epoch, event) VALUES ($1, $2, $3, $4)",
    )
    .bind(scene_id)
    .bind(new_revision)
    .bind(scene.epoch)
    .bind(event)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    scene.world = candidate;
    scene.persisted_tick = tick;
    scene.revision = new_revision;
    Ok(true)
}

/// Resource consumption and expiry are durable transitions. They get one
/// checkpoint and one revision, so a restart cannot replay an eaten portion.
async fn save_interaction_state(
    pool: &PgPool,
    scene_id: Uuid,
    scene: &mut LoadedScene,
) -> Result<bool, SimulationError> {
    let mut tx = pool.begin().await?;
    let row: Option<(i64, i64, i64, Value, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT c.scene_epoch, c.revision, c.simulation_tick, c.state, s.status, s.active_scene_id \
         FROM scenes c JOIN sessions s ON s.id = c.session_id WHERE c.id = $1 FOR UPDATE OF c",
    )
    .bind(scene_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((epoch, revision, persisted_tick, mut state, status, active)) = row else {
        return Ok(false);
    };
    if epoch != scene.epoch
        || persisted_tick != scene.persisted_tick
        || status != "running"
        || active != Some(scene_id)
    {
        return Ok(false);
    }
    let tick =
        i64::try_from(scene.world.tick_number()).map_err(|_| SimulationError::InvalidScene)?;
    let new_revision = revision
        .checked_add(1)
        .ok_or(SimulationError::InvalidScene)?;
    let event = interaction_state_event(&scene.world, &[]);
    let object = state.as_object_mut().ok_or(SimulationError::InvalidScene)?;
    object.insert(
        "simulation".into(),
        serde_json::to_value(scene.world.checkpoint())?,
    );
    object.insert("activeActions".into(), feed_actions(&scene.world));
    let updated = sqlx::query(
        "UPDATE scenes SET state = $1::jsonb, simulation_tick = $2, revision = $3, updated_at = now() \
         WHERE id = $4 AND scene_epoch = $5 AND simulation_tick = $6",
    )
    .bind(state)
    .bind(tick)
    .bind(new_revision)
    .bind(scene_id)
    .bind(scene.epoch)
    .bind(scene.persisted_tick)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Ok(false);
    }
    sqlx::query(
        "INSERT INTO scene_events (scene_id, revision, scene_epoch, event) VALUES ($1, $2, $3, $4)",
    )
    .bind(scene_id)
    .bind(new_revision)
    .bind(scene.epoch)
    .bind(event)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    scene.persisted_tick = tick;
    scene.revision = new_revision;
    Ok(true)
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

/// Run active scenes with a checkpoint. A newly created empty scene has no work
/// until a fish is published with its first checkpoint by the creation flow.
pub async fn run(
    pool: PgPool,
    mut shutdown: watch::Receiver<bool>,
    hub: SimulationHub,
) -> Result<(), SimulationError> {
    let mut scan = interval(Duration::from_secs(1));
    scan.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut workers = JoinSet::new();
    let mut active = HashSet::new();
    loop {
        tokio::select! {
            _ = scan.tick() => {
                while let Some(result) = workers.try_join_next() {
                    if let Ok((scene_id, outcome)) = result {
                        active.remove(&scene_id);
                        if let Err(error) = outcome {
                            eprintln!("simulation scene {scene_id}: {error}");
                        }
                    }
                }
                let scenes: Vec<Uuid> = match sqlx::query_scalar(
                    "SELECT c.id FROM scenes c JOIN sessions s ON s.active_scene_id = c.id \
                     WHERE s.status = 'running' AND c.state ? 'simulation' \
                     ORDER BY c.id LIMIT 3",
                )
                .fetch_all(&pool)
                .await {
                    Ok(scenes) => scenes,
                    Err(error) => {
                        eprintln!("simulation scan: {error}");
                        continue;
                    }
                };
                for scene_id in scenes {
                    if active.insert(scene_id) {
                        let pool = pool.clone();
                        let shutdown = shutdown.clone();
                        let hub = hub.clone();
                        workers.spawn(async move {
                            (scene_id, run_scene(pool, scene_id, shutdown, hub, Duration::from_millis(50)).await)
                        });
                    }
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() { break; }
            }
        }
    }
    while let Some(result) = workers.join_next().await {
        if let Ok((scene_id, outcome)) = result {
            if let Err(error) = outcome {
                eprintln!("simulation scene {scene_id}: {error}");
            }
        }
    }
    Ok(())
}

async fn run_scene(
    pool: PgPool,
    scene_id: Uuid,
    mut shutdown: watch::Receiver<bool>,
    hub: SimulationHub,
    period: Duration,
) -> Result<(), SimulationError> {
    let mut scene = load_scene(&pool, scene_id).await?;
    let mut timer = interval(period);
    timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = timer.tick() => {
                if scene.world.tick_number() % 20 == 0 {
                    let running: bool = sqlx::query_scalar(
                        "SELECT EXISTS(SELECT 1 FROM scenes c JOIN sessions s \
                         ON s.active_scene_id = c.id WHERE c.id = $1 \
                         AND c.scene_epoch = $2 AND s.status = 'running')",
                    )
                    .bind(scene_id)
                    .bind(scene.epoch)
                    .fetch_one(&pool)
                    .await?;
                    if !running { break; }
                    if !apply_pending_fish(&pool, scene_id, &mut scene).await? {
                        return Ok(());
                    }
                    if !apply_pending_interactions(&pool, scene_id, &mut scene).await? {
                        return Ok(());
                    }
                }
                let food_before = scene.world.feed_sources().to_vec();
                scene.world.step();
                if scene.world.feed_sources() != food_before
                    && !save_interaction_state(&pool, scene_id, &mut scene).await?
                {
                    return Ok(());
                }
                if scene.world.tick_number() % 10 == 0 {
                    hub.publish(position_frame(scene_id, &scene));
                }
                if scene.world.tick_number() % 100 == 0
                    && scene.world.tick_number() > scene.persisted_tick as u64
                    && !save_checkpoint(&pool, scene_id, &mut scene).await?
                {
                    return Ok(());
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() { break; }
            }
        }
    }
    if scene.world.tick_number() > scene.persisted_tick as u64 {
        let _ = save_checkpoint(&pool, scene_id, &mut scene).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        access::{AccessStore, GrantKind},
        realtime::{self, InteractionCommand},
    };
    use ldw_sim::Point;
    use sha2::{Digest, Sha256};
    use tokio::time::{sleep, timeout};

    #[tokio::test]
    async fn accepted_feed_is_applied_once_consumed_and_restored() {
        let pool = crate::test_pool().await;
        let owner = Uuid::new_v4();
        let grant = Uuid::new_v4();
        let session = Uuid::new_v4();
        let scene_id = Uuid::new_v4();
        let token = Uuid::new_v4().to_string();
        let token_hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        sqlx::query("INSERT INTO accounts (id, login, role, password_hash) VALUES ($1, $2, 'owner', 'test-hash')")
            .bind(owner).bind(format!("feed-{owner}")).execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO owner_grants (id, account_id, token_hash, csrf_hash, expires_at) \
                     VALUES ($1, $2, $3, $3, now() + interval '1 day')",
        )
        .bind(grant)
        .bind(owner)
        .bind(token_hash.to_vec())
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO sessions (id, owner_id) VALUES ($1, $2)")
            .bind(session)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO scenes (id, session_id, world_id, world_version) VALUES ($1, $2, 'underwater', 1)")
            .bind(scene_id).bind(session).execute(&pool).await.unwrap();
        sqlx::query("UPDATE sessions SET active_scene_id = $1 WHERE id = $2")
            .bind(scene_id)
            .bind(session)
            .execute(&pool)
            .await
            .unwrap();
        let store = AccessStore::new(pool.clone(), [3; 32]);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let command = InteractionCommand {
            kind: "command".into(),
            command_id: Uuid::new_v4(),
            session_id: session,
            scene_id,
            scene_epoch: 1,
            interaction_id: "feed".into(),
            point: realtime::Point { x: 0.0, y: 0.0 },
            expires_at: now_ms + 8_000,
        };
        let outside = InteractionCommand {
            command_id: Uuid::new_v4(),
            point: realtime::Point { x: 7.49, y: 0.0 },
            ..command.clone()
        };
        let invalid =
            realtime::process_command(&store, GrantKind::Owner, &token, session, &outside)
                .await
                .unwrap();
        assert_eq!(invalid["code"], "OUTSIDE_WATER");
        let ack = realtime::process_command(&store, GrantKind::Owner, &token, session, &command)
            .await
            .unwrap();
        assert_eq!(ack["accepted"], true);
        assert_eq!(ack["revision"], 1);
        let pending: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
            .bind(scene_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(pending["pendingInteractions"].as_array().unwrap().len(), 1);
        assert_eq!(pending["simulation"]["tick"], 0);
        let repeat = realtime::process_command(&store, GrantKind::Owner, &token, session, &command)
            .await
            .unwrap();
        assert_eq!(repeat, ack);
        let immediate = InteractionCommand {
            command_id: Uuid::new_v4(),
            ..command.clone()
        };
        let rejected =
            realtime::process_command(&store, GrantKind::Owner, &token, session, &immediate)
                .await
                .unwrap();
        assert_eq!(rejected["accepted"], false);
        assert!(rejected["code"] == "FEED_SCENE_COOLDOWN" || rejected["code"] == "FEED_COOLDOWN");

        let (stop, receiver) = watch::channel(false);
        let worker = tokio::spawn(run_scene(
            pool.clone(),
            scene_id,
            receiver,
            SimulationHub::default(),
            Duration::from_millis(5),
        ));
        timeout(Duration::from_secs(5), async {
            loop {
                let state: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
                    .bind(scene_id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                if state["activeActions"]
                    .as_array()
                    .is_some_and(|actions| actions.len() == 1)
                {
                    assert!(state["pendingInteractions"].as_array().unwrap().is_empty());
                    break;
                }
                sleep(Duration::from_millis(15)).await;
            }
        })
        .await
        .unwrap();
        queue_fish(
            &pool,
            scene_id,
            Uuid::new_v4(),
            "coral-fish",
            "paint-fed",
            Point { x: 0.5, y: 0.0 },
        )
        .await
        .unwrap();
        timeout(Duration::from_secs(5), async {
            loop {
                let state: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
                    .bind(scene_id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                if state["activeActions"][0]["remaining"] == 9 {
                    break;
                }
                sleep(Duration::from_millis(15)).await;
            }
        })
        .await
        .unwrap();
        stop.send(true).unwrap();
        worker.await.unwrap().unwrap();
        let restored = load_scene(&pool, scene_id).await.unwrap();
        assert_eq!(restored.world.feed_sources()[0].remaining, 9);
        assert_eq!(restored.world.feed_sources()[0].fed_fish.len(), 1);
        let consumed_at = restored.world.tick_number();
        let (stop_again, receiver_again) = watch::channel(false);
        let worker_again = tokio::spawn(run_scene(
            pool.clone(),
            scene_id,
            receiver_again,
            SimulationHub::default(),
            Duration::from_millis(5),
        ));
        timeout(Duration::from_secs(5), async {
            loop {
                let loaded = load_scene(&pool, scene_id).await.unwrap();
                if loaded.world.tick_number() >= consumed_at + 40 {
                    break loaded;
                }
                sleep(Duration::from_millis(15)).await;
            }
        })
        .await
        .unwrap();
        stop_again.send(true).unwrap();
        worker_again.await.unwrap().unwrap();
        let after_restart = load_scene(&pool, scene_id).await.unwrap();
        assert_eq!(
            after_restart.world.feed_sources()[0].remaining,
            9,
            "restored fish must not eat the same portion twice"
        );
        let state: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
            .bind(scene_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state["activeActions"][0]["remaining"], 9);
        let transitions: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM scene_events \
            WHERE scene_id = $1 AND event->>'type' = 'interaction_state'",
        )
        .bind(scene_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            transitions >= 2,
            "start and consumption need revisioned events"
        );
    }

    #[tokio::test]
    async fn queued_fish_join_running_world_and_survive_restart() {
        let pool = crate::test_pool().await;
        let owner = Uuid::new_v4();
        let session = Uuid::new_v4();
        let scene_id = Uuid::new_v4();
        sqlx::query("INSERT INTO accounts (id, login, role, password_hash) VALUES ($1, $2, 'owner', 'test-hash')")
            .bind(owner).bind(format!("live-{owner}")).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO sessions (id, owner_id) VALUES ($1, $2)")
            .bind(session)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO scenes (id, session_id, world_id, world_version) VALUES ($1, $2, 'underwater', 1)")
            .bind(scene_id).bind(session).execute(&pool).await.unwrap();
        sqlx::query("UPDATE sessions SET active_scene_id = $1 WHERE id = $2")
            .bind(scene_id)
            .bind(session)
            .execute(&pool)
            .await
            .unwrap();
        let first = Uuid::new_v4();
        publish_first_fish(
            &pool,
            scene_id,
            first,
            "coral-fish",
            "paint-first",
            Point { x: -2.0, y: 0.0 },
        )
        .await
        .unwrap();
        let (stop, receiver) = watch::channel(false);
        let hub = SimulationHub::default();
        let mut frames = hub.subscribe();
        let worker = tokio::spawn(run_scene(
            pool.clone(),
            scene_id,
            receiver,
            hub,
            Duration::from_millis(5),
        ));
        let initial = timeout(Duration::from_secs(5), frames.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(initial.positions.len(), 1);
        let second = Uuid::new_v4();
        let before: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
            .bind(scene_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(before["entities"].as_array().unwrap().len(), 1);
        assert!(
            queue_fish(
                &pool,
                scene_id,
                Uuid::new_v4(),
                "stream-fish",
                "paint-bad",
                Point { x: 99.0, y: 0.0 }
            )
            .await
            .is_err()
        );
        queue_fish(
            &pool,
            scene_id,
            second,
            "stream-fish",
            "paint-second",
            Point { x: 2.0, y: 0.0 },
        )
        .await
        .unwrap();
        assert!(
            queue_fish(
                &pool,
                scene_id,
                second,
                "stream-fish",
                "paint-second",
                Point { x: 2.0, y: 0.0 }
            )
            .await
            .is_err()
        );
        let applied = timeout(Duration::from_secs(5), async {
            loop {
                let frame = frames.recv().await.unwrap();
                if frame.positions.len() == 2 {
                    break frame;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(applied.revision, 2);
        assert!(
            applied
                .positions
                .iter()
                .any(|fish| fish.id == format!("fish-{:032x}", second.as_u128()))
        );
        stop.send(true).unwrap();
        worker.await.unwrap().unwrap();
        let restored = load_scene(&pool, scene_id).await.unwrap();
        assert_eq!(restored.world.fish().len(), 2);
        assert!(restored.persisted_tick >= 20);
        let second_event: Value = sqlx::query_scalar(
            "SELECT event FROM scene_events WHERE scene_id = $1 AND revision = 2",
        )
        .bind(scene_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(second_event["entity"]["paintBlobId"], "paint-second");

        let third = Uuid::new_v4();
        queue_fish(
            &pool,
            scene_id,
            third,
            "coral-fish",
            "paint-third",
            Point { x: 0.0, y: 2.0 },
        )
        .await
        .unwrap();
        let fourth = Uuid::new_v4();
        queue_fish(
            &pool,
            scene_id,
            fourth,
            "stream-fish",
            "paint-fourth",
            Point { x: 3.0, y: -2.0 },
        )
        .await
        .unwrap();
        let queued: i64 =
            sqlx::query_scalar("SELECT count(*) FROM fish_publications WHERE scene_id = $1")
                .bind(scene_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(queued, 2);
        let (stop, receiver) = watch::channel(false);
        let hub = SimulationHub::default();
        let mut frames = hub.subscribe();
        let worker = tokio::spawn(run_scene(
            pool.clone(),
            scene_id,
            receiver,
            hub,
            Duration::from_millis(5),
        ));
        let resumed = timeout(Duration::from_secs(5), async {
            loop {
                let frame = frames.recv().await.unwrap();
                if frame.positions.len() == 4 {
                    break frame;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(resumed.revision, 4);
        stop.send(true).unwrap();
        worker.await.unwrap().unwrap();
        let after = load_scene(&pool, scene_id).await.unwrap();
        assert_eq!(after.world.fish().len(), 4);
        let state: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
            .bind(scene_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state["entities"].as_array().unwrap().len(), 4);
        let revisions: Vec<i64> = sqlx::query_scalar(
            "SELECT revision FROM scene_events WHERE scene_id = $1 ORDER BY revision",
        )
        .bind(scene_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(revisions, [1, 2, 3, 4]);
        let queued: i64 =
            sqlx::query_scalar("SELECT count(*) FROM fish_publications WHERE scene_id = $1")
                .bind(scene_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(queued, 0);
    }

    #[tokio::test]
    async fn checkpoint_survives_database_roundtrip_and_rejects_stale_worker() {
        let pool = crate::test_pool().await;
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

        let ticking = Uuid::new_v4();
        let mut initial = World::new(underwater_bounds().unwrap(), 42).unwrap();
        initial
            .spawn_fish(ticking.as_u128(), Point { x: -3.0, y: 0.0 }, 1.2)
            .unwrap();
        sqlx::query("INSERT INTO scenes (id, session_id, world_id, world_version, state) VALUES ($1, $2, 'underwater', 1, $3)")
            .bind(ticking)
            .bind(session)
            .bind(serde_json::json!({"simulation": initial.checkpoint()}))
            .execute(&pool).await.unwrap();
        sqlx::query("UPDATE sessions SET active_scene_id = $1 WHERE id = $2")
            .bind(ticking)
            .bind(session)
            .execute(&pool)
            .await
            .unwrap();
        let (stop, receiver) = watch::channel(false);
        let hub = SimulationHub::default();
        let mut positions = hub.subscribe();
        let worker = tokio::spawn(run_scene(
            pool.clone(),
            ticking,
            receiver,
            hub,
            Duration::from_millis(1),
        ));
        let first_frame = timeout(Duration::from_secs(5), positions.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first_frame.scene_id, ticking);
        assert_eq!(first_frame.simulation_tick, 10);
        assert_eq!(
            first_frame.positions[0].id,
            format!("fish-{:032x}", ticking.as_u128())
        );
        timeout(Duration::from_secs(5), async {
            loop {
                let tick: i64 =
                    sqlx::query_scalar("SELECT simulation_tick FROM scenes WHERE id = $1")
                        .bind(ticking)
                        .fetch_one(&pool)
                        .await
                        .unwrap();
                if tick >= 100 {
                    break;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("100 logical ticks should save one checkpoint");
        stop.send(true).unwrap();
        worker.await.unwrap().unwrap();
        let restored = load_scene(&pool, ticking).await.unwrap();
        assert!(restored.persisted_tick >= 100);
        assert_eq!(restored.world.tick_number(), restored.persisted_tick as u64);
        let revision: i64 = sqlx::query_scalar("SELECT revision FROM scenes WHERE id = $1")
            .bind(ticking)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            revision, 0,
            "position checkpoints must not create durable revisions"
        );
    }
}

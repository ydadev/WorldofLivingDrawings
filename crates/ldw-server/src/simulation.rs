//! Persisted boundary for the server-owned simulation. The timer and broadcaster
//! are separate; this module never writes a frame to PostgreSQL.

use ldw_sim::{Bounds, Point, World, WorldCheckpoint};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Transaction};
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
    #[error("three simulated sessions are already running")]
    SessionLimit,
}

#[cfg(not(test))]
const MAX_SIMULATED_SESSIONS: i64 = 3;
const ADMISSION_LOCK: i64 = i64::from_be_bytes(*b"LDWSIM03");

/// Call only inside the transaction that creates a scene's first checkpoint.
/// The transaction-level lock serializes the count and the subsequent write.
pub(crate) async fn simulation_slot_available(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<bool, sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(ADMISSION_LOCK)
        .execute(&mut **tx)
        .await?;
    let running: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sessions s JOIN scenes c ON c.id = s.active_scene_id \
         WHERE s.status = 'running' AND c.state ? 'simulation'",
    )
    .fetch_one(&mut **tx)
    .await?;
    #[cfg(test)]
    let limit = std::env::var("LDW_TEST_SIMULATED_SESSION_LIMIT")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(1_000);
    #[cfg(not(test))]
    let limit = MAX_SIMULATED_SESSIONS;
    Ok(running < limit)
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
    changes: broadcast::Sender<Uuid>,
}

impl Default for SimulationHub {
    fn default() -> Self {
        let (sender, _) = broadcast::channel(32);
        let (changes, _) = broadcast::channel(64);
        Self { sender, changes }
    }
}

impl SimulationHub {
    pub fn subscribe(&self) -> broadcast::Receiver<PositionFrame> {
        self.sender.subscribe()
    }

    pub fn publish(&self, frame: PositionFrame) {
        let _ = self.sender.send(frame);
    }

    pub fn subscribe_changes(&self) -> broadcast::Receiver<Uuid> {
        self.changes.subscribe()
    }

    pub fn notify_change(&self, scene_id: Uuid) {
        let _ = self.changes.send(scene_id);
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
    pub action_positions: Vec<ActionPosition>,
}

#[derive(Clone, Debug, Serialize)]
pub struct EntityPosition {
    pub id: String,
    pub position: ldw_sim::Point,
    pub heading: ldw_sim::Point,
    pub depth: f32,
    #[serde(rename = "headingDepth")]
    pub heading_depth: f32,
}

#[derive(Clone, Debug, Serialize)]
pub struct ActionPosition {
    pub id: String,
    pub position: ldw_sim::Point,
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
                depth: fish.depth,
                heading_depth: fish.heading_depth,
            })
            .collect(),
        action_positions: scene
            .world
            .boat()
            .map(|boat| {
                vec![ActionPosition {
                    id: format!("boat-{}", boat.id),
                    position: boat.position,
                }]
            })
            .unwrap_or_default(),
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

fn active_actions(world: &World) -> Value {
    let mut actions: Vec<Value> = world
        .feed_sources()
        .iter()
        .map(|source| {
            json!({
                "id":format!("feed-{}", source.id),
                "interactionId":"feed", "point":source.position,
                "remaining":source.remaining, "expiresAtTick":source.expires_at_tick,
            })
        })
        .collect();
    if let Some(boat) = world.boat() {
        actions.push(json!({
            "id":format!("boat-{}", boat.id), "interactionId":"boat",
            "point":boat.via, "position":boat.position,
            "entry":boat.entry, "exit":boat.exit,
            "expiresAtTick":boat.expires_at_tick,
        }));
    }
    json!(actions)
}

fn interaction_state_event(world: &World, applied: &[Uuid]) -> Value {
    json!({"type":"interaction_state", "activeActions":active_actions(world),
        "appliedCommandIds":applied, "simulationTick":world.tick_number()})
}

fn valid_publication(definition_id: &str, paint_blob_id: &str) -> bool {
    matches!(definition_id, "coral-fish" | "stream-fish")
        && ((paint_blob_id.len() == 64
            && paint_blob_id
                .bytes()
                .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase()))
            || ((2..=64).contains(&paint_blob_id.len())
                && paint_blob_id
                    .bytes()
                    .next()
                    .is_some_and(|ch| ch.is_ascii_lowercase())
                && paint_blob_id
                    .bytes()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == b'-')))
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
    let mut tx = pool.begin().await?;
    let event = publish_first_fish_tx(
        &mut tx,
        scene_id,
        fish_id,
        definition_id,
        paint_blob_id,
        position,
    )
    .await?;
    tx.commit().await?;
    Ok(event)
}

pub(crate) async fn publish_first_fish_tx(
    tx: &mut Transaction<'_, Postgres>,
    scene_id: Uuid,
    fish_id: Uuid,
    definition_id: &str,
    paint_blob_id: &str,
    position: Point,
) -> Result<Value, SimulationError> {
    if !valid_publication(definition_id, paint_blob_id) {
        return Err(SimulationError::InvalidPublication);
    }
    let row: Option<(String, i32, i64, i64, i64, Value, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT c.world_id, c.world_version, c.scene_epoch, c.revision, c.simulation_tick, \
         c.state, s.status, s.active_scene_id FROM scenes c JOIN sessions s ON s.id = c.session_id \
         WHERE c.id = $1 FOR UPDATE OF c",
    )
    .bind(scene_id)
    .fetch_optional(&mut **tx)
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
    if !simulation_slot_available(tx).await? {
        return Err(SimulationError::SessionLimit);
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
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO scene_events (scene_id, revision, scene_epoch, event) VALUES ($1, $2, $3, $4)",
    )
    .bind(scene_id)
    .bind(new_revision)
    .bind(epoch)
    .bind(&event)
    .execute(&mut **tx)
    .await?;
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
    let mut tx = pool.begin().await?;
    queue_fish_tx(
        &mut tx,
        scene_id,
        fish_id,
        definition_id,
        paint_blob_id,
        position,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn queue_fish_tx(
    tx: &mut Transaction<'_, Postgres>,
    scene_id: Uuid,
    fish_id: Uuid,
    definition_id: &str,
    paint_blob_id: &str,
    position: Point,
) -> Result<(), SimulationError> {
    if !valid_publication(definition_id, paint_blob_id) {
        return Err(SimulationError::InvalidPublication);
    }
    let row: Option<(String, i32, i64, Value, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT c.world_id, c.world_version, c.simulation_tick, c.state, s.status, s.active_scene_id \
         FROM scenes c JOIN sessions s ON s.id = c.session_id WHERE c.id = $1 FOR UPDATE OF c",
    )
    .bind(scene_id)
    .fetch_optional(&mut **tx)
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
            .fetch_one(&mut **tx)
            .await?;
    let reserved: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM upload_intents WHERE scene_id = $1 \
         AND status <> 'finalized' AND reservation_until > now()",
    )
    .bind(scene_id)
    .fetch_one(&mut **tx)
    .await?;
    if entities.len() + queued as usize + reserved as usize >= ldw_sim::MAX_FISH {
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
    .execute(&mut **tx)
    .await?;
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
        if entry.get("type").and_then(Value::as_str) != Some("interaction_requested") {
            remaining.push(entry);
            continue;
        }
        let interaction = entry.get("interactionId").and_then(Value::as_str);
        if !matches!(interaction, Some("feed" | "boat")) {
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
        if interaction == Some("feed") {
            candidate
                .start_feed(&id.simple().to_string(), point)
                .map_err(|_| SimulationError::InvalidScene)?;
        } else {
            candidate
                .start_boat(&id.simple().to_string(), point)
                .map_err(|_| SimulationError::InvalidScene)?;
        }
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
    object.insert("activeActions".into(), active_actions(&candidate));
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
    object.insert("activeActions".into(), active_actions(&scene.world));
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
    let mut changes = hub.subscribe_changes();
    let mut pending_wakeup = false;
    let mut timer = interval(period);
    timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = timer.tick() => {
                if scene.world.tick_number() % 20 == 0 || pending_wakeup {
                    pending_wakeup = false;
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
                    let prior_revision = scene.revision;
                    if !apply_pending_interactions(&pool, scene_id, &mut scene).await? {
                        return Ok(());
                    }
                    if scene.revision != prior_revision {
                        hub.notify_change(scene_id);
                    }
                }
                let food_before = scene.world.feed_sources().to_vec();
                let boat_before = scene.world.boat().is_some();
                scene.world.step();
                if (scene.world.feed_sources() != food_before
                    || scene.world.boat().is_some() != boat_before)
                    && !save_interaction_state(&pool, scene_id, &mut scene).await?
                {
                    return Ok(());
                }
                if scene.world.feed_sources() != food_before
                    || scene.world.boat().is_some() != boat_before {
                    hub.notify_change(scene_id);
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
            changed = changes.recv() => {
                match changed {
                    Ok(changed_scene) if changed_scene == scene_id => pending_wakeup = true,
                    Err(broadcast::error::RecvError::Lagged(_)) => pending_wakeup = true,
                    _ => {}
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
        blob_store::BlobStore,
        http::{self, AppState},
        realtime::{self, InteractionCommand},
    };
    use futures_util::{SinkExt, StreamExt};
    use ldw_sim::Point;
    use sha2::{Digest, Sha256};
    use std::sync::Arc;
    use tokio::time::{sleep, timeout};
    use tokio_tungstenite::tungstenite::{Message as ClientMessage, client::IntoClientRequest};

    #[tokio::test]
    #[ignore = "run against a fresh PostgreSQL database to measure three runners and 30 Controllers"]
    async fn three_hundred_fish_progress_across_three_real_time_scenes() {
        let pool = crate::test_pool().await;
        let owner = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO accounts (id, login, role, password_hash) \
             VALUES ($1, $2, 'owner', 'test-hash')",
        )
        .bind(owner)
        .bind(format!("three-scenes-{owner}"))
        .execute(&pool)
        .await
        .unwrap();
        let mut scenes = Vec::new();
        for scene_number in 0..3 {
            let session = Uuid::new_v4();
            let scene_id = Uuid::new_v4();
            sqlx::query("INSERT INTO sessions (id, owner_id) VALUES ($1, $2)")
                .bind(session)
                .bind(owner)
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO scenes (id, session_id, world_id, world_version) \
                 VALUES ($1, $2, 'underwater', 1)",
            )
            .bind(scene_id)
            .bind(session)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("UPDATE sessions SET active_scene_id = $1 WHERE id = $2")
                .bind(scene_id)
                .bind(session)
                .execute(&pool)
                .await
                .unwrap();

            let mut world = initial_world(scene_id).unwrap();
            let mut entities = Vec::new();
            for fish in 0..ldw_sim::MAX_FISH {
                let fish_id = Uuid::new_v4();
                let position = Point {
                    x: -6.3 + (fish % 10) as f32 * 1.4,
                    y: -3.3 + (fish / 10) as f32 * 0.73,
                };
                world.spawn_fish(fish_id.as_u128(), position, 1.2).unwrap();
                entities.push(json!({
                    "id": format!("fish-{:032x}", fish_id.as_u128()),
                    "definitionId": if fish % 2 == 0 { "coral-fish" } else { "stream-fish" },
                    "definitionVersion": 1,
                    "paintBlobId": "paint-load-fixture",
                    "position": position,
                }));
            }
            let state = json!({
                "entities": entities,
                "simulation": world.checkpoint(),
                "activeActions": [],
                "pendingInteractions": [],
            });
            sqlx::query("UPDATE scenes SET state = $1::jsonb WHERE id = $2")
                .bind(state)
                .bind(scene_id)
                .execute(&pool)
                .await
                .unwrap();
            scenes.push((scene_number, session, scene_id));
        }

        let (stop, receiver) = watch::channel(false);
        let hub = SimulationHub::default();
        let mut frames = hub.subscribe();
        let store = AccessStore::new(pool.clone(), [7u8; 32]);
        let blob_root = std::env::temp_dir().join(format!("ldw-load-blobs-{}", Uuid::new_v4()));
        let app = http::router(AppState {
            access: store,
            blob_store: BlobStore::create(blob_root).unwrap(),
            public_origin: Arc::from("https://world.example.test"),
            simulation_hub: hub.clone(),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let mut controllers = tokio::task::JoinSet::new();
        let client_start = Arc::new(tokio::sync::Barrier::new(31));
        let command_times = Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
            Uuid,
            tokio::time::Instant,
        >::new()));
        for &(scene_number, session_id, scene_id) in &scenes {
            let feed_command_id = Uuid::new_v4();
            let boat_command_id = Uuid::new_v4();
            let url = format!("ws://{address}/api/sessions/{session_id}/ws");
            for controller_number in 0..10 {
                let token = format!("load-controller-{}", Uuid::new_v4());
                let csrf = format!("load-csrf-{}", Uuid::new_v4());
                sqlx::query(
                    "INSERT INTO device_grants (id, session_id, participant_id, role, \
                     token_hash, csrf_hash, expires_at, last_heartbeat_at) \
                     VALUES ($1, $2, $3, 'controller', $4, $5, now() + interval '1 hour', now())",
                )
                .bind(Uuid::new_v4())
                .bind(session_id)
                .bind(Uuid::new_v4())
                .bind(crate::access::hash_token(&token).to_vec())
                .bind(crate::access::hash_token(&csrf).to_vec())
                .execute(&pool)
                .await
                .unwrap();
                let mut request = url.as_str().into_client_request().unwrap();
                request
                    .headers_mut()
                    .insert("origin", "https://world.example.test".parse().unwrap());
                request.headers_mut().insert(
                    "cookie",
                    format!("__Host-ldw-controller={token}").parse().unwrap(),
                );
                let (mut socket, _) = timeout(
                    Duration::from_secs(3),
                    tokio_tungstenite::connect_async(request),
                )
                .await
                .unwrap()
                .unwrap();
                socket
                    .send(ClientMessage::text(
                        json!({"type":"hello","csrf":csrf.clone()}).to_string(),
                    ))
                    .await
                    .unwrap();
                let snapshot: Value = serde_json::from_str(
                    timeout(Duration::from_secs(3), socket.next())
                        .await
                        .unwrap()
                        .unwrap()
                        .unwrap()
                        .to_text()
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(snapshot["type"], "snapshot");
                assert_eq!(snapshot["sceneId"], scene_id.to_string());
                assert_eq!(
                    snapshot["entities"].as_array().unwrap().len(),
                    ldw_sim::MAX_FISH
                );
                let client_start = client_start.clone();
                let command_times = command_times.clone();
                let reconnect_url = url.clone();
                controllers.spawn(async move {
                    client_start.wait().await;
                    let mut count = 0;
                    let mut last_tick = 0;
                    let mut accepted = 0;
                    let mut reconnected = false;
                    let mut action_latencies = std::collections::HashMap::new();
                    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
                    let reconnect_at = tokio::time::Instant::now() + Duration::from_secs(6);
                    if controller_number == 0 {
                        sleep(Duration::from_secs(1)).await;
                        for (command_id, interaction_id, point) in [
                            (feed_command_id, "feed", realtime::Point { x: 0.0, y: 0.0 }),
                            (boat_command_id, "boat", realtime::Point { x: 1.0, y: 2.0 }),
                        ] {
                            let command = InteractionCommand {
                                kind: "command".into(),
                                command_id,
                                session_id,
                                scene_id,
                                scene_epoch: 1,
                                interaction_id: interaction_id.into(),
                                point,
                                expires_at: std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap()
                                    .as_millis() as i64 + 8_000,
                            };
                            command_times.lock().unwrap().insert(command_id, tokio::time::Instant::now());
                            socket.send(ClientMessage::text(serde_json::to_string(&command).unwrap()))
                                .await.unwrap();
                        }
                    }
                    while let Some(remaining) =
                        deadline.checked_duration_since(tokio::time::Instant::now())
                    {
                        if controller_number == 9
                            && !reconnected
                            && tokio::time::Instant::now() >= reconnect_at
                        {
                            assert_eq!(action_latencies.len(), 2);
                            socket.close(None).await.unwrap();
                            sleep(Duration::from_millis(300)).await;
                            let mut request = reconnect_url.as_str().into_client_request().unwrap();
                            request.headers_mut().insert("origin", "https://world.example.test".parse().unwrap());
                            request.headers_mut().insert(
                                "cookie",
                                format!("__Host-ldw-controller={token}").parse().unwrap(),
                            );
                            let (replacement, _) = timeout(
                                Duration::from_secs(3),
                                tokio_tungstenite::connect_async(request),
                            ).await.unwrap().unwrap();
                            socket = replacement;
                            socket.send(ClientMessage::text(
                                json!({"type":"hello","csrf":csrf}).to_string(),
                            )).await.unwrap();
                            let snapshot: Value = serde_json::from_str(
                                timeout(Duration::from_secs(3), socket.next())
                                    .await.unwrap().unwrap().unwrap().to_text().unwrap(),
                            ).unwrap();
                            assert_eq!(snapshot["type"], "snapshot");
                            assert_eq!(snapshot["sceneId"], scene_id.to_string());
                            assert_eq!(snapshot["sceneEpoch"], 1);
                            assert!(snapshot["revision"].as_i64().unwrap() >= 3);
                            assert_eq!(snapshot["entities"].as_array().unwrap().len(), ldw_sim::MAX_FISH);
                            assert!(snapshot["pendingInteractions"].as_array().unwrap().is_empty());
                            let actions = snapshot["activeActions"].as_array().unwrap();
                            assert!(actions.iter().any(|action| action["interactionId"] == "boat"));
                            last_tick = snapshot["simulationTick"].as_u64().unwrap();
                            reconnected = true;
                            continue;
                        }
                        let message = match timeout(remaining, socket.next()).await {
                            Ok(Some(Ok(message))) => message,
                            Ok(other) => panic!(
                                "scene {scene_number} Controller {controller_number} closed: {other:?}"
                            ),
                            Err(_) => break,
                        };
                        match message {
                            ClientMessage::Text(text) => {
                                let value: Value = serde_json::from_str(&text).unwrap();
                                if value["type"] == "positions" {
                                    assert_eq!(value["sceneId"], scene_id.to_string());
                                    assert_eq!(
                                        value["positions"].as_array().unwrap().len(),
                                        ldw_sim::MAX_FISH
                                    );
                                    let tick = value["simulationTick"].as_u64().unwrap();
                                    assert!(tick > last_tick);
                                    last_tick = tick;
                                    count += 1;
                                } else if value["type"] == "ack" && controller_number == 0 {
                                    assert_eq!(value["accepted"], true, "{value}");
                                    accepted += 1;
                                } else if value["type"] == "delta" {
                                    assert_eq!(value["sceneId"], scene_id.to_string());
                                    if let Some(applied) = value["event"]["appliedCommandIds"].as_array() {
                                        for command_id in [feed_command_id, boat_command_id] {
                                            if applied.iter().any(|id| id == &command_id.to_string()) {
                                                let sent = command_times.lock().unwrap()[&command_id];
                                                action_latencies.entry(command_id).or_insert(sent.elapsed());
                                            }
                                        }
                                    }
                                }
                            }
                            ClientMessage::Ping(payload) => {
                                socket.send(ClientMessage::Pong(payload)).await.unwrap();
                            }
                            ClientMessage::Close(_) => panic!(
                                "scene {scene_number} Controller {controller_number} closed early"
                            ),
                            _ => {}
                        }
                    }
                    (scene_number, controller_number, count, last_tick, accepted, reconnected, action_latencies)
                });
            }
        }
        client_start.wait().await;
        let started = tokio::time::Instant::now();
        let supervisor = tokio::spawn(run(pool.clone(), receiver, hub));
        let deadline = started + Duration::from_secs(12);
        let mut frame_counts = std::collections::HashMap::<Uuid, usize>::new();
        let mut last_ticks = std::collections::HashMap::<Uuid, u64>::new();
        while let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now()) {
            match timeout(remaining, frames.recv()).await {
                Ok(Ok(frame)) => {
                    assert_eq!(frame.positions.len(), ldw_sim::MAX_FISH);
                    *frame_counts.entry(frame.scene_id).or_default() += 1;
                    last_ticks.insert(frame.scene_id, frame.simulation_tick);
                }
                Ok(Err(error)) => panic!("lost a position frame: {error}"),
                Err(_) => break,
            }
        }
        stop.send(true).unwrap();
        timeout(Duration::from_secs(5), supervisor)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let mut client_frames = 0;
        let mut action_latencies = Vec::new();
        while let Some(result) = controllers.join_next().await {
            let (
                scene_number,
                controller_number,
                count,
                last_tick,
                accepted,
                reconnected,
                observed,
            ) = result.unwrap();
            assert!(
                count >= 18 && last_tick >= 180,
                "scene {scene_number} Controller {controller_number} received {count} frames, last tick {last_tick}"
            );
            client_frames += count;
            assert_eq!(
                observed.len(),
                2,
                "scene {scene_number} Controller {controller_number} missed an applied action"
            );
            if controller_number == 0 {
                assert_eq!(
                    accepted, 2,
                    "scene {scene_number} did not accept both commands"
                );
            }
            if controller_number == 9 {
                assert!(
                    reconnected,
                    "scene {scene_number} missed Controller reconnect"
                );
            }
            action_latencies.extend(observed.into_values());
        }
        server.abort();
        println!("30 Controllers received {client_frames} position frames");
        action_latencies.sort_unstable();
        assert_eq!(action_latencies.len(), 60);
        let p95 = action_latencies[(action_latencies.len() * 95).div_ceil(100) - 1];
        println!(
            "60 feed/boat application deliveries: p95={p95:?}, max={:?}",
            action_latencies.last().unwrap()
        );
        assert!(
            p95 <= Duration::from_millis(500),
            "local action delivery exceeded the p95 target"
        );
        for (scene_number, _, scene_id) in scenes {
            let count = frame_counts.get(&scene_id).copied().unwrap_or_default();
            let last = last_ticks.get(&scene_id).copied().unwrap_or_default();
            let restored = load_scene(&pool, scene_id).await.unwrap();
            println!(
                "scene={scene_number} frames={count} last_frame_tick={last} persisted_tick={} fish={}",
                restored.persisted_tick,
                restored.world.fish().len()
            );
            assert!(count >= 18, "scene {scene_number} stopped sending frames");
            assert!(last >= 180, "scene {scene_number} fell behind real time");
            assert!(restored.world.tick_number() >= last);
            assert_eq!(restored.world.fish().len(), ldw_sim::MAX_FISH);
            assert!(restored.revision >= 1, "feed and boat were not applied");
        }
    }

    #[tokio::test]
    #[ignore = "run against a fresh database with LDW_TEST_SIMULATED_SESSION_LIMIT=3"]
    async fn simulated_session_limit_rejects_fourth_concurrent_start() {
        assert_eq!(
            std::env::var("LDW_TEST_SIMULATED_SESSION_LIMIT").as_deref(),
            Ok("3")
        );
        let pool = crate::test_pool().await;
        let owner = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO accounts (id, login, role, password_hash) \
             VALUES ($1, $2, 'owner', 'test-hash')",
        )
        .bind(owner)
        .bind(format!("capacity-{owner}"))
        .execute(&pool)
        .await
        .unwrap();
        let mut sessions = Vec::new();
        let mut scenes = Vec::new();
        for _ in 0..4 {
            let session = Uuid::new_v4();
            let scene = Uuid::new_v4();
            sqlx::query("INSERT INTO sessions (id, owner_id) VALUES ($1, $2)")
                .bind(session)
                .bind(owner)
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO scenes (id, session_id, world_id, world_version) \
                 VALUES ($1, $2, 'underwater', 1)",
            )
            .bind(scene)
            .bind(session)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("UPDATE sessions SET active_scene_id = $1 WHERE id = $2")
                .bind(scene)
                .bind(session)
                .execute(&pool)
                .await
                .unwrap();
            sessions.push(session);
            scenes.push(scene);
        }
        let tasks = scenes
            .iter()
            .copied()
            .enumerate()
            .map(|(index, scene)| {
                let pool = pool.clone();
                tokio::spawn(async move {
                    let result = publish_first_fish(
                        &pool,
                        scene,
                        Uuid::new_v4(),
                        "coral-fish",
                        "paint-capacity",
                        Point { x: 0.0, y: 0.0 },
                    )
                    .await;
                    (index, result)
                })
            })
            .collect::<Vec<_>>();
        let mut winners = Vec::new();
        let mut loser = None;
        for task in tasks {
            let (index, result) = task.await.unwrap();
            match result {
                Ok(_) => winners.push(index),
                Err(SimulationError::SessionLimit) => {
                    assert!(loser.replace(index).is_none());
                }
                Err(error) => panic!("unexpected publication error: {error}"),
            }
        }
        assert_eq!(winners.len(), 3);
        let loser = loser.unwrap();
        let running: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM sessions s JOIN scenes c ON c.id = s.active_scene_id \
             WHERE s.status = 'running' AND c.state ? 'simulation'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(running, 3);
        let loser_state: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
            .bind(scenes[loser])
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(loser_state.get("simulation").is_none());

        let token = Uuid::new_v4().to_string();
        let token_hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        sqlx::query(
            "INSERT INTO owner_grants (id, account_id, token_hash, csrf_hash, expires_at) \
             VALUES ($1, $2, $3, $3, now() + interval '1 day')",
        )
        .bind(Uuid::new_v4())
        .bind(owner)
        .bind(token_hash.to_vec())
        .execute(&pool)
        .await
        .unwrap();
        let store = AccessStore::new(pool.clone(), [5; 32]);
        let expires_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
            + 8_000;
        let command = InteractionCommand {
            kind: "command".into(),
            command_id: Uuid::new_v4(),
            session_id: sessions[loser],
            scene_id: scenes[loser],
            scene_epoch: 1,
            interaction_id: "feed".into(),
            point: realtime::Point { x: 0.0, y: 0.0 },
            expires_at,
        };
        let rejected =
            realtime::process_command(&store, GrantKind::Owner, &token, sessions[loser], &command)
                .await
                .unwrap();
        assert_eq!(rejected["accepted"], false);
        assert_eq!(rejected["code"], "SIMULATED_SESSION_LIMIT");
        sqlx::query("UPDATE sessions SET status = 'paused' WHERE id = $1")
            .bind(sessions[winners[0]])
            .execute(&pool)
            .await
            .unwrap();
        let replay =
            realtime::process_command(&store, GrantKind::Owner, &token, sessions[loser], &command)
                .await
                .unwrap();
        assert_eq!(replay, rejected);
        let mut next_command = command;
        next_command.command_id = Uuid::new_v4();
        let accepted = realtime::process_command(
            &store,
            GrantKind::Owner,
            &token,
            sessions[loser],
            &next_command,
        )
        .await
        .unwrap();
        assert_eq!(accepted["accepted"], true);
        let new_state: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
            .bind(scenes[loser])
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(new_state.get("simulation").is_some());
        let running: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM sessions s JOIN scenes c ON c.id = s.active_scene_id \
             WHERE s.status = 'running' AND c.state ? 'simulation'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(running, 3);
    }

    #[tokio::test]
    async fn boat_command_moves_threat_and_finishes_durably() {
        let pool = crate::test_pool().await;
        let owner = Uuid::new_v4();
        let grant = Uuid::new_v4();
        let session = Uuid::new_v4();
        let scene_id = Uuid::new_v4();
        let token = Uuid::new_v4().to_string();
        let token_hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        sqlx::query("INSERT INTO accounts (id, login, role, password_hash) VALUES ($1, $2, 'owner', 'test-hash')")
            .bind(owner).bind(format!("boat-{owner}")).execute(&pool).await.unwrap();
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
        let fish_id = Uuid::new_v4();
        publish_first_fish(
            &pool,
            scene_id,
            fish_id,
            "coral-fish",
            "paint-boat",
            Point { x: -5.0, y: 0.0 },
        )
        .await
        .unwrap();
        let store = AccessStore::new(pool.clone(), [5; 32]);
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
            interaction_id: "boat".into(),
            point: realtime::Point { x: 1.0, y: 0.0 },
            expires_at: now_ms + 8_000,
        };
        let ack = realtime::process_command(&store, GrantKind::Owner, &token, session, &command)
            .await
            .unwrap();
        assert_eq!(ack["accepted"], true);
        assert_eq!(
            realtime::process_command(&store, GrantKind::Owner, &token, session, &command)
                .await
                .unwrap(),
            ack
        );
        let next = InteractionCommand {
            command_id: Uuid::new_v4(),
            ..command.clone()
        };
        assert_eq!(
            realtime::process_command(&store, GrantKind::Owner, &token, session, &next)
                .await
                .unwrap()["code"],
            "BOAT_LIMIT"
        );
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
                    assert_eq!(state["activeActions"][0]["interactionId"], "boat");
                    break;
                }
                sleep(Duration::from_millis(15)).await;
            }
        })
        .await
        .unwrap();
        let mut moving_frame = false;
        for _ in 0..8 {
            let frame = timeout(Duration::from_secs(5), frames.recv())
                .await
                .unwrap()
                .unwrap();
            if frame
                .action_positions
                .first()
                .is_some_and(|action| action.position.x > -6.0)
            {
                moving_frame = true;
                break;
            }
        }
        assert!(
            moving_frame,
            "boat must advance in transient position frames"
        );
        timeout(Duration::from_secs(5), async {
            loop {
                let state: Value = sqlx::query_scalar("SELECT state FROM scenes WHERE id = $1")
                    .bind(scene_id)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                if state["activeActions"]
                    .as_array()
                    .is_some_and(|actions| actions.is_empty())
                {
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
        assert!(restored.world.boat().is_none());
        assert!(!restored.world.fish()[0].fleeing);
        let transitions: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM scene_events \
            WHERE scene_id = $1 AND event->>'type' = 'interaction_state'",
        )
        .bind(scene_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(transitions, 2, "start and exit each need one durable event");
        let after_exit = InteractionCommand {
            command_id: Uuid::new_v4(),
            ..command
        };
        assert_eq!(
            realtime::process_command(&store, GrantKind::Owner, &token, session, &after_exit)
                .await
                .unwrap()["code"],
            "BOAT_SCENE_COOLDOWN"
        );
    }

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
        let (stop_final, receiver_final) = watch::channel(false);
        let worker_final = tokio::spawn(run_scene(
            pool.clone(),
            scene_id,
            receiver_final,
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
                    .is_some_and(|actions| actions.is_empty())
                {
                    assert!(
                        state["simulation"]["feed_sources"]
                            .as_array()
                            .unwrap()
                            .is_empty()
                    );
                    break;
                }
                sleep(Duration::from_millis(15)).await;
            }
        })
        .await
        .unwrap();
        stop_final.send(true).unwrap();
        worker_final.await.unwrap().unwrap();
        let transitions: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM scene_events \
            WHERE scene_id = $1 AND event->>'type' = 'interaction_state'",
        )
        .bind(scene_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            transitions >= 3,
            "start, consumption and expiry need revisioned events"
        );

        let participant = Uuid::new_v4();
        let first_controller = Uuid::new_v4().to_string();
        let second_controller = Uuid::new_v4().to_string();
        for token in [&first_controller, &second_controller] {
            let token_hash: [u8; 32] = Sha256::digest(token.as_bytes()).into();
            sqlx::query(
                "INSERT INTO device_grants \
                (id, session_id, participant_id, role, token_hash, csrf_hash, expires_at) \
                VALUES ($1, $2, $3, 'controller', $4, $4, now() + interval '1 day')",
            )
            .bind(Uuid::new_v4())
            .bind(session)
            .bind(participant)
            .bind(token_hash.to_vec())
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "UPDATE scenes SET state = jsonb_set(state, \
            '{interactionLimits,lastSceneFeedMs}', '0'::jsonb, true) WHERE id = $1",
        )
        .bind(scene_id)
        .execute(&pool)
        .await
        .unwrap();
        let fresh_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let first_controller_command = InteractionCommand {
            command_id: Uuid::new_v4(),
            expires_at: fresh_ms + 8_000,
            ..command.clone()
        };
        let accepted = realtime::process_command(
            &store,
            GrantKind::Controller,
            &first_controller,
            session,
            &first_controller_command,
        )
        .await
        .unwrap();
        assert_eq!(accepted["accepted"], true);
        sqlx::query(
            "UPDATE scenes SET state = jsonb_set(state, \
            '{interactionLimits,lastSceneFeedMs}', '0'::jsonb, true) WHERE id = $1",
        )
        .bind(scene_id)
        .execute(&pool)
        .await
        .unwrap();
        let second_controller_command = InteractionCommand {
            command_id: Uuid::new_v4(),
            ..first_controller_command
        };
        let cooldown = realtime::process_command(
            &store,
            GrantKind::Controller,
            &second_controller,
            session,
            &second_controller_command,
        )
        .await
        .unwrap();
        assert_eq!(
            cooldown["code"], "FEED_COOLDOWN",
            "a new grant for the same participant must not bypass cooldown"
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

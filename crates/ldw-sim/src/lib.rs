//! Deterministic, renderer-independent movement core for the fixed side-view world.
//! Positions are logical world units; callers own persistence and authoritative ticks.

use serde::{Deserialize, Serialize};

pub const MAX_FISH: usize = 100;
pub const MAX_OBSTACLES: usize = 16;
pub const TICKS_PER_SECOND: u64 = 20;
const FISH_RADIUS: f32 = 0.18;
const FEED_DETECTION_RADIUS: f32 = 3.75;
const FEED_EATING_RADIUS: f32 = 0.3;
const FEED_DURATION_TICKS: u64 = 300;
const MAX_FEED_SOURCES: usize = 3;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    fn distance_squared(self, other: Self) -> f32 {
        (self.x - other.x).powi(2) + (self.y - other.y).powi(2)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    pub min_x: f32,
    pub max_x: f32,
    pub min_y: f32,
    pub max_y: f32,
}

impl Bounds {
    pub fn contains(self, point: Point, margin: f32) -> bool {
        point.x >= self.min_x + margin
            && point.x <= self.max_x - margin
            && point.y >= self.min_y + margin
            && point.y <= self.max_y - margin
    }
    pub fn valid(self) -> bool {
        [self.min_x, self.max_x, self.min_y, self.max_y]
            .iter()
            .all(|v| v.is_finite())
            && self.max_x - self.min_x > 2.0 * FISH_RADIUS
            && self.max_y - self.min_y > 2.0 * FISH_RADIUS
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Circle {
    pub center: Point,
    pub radius: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fish {
    #[serde(with = "hex_u128")]
    pub id: u128,
    pub position: Point,
    pub target: Point,
    /// World units per second, before the fixed 20 Hz tick.
    pub speed: f32,
    pub heading: Point,
    waypoint: Option<Point>,
    target_generation: u32,
    stuck_ticks: u16,
    #[serde(default)]
    pub feeding: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeedSource {
    pub id: String,
    pub position: Point,
    pub remaining: u8,
    pub expires_at_tick: u64,
    /// Each fish can consume at most one portion from this source.
    pub fed_fish: Vec<String>,
}

mod hex_u128 {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(id: &u128, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("{id:032x}"))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u128, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(D::Error::custom("expected 32 hexadecimal digits"));
        }
        u128::from_str_radix(&value, 16).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimError {
    InvalidBounds,
    InvalidPosition,
    InvalidSpeed,
    DuplicateId,
    FishLimit,
    ObstacleLimit,
    InvalidObstacle,
    UnknownFish,
    InvalidCheckpoint,
    InvalidFeedId,
    FeedLimit,
    DuplicateFeed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorldCheckpoint {
    pub schema_version: u32,
    pub bounds: Bounds,
    pub fish: Vec<Fish>,
    pub obstacles: Vec<Circle>,
    pub tick: u64,
    pub seed: u64,
    #[serde(default)]
    pub feed_sources: Vec<FeedSource>,
}

#[derive(Clone, Debug)]
pub struct World {
    bounds: Bounds,
    fish: Vec<Fish>,
    obstacles: Vec<Circle>,
    tick: u64,
    seed: u64,
    feed_sources: Vec<FeedSource>,
}

impl World {
    pub fn new(bounds: Bounds, seed: u64) -> Result<Self, SimError> {
        if !bounds.valid() {
            return Err(SimError::InvalidBounds);
        }
        Ok(Self {
            bounds,
            fish: Vec::new(),
            obstacles: Vec::new(),
            tick: 0,
            seed,
            feed_sources: Vec::new(),
        })
    }

    pub fn tick_number(&self) -> u64 {
        self.tick
    }

    pub fn checkpoint(&self) -> WorldCheckpoint {
        WorldCheckpoint {
            schema_version: 1,
            bounds: self.bounds,
            fish: self.fish.clone(),
            obstacles: self.obstacles.clone(),
            tick: self.tick,
            seed: self.seed,
            feed_sources: self.feed_sources.clone(),
        }
    }

    pub fn restore(checkpoint: WorldCheckpoint) -> Result<Self, SimError> {
        if checkpoint.schema_version != 1
            || !checkpoint.bounds.valid()
            || checkpoint.fish.len() > MAX_FISH
            || checkpoint.obstacles.len() > MAX_OBSTACLES
            || checkpoint.feed_sources.len() > MAX_FEED_SOURCES
        {
            return Err(SimError::InvalidCheckpoint);
        }
        for obstacle in &checkpoint.obstacles {
            if !obstacle.radius.is_finite()
                || obstacle.radius <= 0.0
                || !checkpoint.bounds.contains(obstacle.center, obstacle.radius)
            {
                return Err(SimError::InvalidCheckpoint);
            }
        }
        let mut seen = std::collections::HashSet::with_capacity(checkpoint.fish.len());
        let mut feed_ids = std::collections::HashSet::new();
        for source in &checkpoint.feed_sources {
            let mut fed = std::collections::HashSet::new();
            if !valid_feed_id(&source.id)
                || !feed_ids.insert(source.id.as_str())
                || !valid_point(source.position, checkpoint.bounds, &checkpoint.obstacles)
                || source.remaining == 0
                || source.remaining > 10
                || source.expires_at_tick <= checkpoint.tick
                || source.fed_fish.len() + source.remaining as usize != 10
                || source
                    .fed_fish
                    .iter()
                    .any(|id| !valid_feed_id(id) || !fed.insert(id))
            {
                return Err(SimError::InvalidCheckpoint);
            }
        }
        for fish in &checkpoint.fish {
            if !seen.insert(fish.id)
                || !fish.speed.is_finite()
                || !(0.1..=3.0).contains(&fish.speed)
                || !valid_point(fish.position, checkpoint.bounds, &checkpoint.obstacles)
                || !valid_point(fish.target, checkpoint.bounds, &checkpoint.obstacles)
                || fish.waypoint.is_some_and(|point| {
                    !valid_point(point, checkpoint.bounds, &checkpoint.obstacles)
                })
                || !fish.heading.x.is_finite()
                || !fish.heading.y.is_finite()
                || fish
                    .feeding
                    .as_ref()
                    .is_some_and(|id| !feed_ids.contains(id.as_str()))
            {
                return Err(SimError::InvalidCheckpoint);
            }
        }
        Ok(Self {
            bounds: checkpoint.bounds,
            fish: checkpoint.fish,
            obstacles: checkpoint.obstacles,
            tick: checkpoint.tick,
            seed: checkpoint.seed,
            feed_sources: checkpoint.feed_sources,
        })
    }
    pub fn fish(&self) -> &[Fish] {
        &self.fish
    }
    pub fn obstacles(&self) -> &[Circle] {
        &self.obstacles
    }
    pub fn feed_sources(&self) -> &[FeedSource] {
        &self.feed_sources
    }

    pub fn start_feed(&mut self, id: &str, position: Point) -> Result<(), SimError> {
        if !valid_feed_id(id) {
            return Err(SimError::InvalidFeedId);
        }
        if !valid_point(position, self.bounds, &self.obstacles) {
            return Err(SimError::InvalidPosition);
        }
        if self.feed_sources.iter().any(|source| source.id == id) {
            return Err(SimError::DuplicateFeed);
        }
        if self.feed_sources.len() >= MAX_FEED_SOURCES {
            return Err(SimError::FeedLimit);
        }
        self.feed_sources.push(FeedSource {
            id: id.to_owned(),
            position,
            remaining: 10,
            expires_at_tick: self.tick.saturating_add(FEED_DURATION_TICKS),
            fed_fish: Vec::new(),
        });
        Ok(())
    }

    pub fn add_obstacle(&mut self, obstacle: Circle) -> Result<(), SimError> {
        if self.obstacles.len() >= MAX_OBSTACLES {
            return Err(SimError::ObstacleLimit);
        }
        if !obstacle.radius.is_finite()
            || obstacle.radius <= 0.0
            || !self.bounds.contains(obstacle.center, obstacle.radius)
        {
            return Err(SimError::InvalidObstacle);
        }
        if self
            .fish
            .iter()
            .any(|fish| point_blocked(fish.position, &[obstacle]))
        {
            return Err(SimError::InvalidObstacle);
        }
        self.obstacles.push(obstacle);
        Ok(())
    }

    pub fn spawn_fish(&mut self, id: u128, position: Point, speed: f32) -> Result<(), SimError> {
        if self.fish.len() >= MAX_FISH {
            return Err(SimError::FishLimit);
        }
        if self.fish.iter().any(|fish| fish.id == id) {
            return Err(SimError::DuplicateId);
        }
        if !speed.is_finite() || !(0.1..=3.0).contains(&speed) {
            return Err(SimError::InvalidSpeed);
        }
        if !position.x.is_finite()
            || !position.y.is_finite()
            || !self.bounds.contains(position, FISH_RADIUS)
            || point_blocked(position, &self.obstacles)
        {
            return Err(SimError::InvalidPosition);
        }
        let target =
            choose_target(self.bounds, &self.obstacles, self.seed, id, 0).unwrap_or(position);
        self.fish.push(Fish {
            id,
            position,
            target,
            speed,
            heading: Point { x: 1.0, y: 0.0 },
            waypoint: None,
            target_generation: 0,
            stuck_ticks: 0,
            feeding: None,
        });
        Ok(())
    }

    pub fn set_target(&mut self, id: u128, target: Point) -> Result<(), SimError> {
        if !target.x.is_finite()
            || !target.y.is_finite()
            || !self.bounds.contains(target, FISH_RADIUS)
            || point_blocked(target, &self.obstacles)
        {
            return Err(SimError::InvalidPosition);
        }
        let fish = self
            .fish
            .iter_mut()
            .find(|fish| fish.id == id)
            .ok_or(SimError::UnknownFish)?;
        fish.target = target;
        fish.waypoint = None;
        fish.stuck_ticks = 0;
        Ok(())
    }

    pub fn step(&mut self) {
        self.tick = self.tick.saturating_add(1);
        self.feed_sources
            .retain(|source| source.remaining > 0 && source.expires_at_tick > self.tick);
        let mut assignments = vec![None; self.fish.len()];
        for (source_index, source) in self.feed_sources.iter().enumerate() {
            let mut candidates: Vec<_> = self
                .fish
                .iter()
                .enumerate()
                .filter(|(index, fish)| {
                    assignments[*index].is_none()
                        && !source.fed_fish.contains(&fish_id(fish.id))
                        && fish.position.distance_squared(source.position)
                            <= FEED_DETECTION_RADIUS.powi(2)
                        && (segment_clear(fish.position, source.position, &self.obstacles)
                            || plan_waypoint(
                                fish.position,
                                source.position,
                                fish.id,
                                self.bounds,
                                &self.obstacles,
                            )
                            .is_some())
                })
                .map(|(index, fish)| {
                    (
                        index,
                        fish.position.distance_squared(source.position),
                        fish.id,
                    )
                })
                .collect();
            candidates.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.2.cmp(&b.2)));
            for (index, _, _) in candidates.into_iter().take(source.remaining as usize) {
                assignments[index] = Some(source_index);
            }
        }
        for (index, assigned) in assignments.iter().enumerate() {
            let fish = &mut self.fish[index];
            fish.feeding = assigned.map(|source_index| self.feed_sources[source_index].id.clone());
            if let Some(source_index) = assigned {
                let source = &self.feed_sources[*source_index];
                fish.target = feeding_target(
                    fish.position,
                    source.position,
                    fish.id,
                    self.bounds,
                    &self.obstacles,
                )
                .unwrap_or(source.position);
                fish.waypoint = None;
            }
        }
        for fish in &mut self.fish {
            if fish.feeding.is_none()
                && (fish.position.distance_squared(fish.target) < 0.04 || fish.stuck_ticks > 80)
            {
                fish.target_generation = fish.target_generation.wrapping_add(1);
                if let Some(target) = choose_target(
                    self.bounds,
                    &self.obstacles,
                    self.seed,
                    fish.id,
                    fish.target_generation,
                ) {
                    fish.target = target;
                }
                fish.waypoint = None;
                fish.stuck_ticks = 0;
            }
            if fish
                .waypoint
                .is_some_and(|waypoint| fish.position.distance_squared(waypoint) < 0.04)
            {
                fish.waypoint = None;
            }
            if fish.waypoint.is_none()
                && !segment_clear(fish.position, fish.target, &self.obstacles)
            {
                fish.waypoint = plan_waypoint(
                    fish.position,
                    fish.target,
                    fish.id,
                    self.bounds,
                    &self.obstacles,
                );
            }
            let destination = fish.waypoint.unwrap_or(fish.target);
            let delta = Point {
                x: destination.x - fish.position.x,
                y: destination.y - fish.position.y,
            };
            let distance = (delta.x * delta.x + delta.y * delta.y).sqrt();
            if distance <= f32::EPSILON {
                continue;
            }
            let desired = Point {
                x: delta.x / distance,
                y: delta.y / distance,
            };
            let step = (fish.speed / TICKS_PER_SECOND as f32).min(distance);
            let left_first = fish.id & 1 == 0;
            let directions = if left_first {
                [
                    (1.0, 0.0),
                    (0.70710677, 0.70710677),
                    (0.0, 1.0),
                    (0.70710677, -0.70710677),
                    (0.0, -1.0),
                    (-0.70710677, 0.70710677),
                    (-0.70710677, -0.70710677),
                ]
            } else {
                [
                    (1.0, 0.0),
                    (0.70710677, -0.70710677),
                    (0.0, -1.0),
                    (0.70710677, 0.70710677),
                    (0.0, 1.0),
                    (-0.70710677, -0.70710677),
                    (-0.70710677, 0.70710677),
                ]
            };
            let mut moved = false;
            for (forward, side) in directions {
                let direction = Point {
                    x: forward * desired.x - side * desired.y,
                    y: forward * desired.y + side * desired.x,
                };
                let next = Point {
                    x: fish.position.x + direction.x * step,
                    y: fish.position.y + direction.y * step,
                };
                if self.bounds.contains(next, FISH_RADIUS)
                    && segment_clear(fish.position, next, &self.obstacles)
                {
                    fish.position = next;
                    fish.heading = direction;
                    fish.stuck_ticks = 0;
                    moved = true;
                    break;
                }
            }
            if !moved {
                fish.stuck_ticks = fish.stuck_ticks.saturating_add(1);
            }
        }
        for fish in &mut self.fish {
            let Some(source) = self.feed_sources.iter_mut().find(|source| {
                fish.feeding.as_deref() == Some(source.id.as_str())
                    && fish.position.distance_squared(source.position) <= FEED_EATING_RADIUS.powi(2)
            }) else {
                continue;
            };
            if source.remaining > 0 && !source.fed_fish.contains(&fish_id(fish.id)) {
                source.remaining -= 1;
                source.fed_fish.push(fish_id(fish.id));
                fish.feeding = None;
            }
        }
        self.feed_sources.retain(|source| source.remaining > 0);
        for fish in &mut self.fish {
            if fish
                .feeding
                .as_ref()
                .is_some_and(|id| !self.feed_sources.iter().any(|source| &source.id == id))
            {
                fish.feeding = None;
            }
        }
    }
}

fn fish_id(id: u128) -> String {
    format!("{id:032x}")
}

fn valid_feed_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn feeding_target(
    from: Point,
    position: Point,
    id: u128,
    bounds: Bounds,
    obstacles: &[Circle],
) -> Option<Point> {
    let directions = [
        (1.0, 0.0),
        (0.70710677, 0.70710677),
        (0.0, 1.0),
        (-0.70710677, 0.70710677),
        (-1.0, 0.0),
        (-0.70710677, -0.70710677),
        (0.0, -1.0),
        (0.70710677, -0.70710677),
    ];
    for offset in 0..directions.len() {
        let (dx, dy) = directions[(id as usize + offset) % directions.len()];
        let target = Point {
            x: position.x + dx * 0.24,
            y: position.y + dy * 0.24,
        };
        if valid_point(target, bounds, obstacles)
            && (segment_clear(from, target, obstacles)
                || plan_waypoint(from, target, id, bounds, obstacles).is_some())
        {
            return Some(target);
        }
    }
    None
}

fn point_blocked(point: Point, obstacles: &[Circle]) -> bool {
    obstacles.iter().any(|obstacle| {
        point.distance_squared(obstacle.center) < (obstacle.radius + FISH_RADIUS).powi(2)
    })
}

fn valid_point(point: Point, bounds: Bounds, obstacles: &[Circle]) -> bool {
    point.x.is_finite()
        && point.y.is_finite()
        && bounds.contains(point, FISH_RADIUS)
        && !point_blocked(point, obstacles)
}

fn segment_clear(start: Point, end: Point, obstacles: &[Circle]) -> bool {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let length_sq = dx * dx + dy * dy;
    obstacles.iter().all(|obstacle| {
        let t = if length_sq <= f32::EPSILON {
            0.0
        } else {
            (((obstacle.center.x - start.x) * dx + (obstacle.center.y - start.y) * dy) / length_sq)
                .clamp(0.0, 1.0)
        };
        let closest = Point {
            x: start.x + t * dx,
            y: start.y + t * dy,
        };
        closest.distance_squared(obstacle.center) >= (obstacle.radius + FISH_RADIUS).powi(2)
    })
}

fn plan_waypoint(
    start: Point,
    target: Point,
    id: u128,
    bounds: Bounds,
    obstacles: &[Circle],
) -> Option<Point> {
    let dx = target.x - start.x;
    let dy = target.y - start.y;
    let length = (dx * dx + dy * dy).sqrt();
    if length <= f32::EPSILON {
        return None;
    }
    let perpendicular = Point {
        x: -dy / length,
        y: dx / length,
    };
    for obstacle in obstacles {
        if segment_clear(start, target, &[*obstacle]) {
            continue;
        }
        let sides = if id & 1 == 0 {
            [1.0, -1.0]
        } else {
            [-1.0, 1.0]
        };
        for offset in [0.45, 0.8, 1.2] {
            for side in sides {
                let distance = obstacle.radius + FISH_RADIUS + offset;
                let waypoint = Point {
                    x: obstacle.center.x + perpendicular.x * side * distance,
                    y: obstacle.center.y + perpendicular.y * side * distance,
                };
                if bounds.contains(waypoint, FISH_RADIUS)
                    && segment_clear(start, waypoint, obstacles)
                    && segment_clear(waypoint, target, obstacles)
                {
                    return Some(waypoint);
                }
            }
        }
    }
    None
}

fn choose_target(
    bounds: Bounds,
    obstacles: &[Circle],
    seed: u64,
    id: u128,
    generation: u32,
) -> Option<Point> {
    let folded_id = id as u64 ^ ((id >> 64) as u64).rotate_left(29);
    let mut state = seed
        ^ folded_id.wrapping_mul(0x9e3779b97f4a7c15)
        ^ (generation as u64).wrapping_mul(0xbf58476d1ce4e5b9);
    for _ in 0..16 {
        state = xorshift(state);
        let fx = (state as u32) as f32 / u32::MAX as f32;
        state = xorshift(state);
        let fy = (state as u32) as f32 / u32::MAX as f32;
        let target = Point {
            x: bounds.min_x + FISH_RADIUS + fx * (bounds.max_x - bounds.min_x - 2.0 * FISH_RADIUS),
            y: bounds.min_y + FISH_RADIUS + fy * (bounds.max_y - bounds.min_y - 2.0 * FISH_RADIUS),
        };
        if !point_blocked(target, obstacles) {
            return Some(target);
        }
    }
    None
}

fn xorshift(mut value: u64) -> u64 {
    if value == 0 {
        value = 0x6a09e667f3bcc909;
    }
    value ^= value << 13;
    value ^= value >> 7;
    value ^= value << 17;
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds() -> Bounds {
        Bounds {
            min_x: -7.5,
            max_x: 7.5,
            min_y: -4.0,
            max_y: 4.0,
        }
    }

    #[test]
    fn one_hundred_fish_stay_inside_water_and_replay_deterministically() {
        let mut world = World::new(bounds(), 42).unwrap();
        world
            .add_obstacle(Circle {
                center: Point { x: 0.0, y: 0.0 },
                radius: 0.8,
            })
            .unwrap();
        for index in 0..100 {
            let col = index % 10;
            let row = index / 10;
            let position = Point {
                x: -6.8 + col as f32 * 1.4,
                y: -3.5 + row as f32 * 0.7,
            };
            if point_blocked(position, world.obstacles()) {
                world
                    .spawn_fish(
                        index as u128,
                        Point {
                            x: position.x,
                            y: 2.5,
                        },
                        1.0,
                    )
                    .unwrap();
            } else {
                world.spawn_fish(index as u128, position, 1.0).unwrap();
            }
        }
        assert_eq!(
            world.spawn_fish(101, Point { x: 1.0, y: 1.0 }, 1.0),
            Err(SimError::FishLimit)
        );
        let mut replay = world.clone();
        for _ in 0..200 {
            world.step();
            replay.step();
            assert_eq!(world.fish(), replay.fish());
            for fish in world.fish() {
                assert!(bounds().contains(fish.position, FISH_RADIUS));
                assert!(!point_blocked(fish.position, world.obstacles()));
            }
        }
        assert_eq!(world.tick_number(), 200);
    }

    #[test]
    fn fish_routes_around_a_circular_obstacle() {
        let mut world = World::new(bounds(), 7).unwrap();
        world
            .add_obstacle(Circle {
                center: Point { x: 0.0, y: 0.0 },
                radius: 1.0,
            })
            .unwrap();
        world.spawn_fish(2, Point { x: -3.0, y: 0.0 }, 1.5).unwrap();
        world.set_target(2, Point { x: 3.0, y: 0.0 }).unwrap();
        for _ in 0..180 {
            world.step();
            assert!(!point_blocked(world.fish()[0].position, world.obstacles()));
        }
        assert!(
            world.fish()[0].position.x > 1.0,
            "fish should pass the obstacle instead of orbiting its near side"
        );
    }

    #[test]
    fn rejects_invalid_geometry_and_duplicate_ids() {
        let mut world = World::new(bounds(), 1).unwrap();
        assert_eq!(
            world.spawn_fish(1, Point { x: 99.0, y: 0.0 }, 1.0),
            Err(SimError::InvalidPosition)
        );
        assert_eq!(
            world.spawn_fish(1, Point { x: 0.0, y: 0.0 }, 0.0),
            Err(SimError::InvalidSpeed)
        );
        world.spawn_fish(1, Point { x: 0.0, y: 0.0 }, 1.0).unwrap();
        assert_eq!(
            world.spawn_fish(1, Point { x: 1.0, y: 0.0 }, 1.0),
            Err(SimError::DuplicateId)
        );
        assert_eq!(
            world.add_obstacle(Circle {
                center: Point { x: 0.0, y: 0.0 },
                radius: 0.5
            }),
            Err(SimError::InvalidObstacle)
        );
    }

    #[test]
    fn checkpoint_roundtrip_preserves_route_and_rejects_corruption() {
        let mut original = World::new(bounds(), 91).unwrap();
        original
            .add_obstacle(Circle {
                center: Point { x: 0.0, y: 0.0 },
                radius: 1.0,
            })
            .unwrap();
        let fish_id = u64::MAX as u128 + 7;
        original
            .spawn_fish(fish_id, Point { x: -3.0, y: 0.0 }, 1.2)
            .unwrap();
        original
            .set_target(fish_id, Point { x: 3.0, y: 0.0 })
            .unwrap();
        for _ in 0..30 {
            original.step();
        }
        let saved = serde_json::to_vec(&original.checkpoint()).unwrap();
        let decoded: WorldCheckpoint = serde_json::from_slice(&saved).unwrap();
        let through_json_value: WorldCheckpoint =
            serde_json::from_value(serde_json::to_value(original.checkpoint()).unwrap()).unwrap();
        assert_eq!(through_json_value.fish[0].id, fish_id);
        let mut previous_format = serde_json::to_value(original.checkpoint()).unwrap();
        previous_format
            .as_object_mut()
            .unwrap()
            .remove("feed_sources");
        previous_format["fish"][0]
            .as_object_mut()
            .unwrap()
            .remove("feeding");
        let old: WorldCheckpoint = serde_json::from_value(previous_format).unwrap();
        assert!(World::restore(old).unwrap().feed_sources().is_empty());
        let mut restored = World::restore(decoded.clone()).unwrap();
        assert_eq!(restored.tick_number(), 30);
        for _ in 0..100 {
            original.step();
            restored.step();
            assert_eq!(original.checkpoint(), restored.checkpoint());
        }
        let mut corrupted = decoded.clone();
        corrupted.fish.push(decoded.fish[0].clone());
        assert!(matches!(
            World::restore(corrupted),
            Err(SimError::InvalidCheckpoint)
        ));
        let mut corrupted = decoded;
        corrupted.fish[0].position = Point { x: 99.0, y: 0.0 };
        assert!(matches!(
            World::restore(corrupted),
            Err(SimError::InvalidCheckpoint)
        ));
    }

    #[test]
    fn feed_assigns_nearby_fish_consumes_once_and_replays_after_checkpoint() {
        let mut world = World::new(bounds(), 31).unwrap();
        world.spawn_fish(1, Point { x: -2.0, y: 0.0 }, 1.2).unwrap();
        world.spawn_fish(2, Point { x: 2.0, y: 0.0 }, 1.2).unwrap();
        world.spawn_fish(3, Point { x: 6.0, y: 0.0 }, 1.2).unwrap();
        let id = "000000000000000000000000000000ab";
        world.start_feed(id, Point { x: 0.0, y: 0.0 }).unwrap();
        assert_eq!(
            world.start_feed(id, Point { x: 0.0, y: 0.0 }),
            Err(SimError::DuplicateFeed)
        );
        for _ in 0..10 {
            world.step();
        }
        assert!(world.fish()[0].feeding.is_some());
        assert!(world.fish()[1].feeding.is_some());
        assert_eq!(world.fish()[2].feeding, None);
        let encoded = serde_json::to_vec(&world.checkpoint()).unwrap();
        let mut recovered = World::restore(serde_json::from_slice(&encoded).unwrap()).unwrap();
        for _ in 0..90 {
            world.step();
            recovered.step();
            assert_eq!(world.checkpoint(), recovered.checkpoint());
        }
        assert_eq!(world.feed_sources()[0].remaining, 8);
        assert_eq!(world.feed_sources()[0].fed_fish.len(), 2);
        for _ in 0..200 {
            world.step();
        }
        assert!(world.feed_sources().is_empty());
        assert!(world.fish().iter().all(|fish| fish.feeding.is_none()));
    }

    #[test]
    fn feed_rejects_invalid_points_enforces_three_sources_and_expires_without_fish() {
        let mut world = World::new(bounds(), 2).unwrap();
        for index in 0..3 {
            world
                .start_feed(
                    &format!("{index:032x}"),
                    Point {
                        x: index as f32,
                        y: 0.0,
                    },
                )
                .unwrap();
        }
        assert_eq!(
            world.start_feed("00000000000000000000000000000004", Point { x: 3.0, y: 0.0 }),
            Err(SimError::FeedLimit)
        );
        assert_eq!(
            world.start_feed("invalid", Point { x: 0.0, y: 0.0 }),
            Err(SimError::InvalidFeedId)
        );
        assert_eq!(
            world.start_feed(
                "00000000000000000000000000000005",
                Point { x: 99.0, y: 0.0 }
            ),
            Err(SimError::InvalidPosition)
        );
        for _ in 0..FEED_DURATION_TICKS {
            world.step();
        }
        assert!(world.feed_sources().is_empty());
        world
            .start_feed("00000000000000000000000000000004", Point { x: 0.0, y: 0.0 })
            .unwrap();
    }

    #[test]
    fn feeding_fish_routes_around_obstacle_without_crossing_it() {
        let mut world = World::new(bounds(), 11).unwrap();
        world
            .add_obstacle(Circle {
                center: Point { x: 0.0, y: 0.0 },
                radius: 0.8,
            })
            .unwrap();
        world.spawn_fish(7, Point { x: -1.5, y: 0.0 }, 1.5).unwrap();
        world
            .start_feed("00000000000000000000000000000001", Point { x: 1.5, y: 0.0 })
            .unwrap();
        for _ in 0..180 {
            world.step();
            assert!(!point_blocked(world.fish()[0].position, world.obstacles()));
        }
        assert_eq!(world.feed_sources()[0].remaining, 9);
    }
}

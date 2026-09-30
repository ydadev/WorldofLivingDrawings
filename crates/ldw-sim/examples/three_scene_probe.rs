//! CPU-only diagnostic for ten logical minutes at the initial three-scene capacity.
//! This does not measure PostgreSQL, WebSocket, upload, or browser rendering.

use ldw_sim::{Bounds, MAX_FISH, Point, TICKS_PER_SECOND, World};
use std::time::{Duration, Instant};

const SCENES: usize = 3;
const TICKS: u64 = 10 * 60 * TICKS_PER_SECOND;

fn percentile(sorted: &[Duration], percent: usize) -> Duration {
    sorted[(sorted.len() * percent).div_ceil(100) - 1]
}

fn main() {
    let bounds = Bounds {
        min_x: -7.5,
        max_x: 7.5,
        min_y: -4.0,
        max_y: 4.0,
    };
    let mut worlds = (0..SCENES)
        .map(|scene| {
            let mut world = World::new(bounds, scene as u64 + 1).unwrap();
            for fish in 0..MAX_FISH {
                world
                    .spawn_fish(
                        (scene * MAX_FISH + fish + 1) as u128,
                        Point {
                            x: -6.3 + (fish % 10) as f32 * 1.4,
                            y: -3.3 + (fish / 10) as f32 * 0.73,
                        },
                        1.0 + (fish % 4) as f32 * 0.2,
                    )
                    .unwrap();
            }
            world
        })
        .collect::<Vec<_>>();

    let mut samples = Vec::with_capacity(TICKS as usize);
    let mut feed_count = 0;
    let mut boat_count = 0;
    let mut checkpoint_count = 0;
    let overall = Instant::now();
    for tick in 0..TICKS {
        let started = Instant::now();
        for (scene, world) in worlds.iter_mut().enumerate() {
            if tick % 400 == 0 {
                world
                    .start_feed(
                        &format!("{:032x}", 0x10000000_u64 + tick * 3 + scene as u64),
                        Point { x: 0.0, y: 0.0 },
                    )
                    .unwrap();
                feed_count += 1;
            }
            if tick % 900 == 0 {
                world
                    .start_boat(
                        &format!("{:032x}", 0x20000000_u64 + tick * 3 + scene as u64),
                        Point { x: 1.0, y: 2.0 },
                    )
                    .unwrap();
                boat_count += 1;
            }
            world.step();
            if (tick + 1) % 100 == 0 {
                let before = world.checkpoint();
                let bytes = serde_json::to_vec(&before).unwrap();
                let decoded = serde_json::from_slice(&bytes).unwrap();
                let restored = World::restore(decoded).unwrap();
                assert_eq!(restored.checkpoint(), before);
                *world = restored;
                checkpoint_count += 1;
            }
        }
        samples.push(started.elapsed());
    }
    for world in &worlds {
        assert_eq!(world.tick_number(), TICKS);
        assert_eq!(world.fish().len(), MAX_FISH);
        assert!(world.fish().iter().all(|fish| {
            fish.position.x.is_finite()
                && fish.position.y.is_finite()
                && bounds.contains(fish.position, 0.0)
        }));
    }
    samples.sort_unstable();
    println!(
        "scenes={SCENES} fish_per_scene={MAX_FISH} logical_ticks={TICKS} feeds={feed_count} boats={boat_count} checkpoints={checkpoint_count} wall_ms={} tick_p50_us={} tick_p95_us={} tick_p99_us={} tick_max_us={}",
        overall.elapsed().as_millis(),
        percentile(&samples, 50).as_micros(),
        percentile(&samples, 95).as_micros(),
        percentile(&samples, 99).as_micros(),
        samples.last().unwrap().as_micros(),
    );
}

//! Remove old, unreferenced Paint Textures. Finalization and this collector
//! share a PostgreSQL advisory transaction lock, so a file cannot be installed
//! and linked while the collector decides whether it is orphaned.

use std::{
    collections::HashSet,
    time::{Duration, SystemTime},
};

use sqlx::PgPool;

use crate::blob_store::{BlobStore, BlobStoreError};

pub(crate) const BLOB_CATALOG_LOCK: i64 = 72_111_401;
const ORPHAN_AGE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, thiserror::Error)]
pub enum GcError {
    #[error("blob storage failed: {0}")]
    Storage(#[from] BlobStoreError),
    #[error("database operation failed: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct GcOutcome {
    pub files_deleted: usize,
    pub catalog_rows_deleted: usize,
}

/// A backup must hold the same lock across its DB snapshot and file archive.
/// Until that backup path is available, run this only in isolated maintenance.
pub async fn collect(pool: &PgPool, store: &BlobStore) -> Result<GcOutcome, GcError> {
    let cutoff = SystemTime::now() - ORPHAN_AGE;
    let old_files: HashSet<String> = store.old_blob_ids(cutoff)?.into_iter().collect();
    let mut outcome = GcOutcome::default();
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(BLOB_CATALOG_LOCK)
        .execute(&mut *tx)
        .await?;

    // Failed finalization may have installed a file without committing its
    // catalog row. The locked catalog check protects concurrent finalization.
    for id in &old_files {
        let catalogued: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM paint_blobs WHERE id = $1)")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        if !catalogued && store.remove_blob_if_old(id, cutoff)? {
            outcome.files_deleted += 1;
        }
    }

    // A catalog row may outlive every scene and intent that used its file.
    // Remove its old file first while writers are blocked, then delete the row
    // in this transaction. A rollback can only leave an unreferenced row.
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT b.id FROM paint_blobs b \
         WHERE b.created_at < now() - interval '24 hours' \
           AND NOT EXISTS (SELECT 1 FROM scene_paint_blobs r WHERE r.blob_id = b.id) \
           AND NOT EXISTS (SELECT 1 FROM upload_intents i WHERE i.paint_blob_id = b.id) \
         ORDER BY b.id LIMIT 1000",
    )
    .fetch_all(&mut *tx)
    .await?;
    for id in rows {
        let existed = old_files.contains(&id);
        if !store.remove_blob_if_old(&id, cutoff)? {
            continue;
        }
        let removed = sqlx::query(
            "DELETE FROM paint_blobs b WHERE b.id = $1 \
               AND NOT EXISTS (SELECT 1 FROM scene_paint_blobs r WHERE r.blob_id = b.id) \
               AND NOT EXISTS (SELECT 1 FROM upload_intents i WHERE i.paint_blob_id = b.id)",
        )
        .bind(&id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        outcome.catalog_rows_deleted += removed as usize;
        if existed {
            outcome.files_deleted += 1;
        }
    }
    tx.commit().await?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, File, FileTimes},
        time::{Duration, SystemTime},
    };

    use sqlx::PgPool;
    use uuid::Uuid;

    use super::*;

    fn image(value: u8) -> Vec<u8> {
        let mut pixels = vec![value; 512 * 512 * 4];
        for pixel in pixels.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 512, 512);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&pixels)
                .unwrap();
        }
        crate::paint_image::normalize_png(&png).unwrap()
    }

    fn path(root: &std::path::Path, id: &str) -> std::path::PathBuf {
        root.join("sha256").join(&id[..2]).join(format!("{id}.png"))
    }

    fn age(path: &std::path::Path) {
        File::open(path)
            .unwrap()
            .set_times(
                FileTimes::new()
                    .set_modified(SystemTime::now() - Duration::from_secs(26 * 60 * 60)),
            )
            .unwrap();
    }

    async fn scene(pool: &PgPool, owner: Uuid) -> (Uuid, Uuid) {
        let session = Uuid::new_v4();
        let scene = Uuid::new_v4();
        sqlx::query("INSERT INTO sessions (id, owner_id) VALUES ($1, $2)")
            .bind(session)
            .bind(owner)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO scenes (id, session_id, world_id, world_version) VALUES ($1, $2, 'underwater', 1)")
            .bind(scene).bind(session).execute(pool).await.unwrap();
        (session, scene)
    }

    #[tokio::test]
    async fn collects_only_old_files_without_any_published_reference() {
        let pool = crate::test_pool().await;
        let root = std::env::temp_dir().join(format!("ldw-gc-{}", Uuid::new_v4()));
        let store = BlobStore::create(root.clone()).unwrap();
        let orphan = store.put_normalized(&image(10)).unwrap();
        let recent = store.put_normalized(&image(20)).unwrap();
        let unreferenced = store.put_normalized(&image(30)).unwrap();
        let shared = store.put_normalized(&image(40)).unwrap();
        for id in [&orphan, &unreferenced, &shared] {
            age(&path(&root, id));
        }
        for id in [&unreferenced, &shared] {
            sqlx::query("INSERT INTO paint_blobs (id, byte_size, created_at) VALUES ($1, $2, now() - interval '26 hours')")
                .bind(id).bind(fs::metadata(path(&root, id)).unwrap().len() as i32)
                .execute(&pool).await.unwrap();
        }
        let owner = Uuid::new_v4();
        sqlx::query("INSERT INTO accounts (id, login, role, password_hash) VALUES ($1, $2, 'owner', 'test-only')")
            .bind(owner).bind(format!("gc-{owner}"))
            .execute(&pool).await.unwrap();
        let (session_a, scene_a) = scene(&pool, owner).await;
        let (session_b, scene_b) = scene(&pool, owner).await;
        for scene in [scene_a, scene_b] {
            sqlx::query("INSERT INTO scene_paint_blobs (scene_id, blob_id) VALUES ($1, $2)")
                .bind(scene)
                .bind(&shared)
                .execute(&pool)
                .await
                .unwrap();
        }

        let first = collect(&pool, &store).await.unwrap();
        assert_eq!(first.files_deleted, 2);
        assert_eq!(first.catalog_rows_deleted, 1);
        assert!(!path(&root, &orphan).exists());
        assert!(!path(&root, &unreferenced).exists());
        assert!(path(&root, &recent).exists());
        assert!(path(&root, &shared).exists());
        sqlx::query("DELETE FROM scene_paint_blobs WHERE scene_id = $1 AND blob_id = $2")
            .bind(scene_a)
            .bind(&shared)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(collect(&pool, &store).await.unwrap().files_deleted, 0);
        assert!(path(&root, &shared).exists());
        sqlx::query("DELETE FROM scene_paint_blobs WHERE scene_id = $1 AND blob_id = $2")
            .bind(scene_b)
            .bind(&shared)
            .execute(&pool)
            .await
            .unwrap();
        let last = collect(&pool, &store).await.unwrap();
        assert_eq!(last.files_deleted, 1);
        assert_eq!(last.catalog_rows_deleted, 1);
        assert!(!path(&root, &shared).exists());

        for scene in [scene_a, scene_b] {
            sqlx::query("DELETE FROM scenes WHERE id = $1")
                .bind(scene)
                .execute(&pool)
                .await
                .unwrap();
        }
        for session in [session_a, session_b] {
            sqlx::query("DELETE FROM sessions WHERE id = $1")
                .bind(session)
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query("DELETE FROM accounts WHERE id = $1")
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}

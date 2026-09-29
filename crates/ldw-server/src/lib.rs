use sqlx::{PgPool, migrate::MigrateError};

/// The server uses versioned, embedded migrations; no database credentials live in source.
pub async fn migrate(pool: &PgPool) -> Result<(), MigrateError> {
    sqlx::migrate!("./migrations").run(pool).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[tokio::test]
    async fn migration_keeps_active_scene_inside_its_session() {
        let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL for isolated test database");
        let pool = PgPool::connect(&database_url).await.expect("connect test PostgreSQL");
        migrate(&pool).await.expect("apply migrations");
        migrate(&pool).await.expect("migrations are repeatable");

        let owner = Uuid::new_v4();
        sqlx::query("INSERT INTO accounts (id, login, role, password_hash) VALUES ($1, $2, 'owner', 'test-hash')")
            .bind(owner).bind(format!("test-{}", owner))
            .execute(&pool).await.expect("insert owner");
        let session_a = Uuid::new_v4();
        let session_b = Uuid::new_v4();
        for session in [session_a, session_b] {
            sqlx::query("INSERT INTO sessions (id, owner_id) VALUES ($1, $2)")
                .bind(session).bind(owner).execute(&pool).await.expect("insert session");
        }
        let scene_a = Uuid::new_v4();
        let scene_b = Uuid::new_v4();
        for (scene, session) in [(scene_a, session_a), (scene_b, session_b)] {
            sqlx::query("INSERT INTO scenes (id, session_id, world_id, world_version) VALUES ($1, $2, 'underwater', 1)")
                .bind(scene).bind(session).execute(&pool).await.expect("insert scene");
        }
        let wrong = sqlx::query("UPDATE sessions SET active_scene_id = $1 WHERE id = $2")
            .bind(scene_b).bind(session_a).execute(&pool).await;
        assert!(wrong.is_err(), "a session cannot activate another session’s scene");
        sqlx::query("UPDATE sessions SET active_scene_id = $1 WHERE id = $2")
            .bind(scene_a).bind(session_a).execute(&pool).await.expect("activate own scene");
        let found: Uuid = sqlx::query_scalar("SELECT active_scene_id FROM sessions WHERE id = $1")
            .bind(session_a).fetch_one(&pool).await.expect("read own scene");
        assert_eq!(found, scene_a);
    }
}

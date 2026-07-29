//! Shared SQLite connection configuration.

#![expect(
    clippy::disallowed_methods,
    reason = "this is the centralized SQLite connection shim"
)]

use crate::DbTelemetry;
use crate::migrations::repair_legacy_recency_migration_version;
use crate::runtime::RuntimeDbInitError;
use crate::telemetry;
use crate::telemetry::DbKind;
use codex_utils_absolute_path::AbsolutePathBuf;
use log::LevelFilter;
use sqlx::ConnectOptions;
use sqlx::Error;
use sqlx::SqlitePool;
use sqlx::migrate::Migrator;
use sqlx::sqlite::SqliteAutoVacuum;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::sqlite::SqliteJournalMode;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::sqlite::SqliteSynchronous;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

const LOGS_DB_FILENAME: &str = "logs_2.sqlite";
const GOALS_DB_FILENAME: &str = "goals_1.sqlite";
const MEMORIES_DB_FILENAME: &str = "memories_1.sqlite";
const QUEUE_DB_FILENAME: &str = "queue_1.sqlite";
const STATE_DB_FILENAME: &str = "state_5.sqlite";
const THREAD_HISTORY_DB_FILENAME: &str = "thread_history_1.sqlite";

#[derive(Clone, Copy)]
struct RuntimeDbSpec {
    label: &'static str,
    filename: &'static str,
    kind: DbKind,
    open_phase: &'static str,
    migrate_phase: &'static str,
}

impl RuntimeDbSpec {
    fn path(self, codex_home: &Path) -> PathBuf {
        codex_home.join(self.filename)
    }
}

const STATE_DB: RuntimeDbSpec = RuntimeDbSpec {
    label: "state DB",
    filename: STATE_DB_FILENAME,
    kind: DbKind::State,
    open_phase: "open_state",
    migrate_phase: "migrate_state",
};

const LOGS_DB: RuntimeDbSpec = RuntimeDbSpec {
    label: "log DB",
    filename: LOGS_DB_FILENAME,
    kind: DbKind::Logs,
    open_phase: "open_logs",
    migrate_phase: "migrate_logs",
};

const GOALS_DB: RuntimeDbSpec = RuntimeDbSpec {
    label: "goals DB",
    filename: GOALS_DB_FILENAME,
    kind: DbKind::Goals,
    open_phase: "open_goals",
    migrate_phase: "migrate_goals",
};

const MEMORIES_DB: RuntimeDbSpec = RuntimeDbSpec {
    label: "memories DB",
    filename: MEMORIES_DB_FILENAME,
    kind: DbKind::Memories,
    open_phase: "open_memories",
    migrate_phase: "migrate_memories",
};

const QUEUE_DB: RuntimeDbSpec = RuntimeDbSpec {
    label: "queue DB",
    filename: QUEUE_DB_FILENAME,
    kind: DbKind::Queue,
    open_phase: "open_queue",
    migrate_phase: "migrate_queue",
};

const THREAD_HISTORY_DB: RuntimeDbSpec = RuntimeDbSpec {
    label: "thread history DB",
    filename: THREAD_HISTORY_DB_FILENAME,
    kind: DbKind::ThreadHistory,
    open_phase: "open_thread_history",
    migrate_phase: "migrate_thread_history",
};

const RUNTIME_DBS: [RuntimeDbSpec; 6] = [
    STATE_DB,
    LOGS_DB,
    GOALS_DB,
    MEMORIES_DB,
    QUEUE_DB,
    THREAD_HISTORY_DB,
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeDbPath {
    pub label: &'static str,
    pub path: PathBuf,
}

/// Resolved configuration shared by all Codex SQLite connections.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqliteConfig {
    sqlite_home: AbsolutePathBuf,
}

impl SqliteConfig {
    pub fn from_sqlite_home(sqlite_home: AbsolutePathBuf) -> Self {
        Self { sqlite_home }
    }

    pub fn new_for_testing(sqlite_home: AbsolutePathBuf) -> Self {
        Self::from_sqlite_home(sqlite_home)
    }

    pub fn home(&self) -> &Path {
        self.sqlite_home.as_path()
    }

    /// Return the path to the primary state database.
    pub fn state_db_path(&self) -> PathBuf {
        STATE_DB.path(self.home())
    }

    /// Return the path to the logs database.
    pub fn logs_db_path(&self) -> PathBuf {
        LOGS_DB.path(self.home())
    }

    /// Return the path to the goals database.
    pub fn goals_db_path(&self) -> PathBuf {
        GOALS_DB.path(self.home())
    }

    /// Return the path to the memories database.
    pub fn memories_db_path(&self) -> PathBuf {
        MEMORIES_DB.path(self.home())
    }

    /// Return the path to the durable user-message queue database.
    pub fn queue_db_path(&self) -> PathBuf {
        QUEUE_DB.path(self.home())
    }

    /// Return the path to the paginated thread-history database.
    pub fn thread_history_db_path(&self) -> PathBuf {
        THREAD_HISTORY_DB.path(self.home())
    }

    /// Return the paths to every database managed by the state runtime.
    pub fn runtime_db_paths(&self) -> Vec<RuntimeDbPath> {
        RUNTIME_DBS
            .iter()
            .map(|spec| RuntimeDbPath {
                label: spec.label,
                path: spec.path(self.home()),
            })
            .collect()
    }

    pub(super) async fn open_state_db(
        &self,
        migrator: &Migrator,
        telemetry_override: Option<&dyn DbTelemetry>,
    ) -> anyhow::Result<SqlitePool> {
        // New state DBs should use incremental auto-vacuum, but retrofitting an
        // existing DB requires a full VACUUM. Do not attempt that during process
        // startup: it is maintenance work that can contend with foreground writers.
        self.open_runtime_db(STATE_DB, migrator, telemetry_override)
            .await
    }

    pub(super) async fn open_logs_db(
        &self,
        migrator: &Migrator,
        telemetry_override: Option<&dyn DbTelemetry>,
    ) -> anyhow::Result<SqlitePool> {
        let path = LOGS_DB.path(self.home());
        let started = Instant::now();
        let ready_pool_result = self.try_open_existing_ready_pool(&path, migrator).await;
        telemetry::record_init_result(
            telemetry_override,
            LOGS_DB.kind,
            LOGS_DB.open_phase,
            started.elapsed(),
            &ready_pool_result,
        );
        match ready_pool_result {
            Ok(Some(pool)) => return Ok(pool),
            Ok(None) => {}
            Err(source) => {
                return Err(RuntimeDbInitError::new(
                    LOGS_DB.label,
                    "open ready",
                    path.as_path(),
                    source,
                )
                .into());
            }
        }
        self.open_runtime_db(LOGS_DB, migrator, telemetry_override)
            .await
    }

    pub(super) async fn open_goals_db(
        &self,
        migrator: &Migrator,
        telemetry_override: Option<&dyn DbTelemetry>,
    ) -> anyhow::Result<SqlitePool> {
        self.open_runtime_db(GOALS_DB, migrator, telemetry_override)
            .await
    }

    pub(super) async fn open_memories_db(
        &self,
        migrator: &Migrator,
        telemetry_override: Option<&dyn DbTelemetry>,
    ) -> anyhow::Result<SqlitePool> {
        self.open_runtime_db(MEMORIES_DB, migrator, telemetry_override)
            .await
    }

    pub(super) async fn open_queue_db(
        &self,
        migrator: &Migrator,
        telemetry_override: Option<&dyn DbTelemetry>,
    ) -> anyhow::Result<SqlitePool> {
        self.open_runtime_db(QUEUE_DB, migrator, telemetry_override)
            .await
    }

    pub(super) async fn open_thread_history_db(
        &self,
        migrator: &Migrator,
        telemetry_override: Option<&dyn DbTelemetry>,
    ) -> anyhow::Result<SqlitePool> {
        self.open_runtime_db(THREAD_HISTORY_DB, migrator, telemetry_override)
            .await
    }

    async fn open_runtime_db(
        &self,
        spec: RuntimeDbSpec,
        migrator: &Migrator,
        telemetry_override: Option<&dyn DbTelemetry>,
    ) -> anyhow::Result<SqlitePool> {
        let path = spec.path(self.home());
        let started = Instant::now();
        let pool_result = self
            .open_read_write_pool(&path)
            .await
            .map_err(anyhow::Error::from);
        telemetry::record_init_result(
            telemetry_override,
            spec.kind,
            spec.open_phase,
            started.elapsed(),
            &pool_result,
        );
        let pool = pool_result.map_err(|source| {
            RuntimeDbInitError::new(spec.label, "open", path.as_path(), source)
        })?;
        let started = Instant::now();
        let migrate_result = async {
            if matches!(spec.kind, DbKind::State) {
                repair_legacy_recency_migration_version(&pool, migrator).await?;
            }
            migrator.run(&pool).await.map_err(anyhow::Error::from)
        }
        .await;
        telemetry::record_init_result(
            telemetry_override,
            spec.kind,
            spec.migrate_phase,
            started.elapsed(),
            &migrate_result,
        );
        if let Err(source) = migrate_result {
            pool.close().await;
            return Err(
                RuntimeDbInitError::new(spec.label, "migrate", path.as_path(), source).into(),
            );
        }
        Ok(pool)
    }

    /// Open an existing, fully migrated WAL database without startup writes.
    ///
    /// `Migrator::run` takes SQLite's migration write lock even when every
    /// migration is already applied. Likewise, setting `journal_mode` and
    /// `auto_vacuum` on every connection can contend with active writers.
    /// Logs are opened by every Codex process, so validate the ready schema
    /// using reads and reserve the mutating path for new or stale databases.
    async fn try_open_existing_ready_pool(
        &self,
        path: &Path,
        migrator: &Migrator,
    ) -> anyhow::Result<Option<SqlitePool>> {
        if !tokio::fs::try_exists(path).await? {
            return Ok(None);
        }

        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(false)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(Duration::from_secs(5))
            .log_statements(LevelFilter::Off);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;

        let journal_mode = sqlx::query_scalar::<_, String>("PRAGMA journal_mode")
            .fetch_one(&pool)
            .await?;
        if migrator.table_name.as_ref() != "_sqlx_migrations" {
            pool.close().await;
            return Ok(None);
        }
        let migrations_table_exists = sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
        )
        .fetch_optional(&pool)
        .await?
        .is_some();
        if !journal_mode.eq_ignore_ascii_case("wal") || !migrations_table_exists {
            pool.close().await;
            return Ok(None);
        }

        let applied = sqlx::query_as::<_, (i64, bool, Vec<u8>)>(
            "SELECT version, success, checksum FROM _sqlx_migrations ORDER BY version",
        )
        .fetch_all(&pool)
        .await?;
        let all_applied_succeeded = applied.iter().all(|(_, success, _)| *success);
        let every_embedded_migration_matches = migrator.iter().all(|migration| {
            applied.iter().any(|(version, success, checksum)| {
                *version == migration.version
                    && *success
                    && checksum.as_slice() == migration.checksum.as_ref()
            })
        });
        let no_unknown_migrations = migrator.ignore_missing
            || applied
                .iter()
                .all(|(version, _, _)| migrator.version_exists(*version));
        if !all_applied_succeeded || !every_embedded_migration_matches || !no_unknown_migrations {
            pool.close().await;
            return Ok(None);
        }

        Ok(Some(pool))
    }

    /// Open a writable Codex SQLite database, creating it if necessary.
    pub async fn open_read_write_pool(&self, path: &Path) -> Result<SqlitePool, Error> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .auto_vacuum(SqliteAutoVacuum::Incremental)
            .busy_timeout(Duration::from_secs(5))
            .log_statements(LevelFilter::Off);
        SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
    }

    /// Open an existing Codex SQLite database without creating or modifying it.
    pub async fn open_read_only_pool(&self, path: &Path) -> Result<SqlitePool, Error> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(false)
            .read_only(true)
            .log_statements(LevelFilter::Off);
        SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::SqliteConfig;
    use crate::migrations::runtime_logs_migrator;
    use codex_utils_absolute_path::test_support::PathExt;
    use std::time::Duration;

    #[tokio::test]
    async fn ready_logs_db_opens_without_waiting_for_migration_write_lock() {
        let codex_home =
            std::env::temp_dir().join(format!("codex-ready-logs-test-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&codex_home)
            .await
            .expect("create test Codex home");
        let sqlite = SqliteConfig::new_for_testing(codex_home.as_path().abs());
        let logs_path = sqlite.logs_db_path();
        let initial_pool = sqlite
            .open_logs_db(&runtime_logs_migrator(), /*telemetry_override*/ None)
            .await
            .expect("create migrated logs DB");
        initial_pool.close().await;

        let writer_pool = sqlite
            .open_read_write_pool(&logs_path)
            .await
            .expect("open competing writer");
        let mut writer = writer_pool.acquire().await.expect("acquire writer");
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *writer)
            .await
            .expect("reserve logs DB writer slot");

        let reopened = tokio::time::timeout(
            Duration::from_secs(1),
            sqlite.open_logs_db(&runtime_logs_migrator(), /*telemetry_override*/ None),
        )
        .await
        .expect("ready logs DB open must not wait for the migration write lock")
        .expect("open ready logs DB");
        let table_count =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM sqlite_master WHERE name = 'logs'")
                .fetch_one(&reopened)
                .await
                .expect("query reopened logs DB");
        assert_eq!(table_count, 1);
        reopened.close().await;

        sqlx::query("ROLLBACK")
            .execute(&mut *writer)
            .await
            .expect("release logs DB writer slot");
        drop(writer);
        writer_pool.close().await;
        tokio::fs::remove_dir_all(codex_home)
            .await
            .expect("remove test Codex home");
    }
}

//! Accès SQLite et migrations.
//!
//! La base reste au même format que la version Python : on peut pointer
//! `db_path` sur le fichier `database.sqlite3` existant.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::Connection;

const SCHEMA: &str = include_str!("schema.sql");

/// Poignée vers la base. Chaque opération ouvre sa connexion dans un thread
/// bloquant dédié, pour ne jamais bloquer le runtime async.
#[derive(Clone)]
pub struct Db {
    path: Arc<PathBuf>,
}

impl Db {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: Arc::new(path.into()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn connect(&self) -> Result<Connection> {
        open(&self.path)
    }

    /// Exécute `f` avec une connexion, sur un thread bloquant.
    pub async fn run<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = open(&path)?;
            f(&mut conn)
        })
        .await
        .context("tâche base de données interrompue")?
    }
}

impl Db {
    /// Comme [`Db::run`], dans une transaction validée si `f` réussit.
    pub async fn tx<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    {
        self.run(move |c| {
            let tx = c.transaction()?;
            let out = f(&tx)?;
            tx.commit()?;
            Ok(out)
        })
        .await
    }
}

fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).with_context(|| {
        format!(
            "Impossible de se connecter à la base de données {}",
            path.display()
        )
    })?;
    conn.busy_timeout(Duration::from_secs(10))?;
    Ok(conn)
}

/// Crée les tables, applique les migrations et insère les données par défaut.
pub fn init(db: &Db) -> Result<()> {
    let existed = db.path().exists();
    if existed {
        backup(db.path())?;
    } else {
        tracing::info!("Création de la base de données: {}", db.path().display());
    }

    let mut conn = db.connect()?;
    // WAL : lectures et écritures simultanées (bot + interface web) sans « database is locked ».
    let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
    tracing::debug!("Mode de journal SQLite: {mode}");
    remove_corners_column(&mut conn)?;
    conn.execute_batch(SCHEMA)?;
    ensure_table_columns(&conn)?;
    Ok(())
}

/// Copie la base avant migration (`<db>.<horodatage>.bak`), comme en Python.
fn backup(path: &Path) -> Result<()> {
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let dest = PathBuf::from(format!("{}.{ts}.bak", path.display()));
    // API de sauvegarde SQLite : inclut les données encore dans le journal WAL.
    crate::backup::copy_db(path, &dest)?;
    tracing::info!("Sauvegarde de la base créée: {}", dest.display());
    Ok(())
}

fn columns(conn: &Connection, table: &str) -> Result<Vec<(String, String, Option<String>)>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Supprime l'ancienne colonne `corners` de `users` si elle existe encore.
fn remove_corners_column(conn: &mut Connection) -> Result<()> {
    let cols = columns(conn, "users")?;
    if !cols.iter().any(|(n, _, _)| n == "corners") {
        return Ok(());
    }
    let tx = conn.transaction()?;
    tx.execute_batch(
        "CREATE TABLE users_new (
            guildId TEXT NOT NULL, userId TEXT NOT NULL, xp REAL DEFAULT 0,
            level INTEGER DEFAULT 1, messages INTEGER DEFAULT 0, coins REAL DEFAULT 0,
            PRIMARY KEY (guildId, userId));
         INSERT INTO users_new (guildId, userId, xp, level, messages, coins)
            SELECT guildId, userId, xp, level, messages, coins FROM users;
         DROP TABLE users;
         ALTER TABLE users_new RENAME TO users;",
    )?;
    tx.commit()?;
    tracing::info!("Colonne 'corners' supprimée de la table users");
    Ok(())
}

/// Ajoute les colonnes manquantes aux tables existantes.
///
/// Le schéma attendu est déduit en créant le schéma de référence dans une
/// base en mémoire, ce qui évite de maintenir une seconde liste de colonnes.
fn ensure_table_columns(conn: &Connection) -> Result<()> {
    let reference = Connection::open_in_memory()?;
    reference.execute_batch(SCHEMA)?;
    let tables: Vec<String> = reference
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;

    let mut added = 0;
    for table in tables {
        let existing: Vec<String> = columns(conn, &table)?.into_iter().map(|c| c.0).collect();
        for (name, ty, default) in columns(&reference, &table)? {
            if existing.contains(&name) {
                continue;
            }
            let mut def = if ty.is_empty() {
                "TEXT".to_string()
            } else {
                ty
            };
            // SQLite refuse ADD COLUMN avec un défaut non constant.
            if let Some(d) = default.filter(|d| d != "CURRENT_TIMESTAMP") {
                def = format!("{def} DEFAULT {d}");
            }
            match conn.execute(
                &format!("ALTER TABLE \"{table}\" ADD COLUMN \"{name}\" {def}"),
                [],
            ) {
                Ok(_) => {
                    added += 1;
                    tracing::info!("Colonne manquante ajoutée: {table}.{name}");
                }
                Err(e) => tracing::warn!("Impossible d'ajouter {table}.{name}: {e}"),
            }
        }
    }
    if added > 0 {
        tracing::info!("Migration des colonnes : {added} colonne(s) ajoutée(s)");
    }
    Ok(())
}

/// Date/heure UTC naïve au format ISO, identique à `utcnow().isoformat()` en Python.
pub fn utcnow_iso() -> String {
    chrono::Utc::now()
        .naive_utc()
        .format("%Y-%m-%dT%H:%M:%S%.6f")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db(name: &str) -> Db {
        let dir = std::env::temp_dir().join(format!("isrobot-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Db::new(dir.join("db.sqlite3"))
    }

    #[test]
    fn init_is_idempotent() {
        let db = temp_db("init");
        init(&db).unwrap();
        init(&db).unwrap();
        let conn = db.connect().unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn migrates_legacy_database() {
        let db = temp_db("legacy");
        {
            let conn = db.connect().unwrap();
            conn.execute_batch(
                "CREATE TABLE users (guildId TEXT NOT NULL, userId TEXT NOT NULL, xp REAL DEFAULT 0,
                    level INTEGER DEFAULT 1, messages INTEGER DEFAULT 0, coins REAL DEFAULT 0,
                    corners INTEGER DEFAULT 0, PRIMARY KEY (guildId, userId));
                 INSERT INTO users VALUES ('1', '2', 300, 2, 10, 5, 3);
                 CREATE TABLE youtube_channels (id INTEGER PRIMARY KEY AUTOINCREMENT,
                    channelId TEXT NOT NULL, channelName TEXT NOT NULL, discordChannelId TEXT NOT NULL);",
            )
            .unwrap();
        }
        init(&db).unwrap();
        let conn = db.connect().unwrap();
        let cols: Vec<String> = columns(&conn, "users")
            .unwrap()
            .into_iter()
            .map(|c| c.0)
            .collect();
        assert!(!cols.contains(&"corners".to_string()));
        let xp: f64 = conn
            .query_row("SELECT xp FROM users", [], |r| r.get(0))
            .unwrap();
        assert_eq!(xp, 300.0);
        let yt: Vec<String> = columns(&conn, "youtube_channels")
            .unwrap()
            .into_iter()
            .map(|c| c.0)
            .collect();
        assert!(yt.contains(&"notifyLive".to_string()));
    }
}

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
    remove_corners_column(&mut conn)?;
    conn.execute_batch(SCHEMA)?;
    ensure_table_columns(&conn)?;
    seed_defaults(&mut conn)?;
    Ok(())
}

/// Copie la base avant migration (`<db>.<horodatage>.bak`), comme en Python.
fn backup(path: &Path) -> Result<()> {
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let dest = PathBuf::from(format!("{}.{ts}.bak", path.display()));
    std::fs::copy(path, &dest)?;
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

type QuestSeed = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    i64,
    i64,
    i64,
    i64,
    &'static str,
    &'static str,
);

const DEFAULT_QUESTS: &[QuestSeed] = &[
    (
        "Message Master",
        "Send 10 messages in the server",
        "daily",
        "messages_sent",
        10,
        50,
        25,
        0,
        "common",
        r#"{"channel_type": "any"}"#,
    ),
    (
        "Chatter",
        "Send 25 messages in the server",
        "daily",
        "messages_sent",
        25,
        100,
        50,
        0,
        "uncommon",
        r#"{"channel_type": "any"}"#,
    ),
    (
        "Counter Helper",
        "Participate in the counting minigame 5 times",
        "daily",
        "counting_participation",
        5,
        75,
        30,
        1,
        "common",
        r#"{"requires_counting_channel": true}"#,
    ),
    (
        "Lucky Coin",
        "Use coinflip 3 times",
        "daily",
        "coinflip_used",
        3,
        30,
        15,
        0,
        "common",
        "{}",
    ),
    (
        "Capture Novice",
        "Attempt 3 captures",
        "random",
        "capture_attempt",
        3,
        100,
        50,
        0,
        "uncommon",
        "{}",
    ),
    (
        "Duel Challenger",
        "Challenge someone to a duel",
        "random",
        "duel_challenge",
        1,
        75,
        40,
        0,
        "uncommon",
        "{}",
    ),
    (
        "Big Spender",
        "Spend 100 coins in the shop",
        "random",
        "coins_spent",
        100,
        50,
        25,
        0,
        "rare",
        "{}",
    ),
    (
        "Social Butterfly",
        "Send 50 messages",
        "random",
        "messages_sent",
        50,
        200,
        100,
        0,
        "rare",
        "{}",
    ),
    (
        "Treasure Hunter",
        "Win 200 coins from captures",
        "random",
        "coins_won_capture",
        200,
        150,
        75,
        0,
        "epic",
        "{}",
    ),
];

const DEFAULT_SHOP_ITEMS: &[(&str, &str, i64, i64, i64, i64, &str)] = &[
    (
        "XP Boost (Small)",
        "Gain 25% more XP from messages for 1 hour",
        100,
        0,
        1,
        -1,
        r#"{"effect": "xp_boost", "multiplier": 1.25, "duration_minutes": 60}"#,
    ),
    (
        "XP Boost (Large)",
        "Gain 50% more XP from messages for 1 hour",
        250,
        0,
        1,
        -1,
        r#"{"effect": "xp_boost", "multiplier": 1.50, "duration_minutes": 60}"#,
    ),
    (
        "Capture Luck Charm",
        "Increase capture success rate by 10% for 30 minutes",
        150,
        0,
        1,
        -1,
        r#"{"effect": "capture_luck", "bonus": 0.10, "duration_minutes": 30}"#,
    ),
    (
        "Quest Reroll Token",
        "Reroll one of your daily quests",
        200,
        0,
        1,
        -1,
        r#"{"effect": "quest_reroll", "uses": 1}"#,
    ),
    (
        "Trade Fee Waiver",
        "Waive tax on your next trade",
        300,
        0,
        1,
        -1,
        r#"{"effect": "trade_fee_waiver", "uses": 1}"#,
    ),
    (
        "XP Shield",
        "Protect your XP from loss in the next failed capture",
        100,
        50,
        1,
        -1,
        r#"{"effect": "xp_shield", "uses": 1}"#,
    ),
];

fn seed_defaults(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction()?;
    let quests: i64 = tx.query_row("SELECT COUNT(*) FROM quests", [], |r| r.get(0))?;
    if quests == 0 {
        let mut stmt = tx.prepare(
            "INSERT INTO quests (name, description, type, target_type, target_value,
                reward_coins, reward_xp, allow_other_channels, rarity, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        for q in DEFAULT_QUESTS {
            stmt.execute(rusqlite::params![
                q.0, q.1, q.2, q.3, q.4, q.5, q.6, q.7, q.8, q.9
            ])?;
        }
        tracing::info!("{} quêtes par défaut insérées", DEFAULT_QUESTS.len());
    }
    let items: i64 = tx.query_row("SELECT COUNT(*) FROM shop_items", [], |r| r.get(0))?;
    if items == 0 {
        let mut stmt = tx.prepare(
            "INSERT INTO shop_items (name, description, price_coins, price_xp, consumable, stock, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for i in DEFAULT_SHOP_ITEMS {
            stmt.execute(rusqlite::params![i.0, i.1, i.2, i.3, i.4, i.5, i.6])?;
        }
        tracing::info!(
            "{} articles de boutique par défaut insérés",
            DEFAULT_SHOP_ITEMS.len()
        );
    }
    tx.commit()?;
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
    fn init_is_idempotent_and_seeds_once() {
        let db = temp_db("init");
        init(&db).unwrap();
        init(&db).unwrap();
        let conn = db.connect().unwrap();
        let q: i64 = conn
            .query_row("SELECT COUNT(*) FROM quests", [], |r| r.get(0))
            .unwrap();
        let s: i64 = conn
            .query_row("SELECT COUNT(*) FROM shop_items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(q, DEFAULT_QUESTS.len() as i64);
        assert_eq!(s, DEFAULT_SHOP_ITEMS.len() as i64);
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

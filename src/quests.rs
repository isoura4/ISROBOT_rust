//! Progression des quêtes (portage partiel de `quests.py`).
//!
//! Seule la partie nécessaire aux modules déjà portés est présente :
//! l'incrément de progression. L'attribution/réclamation arrivera avec le
//! portage du minijeu.

use anyhow::Result;
use rusqlite::{Connection, params};

use crate::db::utcnow_iso;

/// Quête terminée par un incrément.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletedQuest {
    pub id: i64,
    pub name: String,
    pub progress: i64,
    pub target: i64,
}

/// Incrémente la progression de toutes les quêtes actives correspondant à `target_type`.
pub fn increment_progress(
    conn: &mut Connection,
    guild_id: &str,
    user_id: &str,
    target_type: &str,
    amount: i64,
) -> Result<Vec<CompletedQuest>> {
    let tx = conn.transaction()?;
    let quests: Vec<(i64, i64, i64, String)> = {
        let mut stmt = tx.prepare(
            "SELECT uq.id, uq.progress, q.target_value, q.name
             FROM user_quests uq
             JOIN quests q ON uq.questId = q.id
             WHERE uq.guildId = ?1 AND uq.userId = ?2
               AND q.target_type = ?3
               AND uq.completed = 0 AND uq.claimed = 0",
        )?;
        stmt.query_map(params![guild_id, user_id, target_type], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?
    };

    let mut completed = Vec::new();
    for (id, progress, target, name) in quests {
        let new_progress = (progress + amount).min(target);
        if new_progress >= target {
            tx.execute(
                "UPDATE user_quests SET progress = ?1, completed = 1, completed_at = ?2 WHERE id = ?3",
                params![new_progress, utcnow_iso(), id],
            )?;
            completed.push(CompletedQuest {
                id,
                name,
                progress: new_progress,
                target,
            });
        } else {
            tx.execute(
                "UPDATE user_quests SET progress = ?1 WHERE id = ?2",
                params![new_progress, id],
            )?;
        }
    }
    tx.commit()?;
    Ok(completed)
}

/// Le salon est-il exclu de la progression des quêtes ?
pub fn is_exception_channel(conn: &Connection, guild_id: &str, channel_id: &str) -> Result<bool> {
    let found = conn
        .prepare("SELECT 1 FROM quest_exception_channels WHERE guildId = ?1 AND channelId = ?2")?
        .exists(params![guild_id, channel_id])?;
    Ok(found)
}

/// Variante async pratique : incrémente et journalise l'erreur éventuelle.
pub async fn track(db: &crate::db::Db, guild_id: u64, user_id: u64, target_type: &'static str) {
    let (g, u) = (guild_id.to_string(), user_id.to_string());
    if let Err(e) = db
        .run(move |c| increment_progress(c, &g, &u, target_type, 1))
        .await
    {
        tracing::error!("Erreur lors de la progression de quête ({target_type}): {e:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Db, init};

    #[test]
    fn progress_and_completion() {
        let dir = std::env::temp_dir().join(format!("isrobot-quests-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::new(dir.join("db.sqlite3"));
        init(&db).unwrap();
        let mut conn = db.connect().unwrap();
        let qid: i64 = conn
            .query_row("SELECT id FROM quests WHERE name = 'Lucky Coin'", [], |r| {
                r.get(0)
            })
            .unwrap();
        conn.execute(
            "INSERT INTO user_quests (guildId, userId, questId) VALUES ('g', 'u', ?1)",
            [qid],
        )
        .unwrap();

        assert!(
            increment_progress(&mut conn, "g", "u", "coinflip_used", 1)
                .unwrap()
                .is_empty()
        );
        assert!(
            increment_progress(&mut conn, "g", "u", "messages_sent", 5)
                .unwrap()
                .is_empty()
        );
        let done = increment_progress(&mut conn, "g", "u", "coinflip_used", 5).unwrap();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].progress, 3);
        // Une quête terminée ne progresse plus.
        assert!(
            increment_progress(&mut conn, "g", "u", "coinflip_used", 1)
                .unwrap()
                .is_empty()
        );
    }
}

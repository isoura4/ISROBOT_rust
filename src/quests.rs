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
    let done = increment_progress_in(&tx, guild_id, user_id, target_type, amount)?;
    tx.commit()?;
    Ok(done)
}

/// Variante sans transaction propre (l'appelant en a déjà ouvert une).
pub fn increment_progress_in(
    tx: &Connection,
    guild_id: &str,
    user_id: &str,
    target_type: &str,
    amount: i64,
) -> Result<Vec<CompletedQuest>> {
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

// ---------------------------------------------------------------------------
// Quêtes quotidiennes, réclamation des récompenses et séries
// ---------------------------------------------------------------------------

use rand::seq::SliceRandom;
use rusqlite::OptionalExtension;

use crate::economy::{self, UserError};

/// Quête attribuée à un utilisateur (jointure user_quests + quests).
#[derive(Debug, Clone)]
pub struct UserQuest {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub progress: i64,
    pub target: i64,
    pub reward_coins: i64,
    pub reward_xp: i64,
    pub completed: bool,
    pub claimed: bool,
}

const USER_QUEST_SELECT: &str = "SELECT uq.id, q.name, q.description, COALESCE(uq.progress, 0), q.target_value,
        COALESCE(q.reward_coins, 0), COALESCE(q.reward_xp, 0), COALESCE(uq.completed, 0), COALESCE(uq.claimed, 0)
     FROM user_quests uq JOIN quests q ON uq.questId = q.id";

fn map_user_quest(r: &rusqlite::Row<'_>) -> rusqlite::Result<UserQuest> {
    Ok(UserQuest {
        id: r.get(0)?,
        name: r.get(1)?,
        description: r.get(2)?,
        progress: r.get(3)?,
        target: r.get(4)?,
        reward_coins: r.get(5)?,
        reward_xp: r.get(6)?,
        completed: r.get::<_, i64>(7)? != 0,
        claimed: r.get::<_, i64>(8)? != 0,
    })
}

/// Quêtes actives (non réclamées), les plus récentes d'abord.
pub fn active_quests(conn: &Connection, guild: &str, user: &str) -> anyhow::Result<Vec<UserQuest>> {
    Ok(conn
        .prepare(&format!(
            "{USER_QUEST_SELECT} WHERE uq.guildId = ?1 AND uq.userId = ?2 AND uq.claimed = 0
             ORDER BY uq.assigned_at DESC"
        ))?
        .query_map(params![guild, user], map_user_quest)?
        .collect::<rusqlite::Result<_>>()?)
}

fn today() -> String {
    chrono::Utc::now().date_naive().to_string()
}

/// Quêtes quotidiennes attribuées aujourd'hui.
pub fn daily_quests(conn: &Connection, guild: &str, user: &str) -> anyhow::Result<Vec<UserQuest>> {
    Ok(conn
        .prepare(&format!(
            "{USER_QUEST_SELECT} WHERE uq.guildId = ?1 AND uq.userId = ?2
               AND date(uq.assigned_at) = ?3 AND q.type = 'daily' ORDER BY uq.assigned_at"
        ))?
        .query_map(params![guild, user, today()], map_user_quest)?
        .collect::<rusqlite::Result<_>>()?)
}

fn quest_pool(conn: &Connection, kind: &str) -> anyhow::Result<Vec<(i64, String)>> {
    Ok(conn
        .prepare("SELECT id, COALESCE(rarity, 'common') FROM quests WHERE type = ?1 AND active = 1 ORDER BY rarity, name")?
        .query_map([kind], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Attribue les quêtes du jour : 1 quête quotidienne garantie (commune de
/// préférence) et jusqu'à 2 quêtes aléatoires bonus (50 % de chance chacune,
/// seulement si aucune quête aléatoire n'est en cours).
pub fn assign_daily_quests(
    conn: &Connection,
    guild: &str,
    user: &str,
) -> anyhow::Result<Vec<UserQuest>> {
    let existing = daily_quests(conn, guild, user)?;
    if !existing.is_empty() {
        return Ok(existing);
    }
    let daily = quest_pool(conn, "daily")?;
    if daily.is_empty() {
        return Ok(vec![]);
    }
    let mut rng = rand::thread_rng();
    let common: Vec<_> = daily
        .iter()
        .filter(|(_, r)| r == "common")
        .cloned()
        .collect();
    let pool = if common.is_empty() { &daily } else { &common };
    let mut chosen = vec![pool.choose(&mut rng).unwrap().0];

    let has_random: i64 = conn.query_row(
        "SELECT COUNT(*) FROM user_quests uq JOIN quests q ON uq.questId = q.id
         WHERE uq.guildId = ?1 AND uq.userId = ?2 AND q.type = 'random' AND uq.claimed = 0",
        params![guild, user],
        |r| r.get(0),
    )?;
    if has_random == 0 {
        let mut random_pool = quest_pool(conn, "random")?;
        for _ in 0..2 {
            if !random_pool.is_empty() && rand::Rng::gen_bool(&mut rng, 0.5) {
                let i = rand::Rng::gen_range(&mut rng, 0..random_pool.len());
                chosen.push(random_pool.remove(i).0);
            }
        }
    }

    let mut ids = Vec::new();
    for quest_id in chosen {
        conn.execute(
            "INSERT INTO user_quests (guildId, userId, questId, progress, completed, claimed, assigned_at)
             VALUES (?1, ?2, ?3, 0, 0, 0, ?4)",
            params![guild, user, quest_id, crate::db::utcnow_iso()],
        )?;
        ids.push(conn.last_insert_rowid());
    }
    let mut out = Vec::new();
    for id in ids {
        out.push(conn.query_row(
            &format!("{USER_QUEST_SELECT} WHERE uq.id = ?1"),
            [id],
            map_user_quest,
        )?);
    }
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct DailyStatus {
    pub quests: Vec<UserQuest>,
    pub completed: usize,
    pub streak: i64,
    pub can_claim_new: bool,
}

impl DailyStatus {
    pub fn all_completed(&self) -> bool {
        !self.quests.is_empty() && self.completed == self.quests.len()
    }
}

pub fn daily_status(conn: &Connection, guild: &str, user: &str) -> anyhow::Result<DailyStatus> {
    let quests = daily_quests(conn, guild, user)?;
    let tracking = economy::daily_tracking(conn, guild, user)?;
    let can_new = tracking
        .last_daily_claim
        .as_deref()
        .and_then(economy::parse_naive)
        .is_none_or(|d| d.date() < chrono::Utc::now().date_naive());
    Ok(DailyStatus {
        completed: quests.iter().filter(|q| q.completed).count(),
        can_claim_new: can_new && quests.is_empty(),
        streak: tracking.streak,
        quests,
    })
}

/// Met à jour la série de jours consécutifs ; renvoie la nouvelle valeur.
pub fn update_streak(conn: &Connection, guild: &str, user: &str) -> anyhow::Result<i64> {
    let tracking = economy::daily_tracking(conn, guild, user)?;
    let today = chrono::Utc::now().date_naive();
    let streak = match tracking
        .last_daily_claim
        .as_deref()
        .and_then(economy::parse_naive)
    {
        Some(last) => match (today - last.date()).num_days() {
            0 => tracking.streak,
            1 => tracking.streak + 1,
            _ => 1,
        },
        None => 1,
    };
    conn.execute(
        "UPDATE user_daily_tracking SET streak = ?1, last_daily_claim = ?2 WHERE guildId = ?3 AND userId = ?4",
        params![streak, crate::db::utcnow_iso(), guild, user],
    )?;
    Ok(streak)
}

pub fn streak_multiplier(streak: i64) -> f64 {
    match streak {
        30.. => 2.5,
        14..=29 => 2.0,
        7..=13 => 1.5,
        _ => 1.0,
    }
}

#[derive(Debug, Clone)]
pub struct ClaimResult {
    pub quest_name: String,
    pub coins: i64,
    pub xp: i64,
    pub new_level: Option<i64>,
}

pub fn claim_quest(
    conn: &Connection,
    guild: &str,
    user: &str,
    user_quest_id: i64,
) -> anyhow::Result<ClaimResult> {
    let q = conn
        .query_row(
            &format!("{USER_QUEST_SELECT} WHERE uq.id = ?1 AND uq.guildId = ?2 AND uq.userId = ?3"),
            params![user_quest_id, guild, user],
            map_user_quest,
        )
        .optional()?;
    let Some(q) = q else {
        return Err(UserError("Quest not found".into()).into());
    };
    if !q.completed {
        return Err(UserError("Quest not yet completed".into()).into());
    }
    if q.claimed {
        return Err(UserError("Quest already claimed".into()).into());
    }
    let related = Some((user_quest_id, "quest"));
    if q.reward_coins > 0 {
        economy::add_coins(
            conn,
            guild,
            user,
            q.reward_coins as f64,
            "quest_reward",
            related,
            None,
        )?;
    }
    let mut new_level = None;
    if q.reward_xp > 0 {
        let c = economy::add_xp(
            conn,
            guild,
            user,
            q.reward_xp as f64,
            "quest_reward",
            related,
        )?;
        if c.level_up() {
            new_level = Some(c.new_level);
        }
    }
    conn.execute(
        "UPDATE user_quests SET claimed = 1 WHERE id = ?1",
        [user_quest_id],
    )?;
    Ok(ClaimResult {
        quest_name: q.name,
        coins: q.reward_coins,
        xp: q.reward_xp,
        new_level,
    })
}

pub fn claim_all(conn: &Connection, guild: &str, user: &str) -> anyhow::Result<Vec<ClaimResult>> {
    let ids: Vec<i64> = conn
        .prepare("SELECT id FROM user_quests WHERE guildId = ?1 AND userId = ?2 AND completed = 1 AND claimed = 0")?
        .query_map(params![guild, user], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out = Vec::new();
    for id in ids {
        match claim_quest(conn, guild, user, id) {
            Ok(r) => out.push(r),
            Err(e) if e.is::<UserError>() => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod daily_tests {
    use super::*;

    #[test]
    fn assign_claim_and_streak() {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch(include_str!("db/schema.sql")).unwrap();
        crate::db::seed_for_tests(&mut c);
        let assigned = assign_daily_quests(&c, "g", "u").unwrap();
        assert!(!assigned.is_empty() && assigned.len() <= 3);
        // Deuxième appel : mêmes quêtes du jour.
        assert_eq!(
            assign_daily_quests(&c, "g", "u").unwrap().len(),
            daily_quests(&c, "g", "u").unwrap().len()
        );
        assert_eq!(update_streak(&c, "g", "u").unwrap(), 1);
        assert_eq!(update_streak(&c, "g", "u").unwrap(), 1);
        assert!(!daily_status(&c, "g", "u").unwrap().can_claim_new);

        let q = &assigned[0];
        assert!(claim_quest(&c, "g", "u", q.id).is_err()); // pas terminée
        c.execute("UPDATE user_quests SET completed = 1 WHERE id = ?1", [q.id])
            .unwrap();
        let claimed = claim_all(&c, "g", "u").unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(
            economy::ensure_user(&c, "g", "u").unwrap().coins,
            q.reward_coins as f64
        );
        assert_eq!(streak_multiplier(7), 1.5);
        assert_eq!(streak_multiplier(30), 2.5);
    }
}

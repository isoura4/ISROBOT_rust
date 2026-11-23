//! XP vocal : 15 à 25 XP par heure complète passée en vocal (hors salon AFK).

use std::time::Duration;

use poise::serenity_prelude as serenity;
use rand::Rng;
use rusqlite::{Connection, OptionalExtension, params};

use crate::bot::Data;
use crate::xp::level_from_xp;

const VOICE_XP_MIN: i64 = 15;
const VOICE_XP_MAX: i64 = 25;
const CHECK_INTERVAL: Duration = Duration::from_secs(300);

fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Ajoute de l'XP vocal sans toucher au compteur de messages.
/// Renvoie `true` en cas de passage de niveau.
pub fn add_voice_xp(
    conn: &Connection,
    guild: &str,
    user: &str,
    gain: f64,
) -> rusqlite::Result<bool> {
    let current = conn
        .query_row(
            "SELECT xp, level FROM users WHERE guildId = ?1 AND userId = ?2",
            params![guild, user],
            |r| Ok((r.get::<_, f64>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?;
    match current {
        Some((xp, level)) => {
            let new_xp = xp + gain;
            let new_level = level_from_xp(new_xp);
            conn.execute(
                "UPDATE users SET xp = ?1, level = ?2 WHERE guildId = ?3 AND userId = ?4",
                params![new_xp, new_level, guild, user],
            )?;
            Ok(new_level > level)
        }
        None => {
            let new_level = level_from_xp(gain);
            conn.execute(
                "INSERT INTO users (guildId, userId, xp, level, messages, coins) VALUES (?1, ?2, ?3, ?4, 0, 0)",
                params![guild, user, gain, new_level],
            )?;
            Ok(new_level > 1)
        }
    }
}

fn afk_channel(guild: &serenity::Guild) -> Option<serenity::ChannelId> {
    guild.afk_metadata.as_ref().map(|m| m.afk_channel_id)
}

/// Mise à jour des sessions à chaque changement d'état vocal.
pub fn on_voice_state_update(ctx: &serenity::Context, data: &Data, new: &serenity::VoiceState) {
    let Some(guild_id) = new.guild_id else { return };
    if guild_id.get() != data.config.server_id {
        return;
    }
    if new.member.as_ref().is_some_and(|m| m.user.bot) {
        return;
    }
    let afk = ctx.cache.guild(guild_id).and_then(|g| afk_channel(&g));
    let eligible = new.channel_id.is_some_and(|c| Some(c) != afk);
    let key = (guild_id.get(), new.user_id.get());
    let mut sessions = data.voice_sessions.lock().unwrap();
    if eligible {
        sessions.entry(key).or_insert_with(now_ts);
    } else {
        sessions.remove(&key);
    }
}

/// Enregistre les membres déjà en vocal puis crédite l'XP toutes les 5 minutes.
pub async fn award_loop(ctx: serenity::Context, data: Data) {
    let guild_id = serenity::GuildId::new(data.config.server_id);
    if let Some(guild) = ctx.cache.guild(guild_id) {
        let afk = afk_channel(&guild);
        let now = now_ts();
        let mut sessions = data.voice_sessions.lock().unwrap();
        for (uid, vs) in &guild.voice_states {
            let is_bot = guild.members.get(uid).is_some_and(|m| m.user.bot);
            if !is_bot && vs.channel_id.is_some_and(|c| Some(c) != afk) {
                sessions.insert((guild_id.get(), uid.get()), now);
            }
        }
    }

    crate::tasks::every("XP vocal", Some("xp_voice"), CHECK_INTERVAL, || {
        award_once(&ctx, &data)
    })
    .await;
}

async fn award_once(ctx: &serenity::Context, data: &Data) {
    let now = now_ts();
    let snapshot: Vec<((u64, u64), i64)> = data
        .voice_sessions
        .lock()
        .unwrap()
        .iter()
        .map(|(k, v)| (*k, *v))
        .collect();

    for ((g, u), last) in snapshot {
        // Toujours en vocal, hors AFK ?
        let state = ctx.cache.guild(serenity::GuildId::new(g)).map(|guild| {
            let afk = afk_channel(&guild);
            guild
                .voice_states
                .get(&serenity::UserId::new(u))
                .and_then(|vs| vs.channel_id)
                .map(|c| Some(c) == afk)
        });
        match state {
            None | Some(None) => {
                data.voice_sessions.lock().unwrap().remove(&(g, u));
                continue;
            }
            Some(Some(true)) => continue, // AFK : pas d'XP
            Some(Some(false)) => {}
        }

        let hours = (now - last) / 3600;
        if hours < 1 {
            continue;
        }
        let gain = (rand::thread_rng().gen_range(VOICE_XP_MIN..=VOICE_XP_MAX) * hours) as f64;
        let (gs, us) = (g.to_string(), u.to_string());
        if let Err(e) = data
            .db
            .run(move |c| Ok(add_voice_xp(c, &gs, &us, gain)?))
            .await
        {
            tracing::error!("Erreur lors de l'attribution d'XP vocal à {u} sur {g}: {e:#}");
            continue;
        }
        // On conserve la fraction d'heure restante.
        if let Some(ts) = data.voice_sessions.lock().unwrap().get_mut(&(g, u)) {
            *ts = last + hours * 3600;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_xp_keeps_message_count() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(include_str!("../db/schema.sql")).unwrap();
        assert!(!add_voice_xp(&c, "g", "u", 20.0).unwrap());
        c.execute("UPDATE users SET messages = 7", []).unwrap();
        assert!(add_voice_xp(&c, "g", "u", 120.0).unwrap());
        let (xp, msgs): (f64, i64) = c
            .query_row("SELECT xp, messages FROM users", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((xp, msgs), (140.0, 7));
    }
}

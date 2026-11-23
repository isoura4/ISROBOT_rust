//! Système d'XP texte : gain d'XP par message, /level et /leaderboard.

use std::time::{Duration, Instant};

use poise::CreateReply;
use poise::serenity_prelude as serenity;
use rand::Rng;
use rusqlite::{Connection, OptionalExtension, params};

use crate::bot::{Context, Data, Error, colors, embed, locale};
use crate::tr;
use crate::xp::{fmt_xp, level_from_xp, xp_for_level};

const COOLDOWN: Duration = Duration::from_secs(60);

#[derive(Debug, PartialEq)]
pub struct XpUpdate {
    pub new_xp: f64,
    pub new_level: i64,
    pub level_up: bool,
    pub messages: i64,
}

/// Ajoute de l'XP à un utilisateur (crée la ligne si besoin).
pub fn add_user_xp(
    conn: &Connection,
    guild_id: &str,
    user_id: &str,
    gain: f64,
) -> rusqlite::Result<XpUpdate> {
    let current = conn
        .query_row(
            "SELECT xp, level, messages FROM users WHERE guildId = ?1 AND userId = ?2",
            params![guild_id, user_id],
            |r| {
                Ok((
                    r.get::<_, f64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;

    match current {
        Some((xp, level, messages)) => {
            let new_xp = xp + gain;
            let new_level = level_from_xp(new_xp);
            conn.execute(
                "UPDATE users SET xp = ?1, level = ?2, messages = ?3 WHERE guildId = ?4 AND userId = ?5",
                params![new_xp, new_level, messages + 1, guild_id, user_id],
            )?;
            Ok(XpUpdate {
                new_xp,
                new_level,
                level_up: new_level > level,
                messages: messages + 1,
            })
        }
        None => {
            let new_level = level_from_xp(gain);
            conn.execute(
                "INSERT INTO users (guildId, userId, xp, level, messages, coins) VALUES (?1, ?2, ?3, ?4, 1, 0)",
                params![guild_id, user_id, gain, new_level],
            )?;
            Ok(XpUpdate {
                new_xp: gain,
                new_level,
                level_up: new_level > 1,
                messages: 1,
            })
        }
    }
}

/// Gain d'XP sur message (uniquement sur le serveur configuré, 1 fois par minute).
pub async fn on_message(
    ctx: &serenity::Context,
    data: &Data,
    msg: &serenity::Message,
) -> Result<(), Error> {
    let Some(guild_id) = msg.guild_id else {
        return Ok(());
    };
    if guild_id.get() != data.config.server_id {
        return Ok(());
    }

    let key = (guild_id.get(), msg.author.id.get());
    {
        let now = Instant::now();
        let mut cds = data.xp_cooldowns.lock().unwrap();
        if cds
            .get(&key)
            .is_some_and(|t| now.duration_since(*t) < COOLDOWN)
        {
            return Ok(());
        }
        cds.insert(key, now);
        if cds.len() > 1000 {
            cds.retain(|_, t| now.duration_since(*t) < COOLDOWN);
        }
    }

    let gain = rand::thread_rng().gen_range(15..=25) as f64;
    let (g, u) = (guild_id.to_string(), msg.author.id.to_string());
    let result = data
        .db
        .run(move |c| Ok(add_user_xp(c, &g, &u, gain)?))
        .await?;

    if result.level_up {
        let e = embed(tr!("xp.levelup.title", None), colors::GOLD)
            .description(tr!(
                "xp.levelup.description",
                None,
                level = result.new_level
            ))
            .field(
                tr!("xp.field_total", None),
                format!("{} XP", fmt_xp(result.new_xp)),
                true,
            )
            .field(
                tr!("xp.field_messages", None),
                tr!("xp.messages_count", None, count = result.messages),
                true,
            );
        let dm = serenity::CreateMessage::new()
            .embed(e)
            .flags(serenity::MessageFlags::SUPPRESS_NOTIFICATIONS);
        if let Err(e) = msg.author.direct_message(ctx, dm).await {
            // MP fermés ou bot bloqué : sans gravité.
            tracing::debug!(
                "Impossible d'envoyer le MP de level up à {}: {e}",
                msg.author.name
            );
        }
    }
    Ok(())
}

/// Affiche le niveau et l'XP d'un utilisateur.
#[poise::command(slash_command, guild_only)]
pub async fn level(ctx: Context<'_>, user: Option<serenity::Member>) -> Result<(), Error> {
    let loc = locale(ctx);
    let self_target = user.is_none();
    let target = match user {
        Some(m) => m,
        None => ctx
            .author_member()
            .await
            .ok_or_else(|| anyhow::anyhow!("membre introuvable"))?
            .into_owned(),
    };

    let (g, u) = (
        ctx.guild_id().unwrap().to_string(),
        target.user.id.to_string(),
    );
    let row = ctx
        .data()
        .db
        .run(move |c| {
            Ok(c.query_row(
                "SELECT xp, level, messages FROM users WHERE guildId = ?1 AND userId = ?2",
                params![g, u],
                |r| {
                    Ok((
                        r.get::<_, f64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?)
        })
        .await?;

    let Some((xp, level, messages)) = row else {
        let text = if self_target {
            tr!("xp.no_xp_self", loc)
        } else {
            tr!(
                "xp.no_xp_other",
                loc,
                user = format!("<@{}>", target.user.id)
            )
        };
        ctx.send(CreateReply::default().content(text).ephemeral(true))
            .await?;
        return Ok(());
    };

    let base = xp_for_level(level);
    let progress = xp - base;
    let needed = xp_for_level(level + 1) - base;
    let filled = ((progress / needed) * 20.0).clamp(0.0, 20.0) as usize;
    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(20 - filled));

    let e = embed(
        tr!("xp.level_title", loc, user = target.display_name()),
        colors::BLUE,
    )
    .thumbnail(target.face())
    .field(tr!("xp.field_level", loc), format!("**{level}**"), true)
    .field(tr!("xp.field_xp", loc), format!("**{}**", fmt_xp(xp)), true)
    .field(
        tr!("xp.field_messages_emoji", loc),
        format!("**{messages}**"),
        true,
    )
    .field(
        tr!("xp.field_progress", loc),
        format!("```{bar}```\n{}/{} XP", fmt_xp(progress), fmt_xp(needed)),
        false,
    );
    ctx.send(CreateReply::default().embed(e)).await?;
    Ok(())
}

/// Affiche le classement des niveaux.
#[poise::command(slash_command, guild_only)]
pub async fn leaderboard(ctx: Context<'_>) -> Result<(), Error> {
    let loc = locale(ctx);
    let guild_id = ctx.guild_id().unwrap();
    let g = guild_id.to_string();
    let rows: Vec<(String, f64, i64, i64)> = ctx
        .data()
        .db
        .run(move |c| {
            let mut stmt = c.prepare(
                "SELECT userId, xp, level, messages FROM users WHERE guildId = ?1 ORDER BY xp DESC LIMIT 10",
            )?;
            let rows = stmt
                .query_map([g], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<_>>()?;
            Ok(rows)
        })
        .await?;

    if rows.is_empty() {
        ctx.send(
            CreateReply::default()
                .content(tr!("xp.leaderboard_empty", loc))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }

    let names: Vec<String> = {
        let guild = ctx.guild();
        rows.iter()
            .map(|(uid, ..)| {
                uid.parse::<u64>()
                    .ok()
                    .and_then(|id| {
                        guild
                            .as_ref()?
                            .members
                            .get(&serenity::UserId::new(id))
                            .map(|m| m.display_name().to_string())
                    })
                    .unwrap_or_else(|| format!("<@{uid}>"))
            })
            .collect()
    };

    let medals = ["🥇", "🥈", "🥉"];
    let text: String = rows
        .iter()
        .zip(names)
        .enumerate()
        .map(|(i, ((_, xp, level, messages), name))| {
            tr!(
                "xp.leaderboard_entry",
                loc,
                medal = medals.get(i).copied().unwrap_or("🏅"),
                name = name,
                level = *level,
                xp = fmt_xp(*xp),
                messages = *messages,
            ) + "\n\n"
        })
        .collect();

    let e = embed(tr!("xp.leaderboard_title", loc), colors::GOLD).description(text);
    ctx.send(CreateReply::default().embed(e)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xp_accumulates_and_levels_up() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(include_str!("../db/schema.sql")).unwrap();
        let first = add_user_xp(&c, "g", "u", 20.0).unwrap();
        assert_eq!(
            first,
            XpUpdate {
                new_xp: 20.0,
                new_level: 1,
                level_up: false,
                messages: 1
            }
        );
        let second = add_user_xp(&c, "g", "u", 110.0).unwrap();
        assert_eq!(
            second,
            XpUpdate {
                new_xp: 130.0,
                new_level: 2,
                level_up: true,
                messages: 2
            }
        );
    }
}

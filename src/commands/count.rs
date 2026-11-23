//! Minijeu du compteur : commande de configuration et logique de jeu.

use poise::CreateReply;
use poise::serenity_prelude as serenity;
use rusqlite::{OptionalExtension, params};

use crate::bot::{Context, Data, Error, locale};
use crate::{quests, tr};

/// Paramétrer le salon pour le minijeu du compteur.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn count(
    ctx: Context<'_>,
    #[channel_types("Text")] channel: serenity::GuildChannel,
) -> Result<(), Error> {
    let loc = locale(ctx);
    let guild_id = ctx.guild_id().unwrap().to_string();
    let channel_id = channel.id.to_string();
    let user_id = ctx.author().id.to_string();

    let created = ctx
        .data()
        .db
        .run(move |c| {
            let exists = c
                .prepare("SELECT 1 FROM counter_game WHERE guildId = ?1 AND channelId = ?2")?
                .exists(params![guild_id, channel_id])?;
            if exists {
                return Ok(false);
            }
            c.execute(
                "INSERT INTO counter_game (guildId, channelId, messageId, userId, lastUserId, count)
                 VALUES (?1, ?2, '', ?3, '0', 0)",
                params![guild_id, channel_id, user_id],
            )?;
            Ok(true)
        })
        .await?;

    let mention = format!("<#{}>", channel.id);
    let key = if created {
        "count.configured"
    } else {
        "count.already"
    };
    ctx.send(
        CreateReply::default()
            .content(tr!(key, loc, channel = mention))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Résultat d'une tentative de comptage.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Correct,
    SameUser,
    SameNumber { expected: i64 },
    WrongNumber { expected: i64 },
}

/// Règles du jeu, isolées pour être testables.
pub fn judge(last_count: i64, last_user: Option<&str>, author: &str, number: i64) -> Outcome {
    if last_user == Some(author) {
        Outcome::SameUser
    } else if number == last_count + 1 {
        Outcome::Correct
    } else if number == last_count {
        Outcome::SameNumber {
            expected: last_count + 1,
        }
    } else {
        Outcome::WrongNumber {
            expected: last_count + 1,
        }
    }
}

/// Traite un message posté dans un éventuel salon de comptage.
pub async fn on_message(
    ctx: &serenity::Context,
    data: &Data,
    msg: &serenity::Message,
) -> Result<(), Error> {
    let Some(guild_id) = msg.guild_id else {
        return Ok(());
    };
    let content = msg.content.as_str();

    // Uniquement des chiffres, et dans des bornes raisonnables.
    if content.is_empty() || !content.chars().all(|c| c.is_ascii_digit()) {
        return Ok(());
    }
    let Ok(number) = content.parse::<i64>() else {
        return Ok(());
    };
    if number > 1_000_000 {
        return Ok(());
    }

    let (g, ch) = (guild_id.to_string(), msg.channel_id.to_string());
    let is_counter = {
        let (g, ch) = (g.clone(), ch.clone());
        data.db
            .run(move |c| {
                Ok(
                    c.prepare("SELECT 1 FROM counter_game WHERE guildId = ?1 AND channelId = ?2")?
                        .exists(params![g, ch])?,
                )
            })
            .await?
    };
    if !is_counter {
        return Ok(());
    }

    let lock = data.counter_lock(guild_id.get(), msg.channel_id.get());
    let _guard = lock.lock().await;

    let state = {
        let (g, ch) = (g.clone(), ch.clone());
        data.db
            .run(move |c| {
                Ok(c.query_row(
                    "SELECT count, lastUserId FROM counter_game WHERE guildId = ?1 AND channelId = ?2",
                    params![g, ch],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)),
                )
                .optional()?)
            })
            .await?
    };
    let Some((last_count, last_user)) = state else {
        return Ok(());
    };

    let author = msg.author.id.to_string();
    let error_text = match judge(last_count, last_user.as_deref(), &author, number) {
        Outcome::Correct => {
            msg.react(ctx, '✅').await?;
            let author2 = author.clone();
            data.db
                .run(move |c| {
                    c.execute(
                        "UPDATE counter_game SET count = ?1, lastUserId = ?2 WHERE guildId = ?3 AND channelId = ?4",
                        params![number, author2, g, ch],
                    )?;
                    Ok(())
                })
                .await?;
            quests::track(
                &data.db,
                guild_id.get(),
                msg.author.id.get(),
                "counting_participation",
            )
            .await;
            return Ok(());
        }
        Outcome::SameUser => tr!("count.same_user", None),
        Outcome::SameNumber { expected } => tr!("count.same_number", None, expected = expected),
        Outcome::WrongNumber { expected } => {
            tr!(
                "count.wrong_number",
                None,
                expected = expected,
                actual = content
            )
        }
    };

    // Erreur : on réinitialise le compteur.
    msg.react(ctx, '❌').await?;
    msg.channel_id.say(ctx, error_text).await?;
    msg.channel_id.say(ctx, tr!("count.reset", None)).await?;
    // Comme en Python : remise à zéro de tous les compteurs du serveur.
    data.db
        .run(move |c| {
            c.execute(
                "UPDATE counter_game SET count = 0, lastUserId = NULL WHERE guildId = ?1",
                params![g],
            )?;
            Ok(())
        })
        .await?;
    msg.channel_id
        .say(ctx, tr!("count.reset_done", None))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules() {
        assert_eq!(judge(0, None, "a", 1), Outcome::Correct);
        assert_eq!(judge(0, Some("0"), "a", 1), Outcome::Correct);
        assert_eq!(judge(4, Some("b"), "a", 5), Outcome::Correct);
        assert_eq!(judge(4, Some("a"), "a", 5), Outcome::SameUser);
        assert_eq!(
            judge(4, Some("b"), "a", 4),
            Outcome::SameNumber { expected: 5 }
        );
        assert_eq!(
            judge(4, Some("b"), "a", 7),
            Outcome::WrongNumber { expected: 5 }
        );
    }
}

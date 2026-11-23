//! Commande /ai et modération assistée par IA des messages.

use poise::CreateReply;
use poise::serenity_prelude as serenity;
use rusqlite::params;
use serde_json::json;

use crate::ai::{self, AiError, Feature};
use crate::bot::{Context, Data, Error, colors};
use crate::moderation::{self as m, now_iso};
use crate::tr;

const SYSTEM_PROMPT: &str = "Tu es un assistant IA respectueux et utile dans un serveur Discord.
Tu DOIS respecter les règles suivantes:
1. Ne jamais générer, décrire ou aider avec du contenu NSFW, explicite, pornographique ou sexuel
2. Ne jamais fournir d'instructions pour des activités illégales (drogue, piratage, violence, etc.)
3. Ne jamais générer de contenu offensant, haineux ou discriminatoire
4. Refuser poliment toute demande inappropriée
5. Toujours rester courtois et constructif

Si une question viole ces règles, réponds simplement: \"Je ne peux pas répondre à cette question car elle viole les règles du serveur.\"
";

fn limit(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn error_embed(title: String, description: String) -> serenity::CreateEmbed {
    serenity::CreateEmbed::new()
        .title(title)
        .description(description)
        .colour(colors::RED)
}

/// Poser une question à l'IA.
#[poise::command(slash_command, guild_only)]
pub async fn ai(ctx: Context<'_>, question: String) -> Result<(), Error> {
    let loc = ctx.locale();
    if !ai::enabled(Feature::Command) {
        let e = serenity::CreateEmbed::new()
            .title(tr!("ai.disabled.title", loc))
            .description(tr!("ai.disabled.command", loc))
            .colour(colors::ORANGE);
        ctx.send(CreateReply::default().embed(e).ephemeral(true))
            .await?;
        return Ok(());
    }
    ctx.defer().await?;

    let ephemeral_embed =
        |e: serenity::CreateEmbed| CreateReply::default().embed(e).ephemeral(true);
    let question = question.trim().to_string();
    if question.is_empty() {
        ctx.send(
            CreateReply::default()
                .content(tr!("ai.empty_question", loc))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }
    let max_q = limit("ai_max_question_length", 500);
    if question.chars().count() > max_q {
        ctx.send(
            CreateReply::default()
                .content(tr!("ai.question_too_long", loc, max = max_q))
                .ephemeral(true),
        )
        .await?;
        return Ok(());
    }
    if ai::contains_inappropriate_content(&question) {
        let e = error_embed(
            tr!("ai.inappropriate_title", loc),
            tr!("ai.inappropriate_desc", loc),
        )
        .footer(serenity::CreateEmbedFooter::new(tr!(
            "ai.inappropriate_footer",
            loc
        )));
        ctx.send(ephemeral_embed(e)).await?;
        return Ok(());
    }

    let messages = json!([
        { "role": "system", "content": SYSTEM_PROMPT },
        { "role": "user", "content": question },
    ]);
    let opts = ai::ChatOptions {
        temperature: Some(0.7),
        ..Default::default()
    };
    let response = match ai::chat(&ctx.data().http, messages, opts).await {
        Ok(r) => r,
        Err(err) => {
            let desc = match &err {
                AiError::Timeout(_) => tr!("ai.timeout_error", loc),
                AiError::Connection(_) => tr!("ai.conn_error", loc),
                AiError::Config(e) => tr!("ai.config_error", loc, error = e),
                AiError::Other(e) => tr!("ai.comm_error", loc, error = e),
            };
            tracing::warn!("Erreur IA: {err}");
            ctx.send(ephemeral_embed(error_embed(
                tr!("ai.error_title", loc),
                desc,
            )))
            .await?;
            return Ok(());
        }
    };
    let Some(answer) = response else {
        ctx.send(ephemeral_embed(error_embed(
            tr!("ai.error_title", loc),
            tr!("ai.invalid_response", loc),
        )))
        .await?;
        return Ok(());
    };
    if ai::contains_inappropriate_content(&answer) {
        let e = error_embed(tr!("ai.filtered_title", loc), tr!("ai.filtered_desc", loc)).footer(
            serenity::CreateEmbedFooter::new(tr!("ai.filtered_footer", loc)),
        );
        ctx.send(ephemeral_embed(e)).await?;
        return Ok(());
    }

    let answer = crate::bot::truncate(&answer, limit("ai_max_response_length", 1024).min(1024));
    let display_name = match ctx.author_member().await {
        Some(mb) => mb.display_name().to_string(),
        None => ctx.author().display_name().to_string(),
    };
    let e = serenity::CreateEmbed::new()
        .title(tr!("ai.response_title", loc))
        .colour(colors::BLUE)
        .field(tr!("ai.field_question", loc), question, false)
        .field(tr!("ai.field_response", loc), answer, false)
        .footer(serenity::CreateEmbedFooter::new(tr!(
            "ai.footer",
            loc,
            model = ai::active_model(),
            user = display_name
        )));
    ctx.send(CreateReply::default().embed(e)).await?;
    Ok(())
}

/// Récupère le texte du message de règles configuré (recherché dans les salons textuels).
async fn server_rules(
    ctx: &serenity::Context,
    guild: serenity::GuildId,
    message_id: &str,
) -> Option<String> {
    let id = serenity::MessageId::new(message_id.parse::<u64>().ok().filter(|n| *n > 0)?);
    let channels: Vec<serenity::ChannelId> = ctx
        .cache
        .guild(guild)?
        .channels
        .values()
        .filter(|c| c.kind == serenity::ChannelType::Text)
        .map(|c| c.id)
        .collect();
    for ch in channels {
        if let Ok(msg) = ch.message(ctx, id).await {
            return if msg.content.is_empty() {
                msg.embeds.first().and_then(|e| e.description.clone())
            } else {
                Some(msg.content)
            };
        }
    }
    tracing::warn!("Message de règles {message_id} introuvable sur le serveur {guild}");
    None
}

fn flag_embed(flag_id: i64, msg: &serenity::Message, a: &ai::Analysis) -> serenity::CreateEmbed {
    let (risk, color) = match a.score {
        80.. => ("ai.risk_high", 0x992d22),
        60..=79 => ("ai.risk_medium", colors::ORANGE),
        40..=59 => ("ai.risk_low", colors::GOLD),
        _ => ("ai.risk_info", colors::BLUE),
    };
    let content = if msg.content.is_empty() {
        tr!("ai.no_text", None)
    } else {
        crate::bot::truncate(&msg.content, 503)
    };
    serenity::CreateEmbed::new()
        .title(tr!("ai.flag_title", None, flag_id = flag_id))
        .description(tr!(
            "ai.flag_risk",
            None,
            risk = tr!(risk, None),
            score = a.score
        ))
        .colour(color)
        .timestamp(serenity::Timestamp::now())
        .field(tr!("ai.field_category", None), a.category.clone(), true)
        .field(
            tr!("ai.field_score", None),
            format!("{}/100", a.score),
            true,
        )
        .field(
            tr!("ai.field_reason", None),
            crate::bot::truncate(&a.reason, 1024),
            false,
        )
        .field(
            tr!("ai.field_author", None),
            format!("<@{}>", msg.author.id),
            true,
        )
        .field(
            tr!("ai.field_channel", None),
            format!("<#{}>", msg.channel_id),
            true,
        )
        .field(
            tr!("ai.field_link", None),
            tr!("ai.field_link_value", None, url = msg.link()),
            true,
        )
        .field(tr!("ai.field_content", None), content, false)
        .footer(serenity::CreateEmbedFooter::new(tr!(
            "ai.flag_footer",
            None
        )))
}

/// Analyse un message avec l'IA et le signale aux modérateurs si besoin.
pub async fn moderate_message(
    ctx: &serenity::Context,
    data: &Data,
    msg: &serenity::Message,
) -> Result<(), Error> {
    if !ai::enabled(Feature::Moderation) || msg.content.is_empty() {
        return Ok(());
    }
    let Some(guild) = msg.guild_id else {
        return Ok(());
    };
    let Some(config) = m::load_config(data, guild).await else {
        return Ok(());
    };
    if !config.ai_enabled {
        return Ok(());
    }

    let rules = match &config.rules_message_id {
        Some(id) => server_rules(ctx, guild, id).await,
        None => None,
    };
    let Some(analysis) = ai::analyze_message(
        &data.http,
        &msg.content,
        rules.as_deref(),
        Some(config.ai_model.clone()),
        Some(config.ollama_host.clone()),
    )
    .await
    else {
        return Ok(());
    };
    if analysis.score < config.ai_confidence_threshold {
        return Ok(());
    }

    let content = crate::bot::truncate(&msg.content, 2003);
    let (g, mid, ch, uid, a) = (
        guild.to_string(),
        msg.id.to_string(),
        msg.channel_id.to_string(),
        msg.author.id.to_string(),
        analysis.clone(),
    );
    let flag_id =
        data.db
            .run(move |c| {
                let exists = c
                    .prepare("SELECT 1 FROM ai_flags WHERE message_id = ?1")?
                    .exists([&mid])?;
                if exists {
                    return Ok(None);
                }
                c.execute(
                "INSERT INTO ai_flags (guild_id, message_id, channel_id, user_id, message_content,
                    ai_score, ai_category, ai_reason, moderator_action, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', ?9)",
                params![g, mid, ch, uid, content, a.score, a.category, a.reason, now_iso()],
            )?;
                Ok(Some(c.last_insert_rowid()))
            })
            .await?;

    let (Some(flag_id), Some(channel)) = (
        flag_id,
        config
            .ai_flag_channel_id
            .as_deref()
            .and_then(|s| s.parse::<u64>().ok()),
    ) else {
        return Ok(());
    };
    serenity::ChannelId::new(channel)
        .send_message(
            ctx,
            serenity::CreateMessage::new().embed(flag_embed(flag_id, msg, &analysis)),
        )
        .await?;
    tracing::info!(
        "Message flagué par l'IA: {} (score: {}, catégorie: {})",
        msg.id,
        analysis.score,
        analysis.category
    );
    Ok(())
}

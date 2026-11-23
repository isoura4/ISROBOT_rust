//! Commande /ai.

use poise::CreateReply;
use poise::serenity_prelude as serenity;
use serde_json::json;

use crate::ai::{self, AiError, Feature};
use crate::bot::{Context, Error, colors};
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
    crate::config::var(key)
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
    // Anti-spam : délai par utilisateur entre deux questions (AI_COOLDOWN_SECONDS).
    let cooldown = std::time::Duration::from_secs(limit("AI_COOLDOWN_SECONDS", 30) as u64);
    let remaining = {
        let cds = ctx.data().ai_cooldowns.lock().unwrap();
        cds.get(&ctx.author().id.get())
            .map(|t| cooldown.saturating_sub(t.elapsed()))
            .filter(|d| !d.is_zero())
    };
    if let Some(rem) = remaining {
        let secs = rem.as_secs().max(1);
        let time = if secs >= 60 { format!("{} min", secs.div_ceil(60)) } else { format!("{secs} s") };
        ctx.send(
            CreateReply::default()
                .content(tr!("errors.cooldown", loc, time = time))
                .ephemeral(true),
        )
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
    if ai::enabled(Feature::ContentFilter) && ai::contains_inappropriate_content(&question) {
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

    {
        let mut cds = ctx.data().ai_cooldowns.lock().unwrap();
        cds.retain(|_, t| t.elapsed() < cooldown);
        cds.insert(ctx.author().id.get(), std::time::Instant::now());
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
    if ai::enabled(Feature::ContentFilter) && ai::contains_inappropriate_content(&answer) {
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

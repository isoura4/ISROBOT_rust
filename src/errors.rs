//! Gestion globale des erreurs de commandes (équivalent de `utils/error_handlers.py`).

use poise::CreateReply;
use poise::serenity_prelude as serenity;

use crate::bot::{Data, Error, colors, embed};
use crate::tr;

/// Classe une erreur en clé `errors.<clé>` des fichiers de traduction.
pub fn classify(error: &Error) -> &'static str {
    for cause in error.chain() {
        if let Some(e) = cause.downcast_ref::<rusqlite::Error>() {
            return match e {
                rusqlite::Error::SqliteFailure(f, _)
                    if matches!(
                        f.code,
                        rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                    ) =>
                {
                    "database_locked"
                }
                _ => "database_error",
            };
        }
        if let Some(serenity::Error::Http(serenity::HttpError::UnsuccessfulRequest(r))) =
            cause.downcast_ref::<serenity::Error>()
            && r.status_code.as_u16() == 403
        {
            return "discord_forbidden";
        }
        if cause
            .downcast_ref::<tokio::time::error::Elapsed>()
            .is_some()
        {
            return "api_timeout";
        }
    }
    "unknown_error"
}

async fn reply_error(ctx: poise::Context<'_, Data, Error>, message: String) {
    let loc = ctx.locale();
    let e = embed(tr!("errors.title", loc), colors::RED)
        .description(message)
        .footer(serenity::CreateEmbedFooter::new(tr!("errors.footer", loc)));
    if let Err(err) = ctx
        .send(CreateReply::default().embed(e).ephemeral(true))
        .await
    {
        tracing::error!("Impossible d'envoyer le message d'erreur: {err}");
    }
}

pub async fn on_error(error: poise::FrameworkError<'_, Data, Error>) {
    match error {
        poise::FrameworkError::Setup { error, .. } => {
            tracing::error!("Échec du démarrage du bot: {error:#}");
            std::process::exit(1);
        }
        poise::FrameworkError::Command { error, ctx, .. } => {
            let key = classify(&error);
            tracing::error!("Erreur dans /{}: [{key}] {error:#}", ctx.command().name);
            reply_error(ctx, tr!(&format!("errors.{key}"), ctx.locale())).await;
        }
        poise::FrameworkError::MissingUserPermissions {
            missing_permissions,
            ctx,
            ..
        } => {
            let missing = missing_permissions
                .map(|p| p.to_string())
                .unwrap_or_default();
            reply_error(
                ctx,
                tr!(
                    "errors.discord_missing_permissions",
                    ctx.locale(),
                    missing = missing
                ),
            )
            .await;
        }
        poise::FrameworkError::MissingBotPermissions {
            missing_permissions,
            ctx,
            ..
        } => {
            reply_error(
                ctx,
                tr!(
                    "errors.discord_bot_missing_permissions",
                    ctx.locale(),
                    missing = missing_permissions.to_string()
                ),
            )
            .await;
        }
        other => {
            if let Err(e) = poise::builtins::on_error(other).await {
                tracing::error!("Erreur lors de la gestion d'une erreur: {e}");
            }
        }
    }
}

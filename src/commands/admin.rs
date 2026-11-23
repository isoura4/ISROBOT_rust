//! Administration : /reload, /feature_list, /feature_enable, /feature_disable.
//!
//! Rust ne recharge pas de code à chaud : /reload relit le `.env` et
//! resynchronise les commandes (les modules désactivés disparaissent, les
//! modules réactivés réapparaissent, sans redémarrer le bot).

use poise::CreateReply;
use poise::serenity_prelude as serenity;

use crate::bot::{Command, Context, Error, colors, reply_ephemeral};
use crate::config;
use crate::features::{FEATURES, feature_env_key, feature_label, is_enabled};
use crate::tr;

pub fn commands() -> Vec<Command> {
    vec![
        reload(),
        feature_list(),
        feature_enable(),
        feature_disable(),
    ]
}

/// Resynchronise les commandes du serveur ; renvoie `true` en cas de succès.
async fn resync(ctx: Context<'_>) -> bool {
    let guild = serenity::GuildId::new(ctx.data().config.server_id);
    match super::sync(ctx.http(), &ctx.framework().options().commands, guild).await {
        Ok(_) => true,
        Err(e) => {
            tracing::error!("Erreur lors de la synchronisation: {e:#}");
            false
        }
    }
}

/// Recharge les commandes du bot.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn reload(ctx: Context<'_>) -> Result<(), Error> {
    let loc = ctx.locale();
    ctx.defer().await?;
    if let Err(e) = config::reload_env(&ctx.data().config.root) {
        tracing::warn!("Impossible de relire .env: {e}");
    }
    let (enabled, disabled): (Vec<_>, Vec<_>) = FEATURES
        .iter()
        .map(|(k, _)| *k)
        .partition(|k| is_enabled(k));
    let synced = resync(ctx).await;

    let mut e = serenity::CreateEmbed::new()
        .title(tr!("reload.title", loc))
        .colour(if synced {
            colors::GREEN
        } else {
            colors::ORANGE
        });
    if !enabled.is_empty() {
        e = e.field(
            tr!("reload.field_reloaded", loc),
            enabled
                .iter()
                .map(|k| format!("• {}", feature_label(k, loc)))
                .collect::<Vec<_>>()
                .join("\n"),
            false,
        );
    }
    if !disabled.is_empty() {
        e = e.field(
            tr!("reload.field_disabled", loc),
            disabled
                .iter()
                .map(|k| format!("• {}", feature_label(k, loc)))
                .collect::<Vec<_>>()
                .join("\n"),
            false,
        );
    }
    e = if synced {
        e.field(
            tr!("reload.field_sync", loc),
            tr!("reload.sync_done", loc),
            false,
        )
    } else {
        e.field(
            tr!("reload.field_sync_warn", loc),
            tr!("reload.sync_error", loc),
            false,
        )
    };
    ctx.send(CreateReply::default().embed(e)).await?;
    Ok(())
}

/// Voir les fonctionnalités activées/désactivées via .env.
#[poise::command(
    slash_command,
    guild_only,
    ephemeral,
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn feature_list(ctx: Context<'_>) -> Result<(), Error> {
    let loc = ctx.locale();
    let mut e = serenity::CreateEmbed::new()
        .title(tr!("reload.features_title", loc))
        .colour(0x5865f2);
    for (key, _) in FEATURES {
        let status = tr!(
            if is_enabled(key) {
                "reload.feature_status_on"
            } else {
                "reload.feature_status_off"
            },
            loc
        );
        e = e.field(
            format!("{} ({key})", feature_label(key, loc)),
            format!("{status}\n`{}`", feature_env_key(key)),
            false,
        );
    }
    ctx.send(CreateReply::default().embed(e).ephemeral(true))
        .await?;
    Ok(())
}

async fn autocomplete_feature(
    ctx: Context<'_>,
    partial: &str,
) -> serenity::CreateAutocompleteResponse {
    let loc = ctx.locale();
    let p = partial.to_lowercase();
    let choices: Vec<serenity::AutocompleteChoice> = FEATURES
        .iter()
        .map(|(k, _)| (*k, feature_label(k, loc)))
        .filter(|(k, label)| k.contains(&p) || label.to_lowercase().contains(&p))
        .take(25)
        .map(|(k, label)| serenity::AutocompleteChoice::new(format!("{k} — {label}"), k))
        .collect();
    serenity::CreateAutocompleteResponse::new().set_choices(choices)
}

async fn toggle(ctx: Context<'_>, feature: String, enabled: bool) -> Result<(), Error> {
    let loc = ctx.locale();
    if !FEATURES.iter().any(|(k, _)| *k == feature) {
        return reply_ephemeral(ctx, tr!("reload.unknown_feature", loc, feature = feature)).await;
    }
    ctx.defer_ephemeral().await?;
    if let Err(e) = config::set_env_key(
        &ctx.data().config.root,
        &feature_env_key(&feature),
        if enabled { "true" } else { "false" },
    ) {
        return reply_ephemeral(
            ctx,
            tr!(
                "reload.apply_failed",
                loc,
                feature = feature,
                error = e.to_string()
            ),
        )
        .await;
    }
    let synced = resync(ctx).await;
    reply_ephemeral(
        ctx,
        tr!(
            "reload.toggle_success",
            loc,
            label = feature_label(&feature, loc),
            state = tr!(
                if enabled {
                    "reload.state_enabled"
                } else {
                    "reload.state_disabled"
                },
                loc
            ),
            sync = tr!(
                if synced {
                    "reload.sync_ok"
                } else {
                    "reload.sync_fail"
                },
                loc
            )
        ),
    )
    .await
}

/// Activer une fonctionnalité et mettre à jour le .env.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn feature_enable(
    ctx: Context<'_>,
    #[autocomplete = "autocomplete_feature"] feature: String,
) -> Result<(), Error> {
    toggle(ctx, feature, true).await
}

/// Désactiver une fonctionnalité et mettre à jour le .env.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn feature_disable(
    ctx: Context<'_>,
    #[autocomplete = "autocomplete_feature"] feature: String,
) -> Result<(), Error> {
    toggle(ctx, feature, false).await
}

//! Commandes de modération : /warn, /warns, /unwarn, /mute, /unmute, /modlog,
//! /modconfig, menu contextuel « Warn User » et /appeal (avec revue par boutons).

use poise::CreateReply;
use poise::serenity_prelude as serenity;

use crate::bot::{Context, Data, Error, colors, escape_user_content, reply_ephemeral, send_dm};
use crate::moderation::{self as m, ModConfig};
use crate::tr;
use crate::ui;

fn guild_name(ctx: Context<'_>) -> String {
    ctx.guild().map(|g| g.name.clone()).unwrap_or_default()
}

/// Résultat d'un avertissement.
struct WarnOutcome {
    count: i64,
    dm_sent: bool,
    action: String,
}

/// Logique commune à /warn et au menu contextuel : avertissement, MP, mutes automatiques.
#[allow(clippy::too_many_arguments)]
async fn issue_warning(
    ctx: &serenity::Context,
    data: &Data,
    guild: serenity::GuildId,
    guild_name: &str,
    target: serenity::UserId,
    moderator: serenity::UserId,
    reason: &str,
    loc: Option<&str>,
    ctx_keys: bool,
) -> Result<(WarnOutcome, Option<ModConfig>), Error> {
    tracing::info!(
        "[AUDIT] ACTION: WARN | ACTOR: {moderator} | TARGET: {target} | REASON: {reason} | GUILD: {guild_name}"
    );
    let (g, u, mo, r) = (
        guild.to_string(),
        target.to_string(),
        moderator.to_string(),
        reason.to_string(),
    );
    let (count, config) = data
        .db
        .run(move |c| {
            let n = m::increment_warning(c, &g, &u, &mo, &r)?;
            Ok((n, m::get_config(c, &g)?))
        })
        .await?;

    let dm_sent = send_dm(
        ctx,
        target,
        m::warning_embed(reason, count, guild_name, None, loc),
    )
    .await;

    let prefix = if ctx_keys { "mod.ctx" } else { "warn" };
    let auto = match count {
        2 => Some((
            "mod.ctx.auto_mute_reason_2",
            config
                .as_ref()
                .map(|c| c.mute_duration_warn_2)
                .unwrap_or(3600),
        )),
        3 => Some((
            "mod.ctx.auto_mute_reason_3",
            config
                .as_ref()
                .map(|c| c.mute_duration_warn_3)
                .unwrap_or(86400),
        )),
        _ => None,
    };
    let mut action = String::new();
    if let Some((reason_key, duration)) = auto {
        let mute_reason = tr!(reason_key, loc);
        m::apply_mute(
            &ctx.http,
            data,
            guild,
            target,
            Some(moderator.to_string()),
            mute_reason.clone(),
            duration,
            true,
        )
        .await?;
        action = tr!(
            &format!("{prefix}.action_auto_mute"),
            loc,
            duration = m::format_duration(duration, loc)
        );
        send_dm(
            ctx,
            target,
            m::mute_embed(&mute_reason, duration, guild_name, loc),
        )
        .await;
    } else if count >= 4 {
        action = tr!(&format!("{prefix}.action_warn4"), loc);
    }
    Ok((
        WarnOutcome {
            count,
            dm_sent,
            action,
        },
        config,
    ))
}

/// Donner un avertissement à un utilisateur.
#[poise::command(
    slash_command,
    guild_only,
    ephemeral,
    default_member_permissions = "MODERATE_MEMBERS",
    required_permissions = "MODERATE_MEMBERS"
)]
pub async fn warn(ctx: Context<'_>, user: serenity::Member, reason: String) -> Result<(), Error> {
    let loc = ctx.locale();
    ctx.defer_ephemeral().await?;
    if user.user.bot {
        return reply_ephemeral(ctx, tr!("mod.ctx.bot", loc)).await;
    }
    if user.user.id == ctx.author().id {
        return reply_ephemeral(ctx, tr!("mod.ctx.self", loc)).await;
    }
    let guild = ctx.guild_id().unwrap();
    let name = guild_name(ctx);
    let result = issue_warning(
        ctx.serenity_context(),
        ctx.data(),
        guild,
        &name,
        user.user.id,
        ctx.author().id,
        &reason,
        loc,
        false,
    )
    .await;
    let (out, config) = match result {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Error in warn command: {e:#}");
            return reply_ephemeral(ctx, tr!("warn.error", loc)).await;
        }
    };
    m::post_modlog(
        ctx.http(),
        config.as_ref(),
        m::modlog_embed(
            "warn",
            user.user.id,
            Some(ctx.author().id),
            &[
                ("reason", reason.clone()),
                ("warn_count", out.count.to_string()),
            ],
            loc,
        ),
    )
    .await;
    let dm = if out.dm_sent {
        tr!("mod.ctx.dm_sent", loc)
    } else {
        tr!("warn.dm_not_sent", loc)
    };
    reply_ephemeral(
        ctx,
        tr!(
            "mod.ctx.warn_issued",
            loc,
            user = format!("<@{}>", user.user.id),
            reason = reason,
            count = out.count,
            dm = dm,
            action = out.action
        ),
    )
    .await
}

fn action_emoji(action: &str) -> &'static str {
    match action {
        "warn_issued" => "⚠️",
        "warn_decreased" => "✅",
        "mute_applied" => "🔇",
        "mute_removed" => "🔊",
        "appeal_created" => "📝",
        "appeal_reviewed" => "⚖️",
        _ => "📌",
    }
}

fn reason_or_default(reason: &Option<String>, loc: Option<&str>) -> String {
    reason
        .as_deref()
        .filter(|r| !r.is_empty())
        .map(escape_user_content)
        .unwrap_or_else(|| tr!("mod.no_reason", loc))
}

/// Voir l'historique des avertissements d'un utilisateur.
#[poise::command(
    slash_command,
    guild_only,
    ephemeral,
    default_member_permissions = "MODERATE_MEMBERS",
    required_permissions = "MODERATE_MEMBERS"
)]
pub async fn warns(ctx: Context<'_>, user: serenity::Member) -> Result<(), Error> {
    let loc = ctx.locale();
    ctx.defer_ephemeral().await?;
    let (g, u) = (
        ctx.guild_id().unwrap().to_string(),
        user.user.id.to_string(),
    );
    let (count, hist, config) = ctx
        .data()
        .db
        .run(move |c| {
            Ok((
                m::warning_count(c, &g, &u)?,
                m::history(c, &g, Some(&u), 10)?,
                m::get_config(c, &g)?,
            ))
        })
        .await?;

    let mut e = serenity::CreateEmbed::new()
        .title(tr!("warns.title", loc, user = user.display_name()))
        .description(tr!("warns.current", loc, count = count))
        .colour(colors::BLUE)
        .timestamp(serenity::Timestamp::now());
    if hist.is_empty() {
        e = e.field(
            tr!("warns.history_field", loc),
            tr!("warns.no_history", loc),
            false,
        );
    } else {
        let text = hist
            .iter()
            .map(|h| {
                format!(
                    "{} **{}** <t:{}:R>\n└─ {}",
                    action_emoji(&h.action),
                    m::title_case(&h.action),
                    m::ts(&h.created_at),
                    reason_or_default(&h.reason, loc)
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        e = e.field(
            tr!("appeal.recent_history", loc),
            crate::bot::truncate(&text, 1024),
            false,
        );
    }
    if count > 0 {
        e = e.field(
            tr!("warns.next_decay", loc),
            tr!(
                "warns.next_decay_value",
                loc,
                days = m::decay_days(count, config.as_ref())
            ),
            true,
        );
    }
    ctx.send(CreateReply::default().embed(e).ephemeral(true))
        .await?;
    Ok(())
}

/// Retire le mute automatique si l'utilisateur n'a plus d'avertissement.
async fn lift_auto_mute_if_clean(
    ctx: &serenity::Context,
    data: &Data,
    guild: serenity::GuildId,
    user: serenity::UserId,
    moderator: Option<String>,
    reason: String,
) {
    let (g, u) = (guild.to_string(), user.to_string());
    let auto = data
        .db
        .run(move |c| Ok(m::active_mute(c, &g, &u)?))
        .await
        .ok()
        .flatten();
    if auto == Some(true) {
        if let Err(e) = m::lift_mute(&ctx.http, data, guild, user, moderator, reason).await {
            tracing::error!("Erreur lors du retrait du timeout: {e:#}");
        }
    }
}

/// Retirer un avertissement à un utilisateur.
#[poise::command(
    slash_command,
    guild_only,
    ephemeral,
    default_member_permissions = "MODERATE_MEMBERS",
    required_permissions = "MODERATE_MEMBERS"
)]
pub async fn unwarn(
    ctx: Context<'_>,
    user: serenity::Member,
    reason: Option<String>,
) -> Result<(), Error> {
    let loc = ctx.locale();
    ctx.defer_ephemeral().await?;
    let guild = ctx.guild_id().unwrap();
    let reason = reason.unwrap_or_else(|| tr!("unwarn.default_reason", loc));
    let (g, u, mo, r) = (
        guild.to_string(),
        user.user.id.to_string(),
        ctx.author().id.to_string(),
        reason.clone(),
    );
    let res = ctx
        .data()
        .db
        .run(move |c| {
            if m::warning_count(c, &g, &u)? <= 0 {
                return Ok(None);
            }
            Ok(Some((
                m::decrement_warning(c, &g, &u, Some(&mo), Some(&r))?,
                m::get_config(c, &g)?,
            )))
        })
        .await;
    let (new_count, config) = match res {
        Ok(Some(v)) => v,
        Ok(None) => {
            return reply_ephemeral(
                ctx,
                tr!("unwarn.none", loc, user = format!("<@{}>", user.user.id)),
            )
            .await;
        }
        Err(e) => {
            tracing::error!("Error in unwarn command: {e:#}");
            return reply_ephemeral(ctx, tr!("unwarn.error", loc)).await;
        }
    };
    if new_count == 0 {
        lift_auto_mute_if_clean(
            ctx.serenity_context(),
            ctx.data(),
            guild,
            user.user.id,
            Some(ctx.author().id.to_string()),
            tr!("unwarn.timeout_reason", loc),
        )
        .await;
    }
    m::post_modlog(
        ctx.http(),
        config.as_ref(),
        m::modlog_embed(
            "unwarn",
            user.user.id,
            Some(ctx.author().id),
            &[("reason", reason), ("warn_count", new_count.to_string())],
            loc,
        ),
    )
    .await;
    reply_ephemeral(
        ctx,
        tr!(
            "unwarn.success",
            loc,
            user = format!("<@{}>", user.user.id),
            count = new_count
        ),
    )
    .await
}

/// Rendre muet un utilisateur pour une durée donnée.
#[poise::command(
    slash_command,
    guild_only,
    ephemeral,
    default_member_permissions = "MODERATE_MEMBERS",
    required_permissions = "MODERATE_MEMBERS"
)]
pub async fn mute(
    ctx: Context<'_>,
    user: serenity::Member,
    duration: String,
    reason: String,
) -> Result<(), Error> {
    let loc = ctx.locale();
    ctx.defer_ephemeral().await?;
    if user.user.bot {
        return reply_ephemeral(ctx, tr!("mute.bot", loc)).await;
    }
    let Some(seconds) = m::parse_duration(&duration) else {
        return reply_ephemeral(ctx, tr!("mute.bad_duration", loc)).await;
    };
    let guild = ctx.guild_id().unwrap();
    if let Err(e) = m::apply_mute(
        ctx.http(),
        ctx.data(),
        guild,
        user.user.id,
        Some(ctx.author().id.to_string()),
        reason.clone(),
        seconds,
        false,
    )
    .await
    {
        tracing::error!("Error in mute command: {e:#}");
        return reply_ephemeral(ctx, tr!("mute.error", loc)).await;
    }
    let dm_sent = send_dm(
        ctx.serenity_context(),
        user.user.id,
        m::mute_embed(&reason, seconds, &guild_name(ctx), loc),
    )
    .await;
    let config = m::load_config(ctx.data(), guild).await;
    m::post_modlog(
        ctx.http(),
        config.as_ref(),
        m::modlog_embed(
            "mute",
            user.user.id,
            Some(ctx.author().id),
            &[
                ("reason", reason.clone()),
                ("duration", m::format_duration(seconds, loc)),
            ],
            loc,
        ),
    )
    .await;
    let dm = tr!(
        if dm_sent {
            "mod.ctx.dm_sent"
        } else {
            "mod.ctx.dm_not_sent"
        },
        loc
    );
    reply_ephemeral(
        ctx,
        tr!(
            "mute.success",
            loc,
            user = format!("<@{}>", user.user.id),
            duration = m::format_duration(seconds, loc),
            reason = reason,
            dm = dm
        ),
    )
    .await
}

/// Lever le mute d'un utilisateur.
#[poise::command(
    slash_command,
    guild_only,
    ephemeral,
    default_member_permissions = "MODERATE_MEMBERS",
    required_permissions = "MODERATE_MEMBERS"
)]
pub async fn unmute(ctx: Context<'_>, user: serenity::Member) -> Result<(), Error> {
    let loc = ctx.locale();
    ctx.defer_ephemeral().await?;
    let guild = ctx.guild_id().unwrap();
    let (g, u) = (guild.to_string(), user.user.id.to_string());
    let muted = ctx
        .data()
        .db
        .run(move |c| Ok(m::active_mute(c, &g, &u)?))
        .await?;
    if muted.is_none() {
        return reply_ephemeral(
            ctx,
            tr!(
                "unmute.not_muted",
                loc,
                user = format!("<@{}>", user.user.id)
            ),
        )
        .await;
    }
    let reason = tr!("unmute.reason", loc);
    if let Err(e) = m::lift_mute(
        ctx.http(),
        ctx.data(),
        guild,
        user.user.id,
        Some(ctx.author().id.to_string()),
        reason.clone(),
    )
    .await
    {
        tracing::error!("Error in unmute command: {e:#}");
        return reply_ephemeral(ctx, tr!("unmute.error", loc)).await;
    }
    let config = m::load_config(ctx.data(), guild).await;
    m::post_modlog(
        ctx.http(),
        config.as_ref(),
        m::modlog_embed(
            "unmute",
            user.user.id,
            Some(ctx.author().id),
            &[("reason", reason)],
            loc,
        ),
    )
    .await;
    reply_ephemeral(
        ctx,
        tr!("unmute.success", loc, user = format!("<@{}>", user.user.id)),
    )
    .await
}

/// Voir le journal de modération (serveur ou utilisateur).
#[poise::command(
    slash_command,
    guild_only,
    ephemeral,
    default_member_permissions = "MODERATE_MEMBERS",
    required_permissions = "MODERATE_MEMBERS"
)]
pub async fn modlog(ctx: Context<'_>, user: Option<serenity::Member>) -> Result<(), Error> {
    let loc = ctx.locale();
    ctx.defer_ephemeral().await?;
    let g = ctx.guild_id().unwrap().to_string();
    let uid = user.as_ref().map(|u| u.user.id.to_string());
    let per_user = uid.is_some();
    let hist = ctx
        .data()
        .db
        .run(move |c| {
            Ok(m::history(
                c,
                &g,
                uid.as_deref(),
                if uid.is_some() { 15 } else { 20 },
            )?)
        })
        .await?;

    let title = match &user {
        Some(u) => tr!("modlog.title_user", loc, user = u.display_name()),
        None => tr!("modlog.title_server", loc),
    };
    let description = if hist.is_empty() {
        tr!("modlog.no_entries", loc)
    } else {
        let text = hist
            .iter()
            .map(|h| {
                let who = if per_user {
                    String::new()
                } else {
                    format!("<@{}> - ", h.user_id)
                };
                format!(
                    "{who}**{}** <t:{}:R>\n└─ {}",
                    m::title_case(&h.action),
                    m::ts(&h.created_at),
                    reason_or_default(&h.reason, loc)
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        crate::bot::truncate(&text, 4096)
    };
    let e = serenity::CreateEmbed::new()
        .title(title)
        .description(description)
        .colour(colors::BLUE)
        .timestamp(serenity::Timestamp::now());
    ctx.send(CreateReply::default().embed(e).ephemeral(true))
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// /modconfig
// ---------------------------------------------------------------------------

/// Configuration du système de modération.
#[poise::command(
    slash_command,
    guild_only,
    subcommands("modconfig_view", "modconfig_set"),
    subcommand_required,
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn modconfig(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

fn channel_or_unset(id: &Option<String>, loc: Option<&str>) -> String {
    id.as_ref()
        .map(|c| format!("<#{c}>"))
        .unwrap_or_else(|| tr!("modconfig.not_set", loc))
}

/// Voir la configuration actuelle.
#[poise::command(
    slash_command,
    rename = "view",
    ephemeral,
    required_permissions = "ADMINISTRATOR"
)]
pub async fn modconfig_view(ctx: Context<'_>) -> Result<(), Error> {
    let loc = ctx.locale();
    ctx.defer_ephemeral().await?;
    let config = m::load_config(ctx.data(), ctx.guild_id().unwrap()).await;
    let mut e = serenity::CreateEmbed::new()
        .title(tr!("modconfig.title", loc))
        .colour(colors::BLUE)
        .timestamp(serenity::Timestamp::now());
    match config {
        None => e = e.description(tr!("modconfig.empty", loc)),
        Some(c) => {
            let status = tr!(
                if c.ai_enabled {
                    "modconfig.ai_on"
                } else {
                    "modconfig.ai_off"
                },
                loc
            );
            e = e
                .field(
                    tr!("modconfig.field_channels", loc),
                    tr!(
                        "modconfig.channels_value",
                        loc,
                        log = channel_or_unset(&c.log_channel_id, loc),
                        appeal = channel_or_unset(&c.appeal_channel_id, loc),
                        flag = channel_or_unset(&c.ai_flag_channel_id, loc)
                    ),
                    false,
                )
                .field(
                    tr!("modconfig.field_ai", loc),
                    tr!(
                        "modconfig.ai_value",
                        loc,
                        status = status,
                        threshold = c.ai_confidence_threshold,
                        model = c.ai_model,
                        host = c.ollama_host
                    ),
                    false,
                )
                .field(
                    tr!("modconfig.field_decay", loc),
                    tr!(
                        "modconfig.decay_value",
                        loc,
                        d1 = c.warn_1_decay_days,
                        d2 = c.warn_2_decay_days,
                        d3 = c.warn_3_decay_days
                    ),
                    false,
                )
                .field(
                    tr!("modconfig.field_mute", loc),
                    tr!(
                        "modconfig.mute_value",
                        loc,
                        m2 = m::format_duration(c.mute_duration_warn_2, loc),
                        m3 = m::format_duration(c.mute_duration_warn_3, loc)
                    ),
                    false,
                )
                .field(
                    tr!("modconfig.field_rules", loc),
                    tr!(
                        "modconfig.rules_value",
                        loc,
                        id = c
                            .rules_message_id
                            .unwrap_or_else(|| tr!("modconfig.not_set", loc))
                    ),
                    false,
                );
        }
    }
    ctx.send(CreateReply::default().embed(e).ephemeral(true))
        .await?;
    Ok(())
}

#[derive(Debug, poise::ChoiceParameter)]
pub enum ConfigParam {
    #[name = "log_channel"]
    LogChannel,
    #[name = "appeal_channel"]
    AppealChannel,
    #[name = "ai_flag_channel"]
    AiFlagChannel,
    #[name = "ai_enabled"]
    AiEnabled,
    #[name = "ai_confidence_threshold"]
    AiConfidenceThreshold,
    #[name = "ai_model"]
    AiModel,
    #[name = "ollama_host"]
    OllamaHost,
    #[name = "rules_message_id"]
    RulesMessageId,
    #[name = "warn_1_decay_days"]
    Warn1DecayDays,
    #[name = "warn_2_decay_days"]
    Warn2DecayDays,
    #[name = "warn_3_decay_days"]
    Warn3DecayDays,
    #[name = "mute_duration_warn_2"]
    MuteDurationWarn2,
    #[name = "mute_duration_warn_3"]
    MuteDurationWarn3,
}

impl ConfigParam {
    fn column(&self) -> &'static str {
        match self {
            Self::LogChannel => "log_channel_id",
            Self::AppealChannel => "appeal_channel_id",
            Self::AiFlagChannel => "ai_flag_channel_id",
            Self::AiEnabled => "ai_enabled",
            Self::AiConfidenceThreshold => "ai_confidence_threshold",
            Self::AiModel => "ai_model",
            Self::OllamaHost => "ollama_host",
            Self::RulesMessageId => "rules_message_id",
            Self::Warn1DecayDays => "warn_1_decay_days",
            Self::Warn2DecayDays => "warn_2_decay_days",
            Self::Warn3DecayDays => "warn_3_decay_days",
            Self::MuteDurationWarn2 => "mute_duration_warn_2",
            Self::MuteDurationWarn3 => "mute_duration_warn_3",
        }
    }

    /// Valide et normalise la valeur saisie.
    fn validate(&self, value: &str, text_channels: &[serenity::ChannelId]) -> Option<String> {
        let v = value.trim();
        match self {
            Self::LogChannel | Self::AppealChannel | Self::AiFlagChannel => {
                let id = v.trim_start_matches("<#").trim_end_matches('>');
                let id: u64 = id.parse().ok()?;
                text_channels
                    .contains(&serenity::ChannelId::new(id.max(1)))
                    .then(|| id.to_string())
            }
            Self::AiEnabled => match v.to_lowercase().as_str() {
                "true" | "1" | "yes" | "on" | "activé" => Some("1".into()),
                "false" | "0" | "no" | "off" | "désactivé" => Some("0".into()),
                _ => None,
            },
            Self::AiConfidenceThreshold => v
                .parse::<i64>()
                .ok()
                .filter(|n| (0..=100).contains(n))
                .map(|n| n.to_string()),
            Self::Warn1DecayDays
            | Self::Warn2DecayDays
            | Self::Warn3DecayDays
            | Self::MuteDurationWarn2
            | Self::MuteDurationWarn3 => v
                .parse::<i64>()
                .ok()
                .filter(|n| *n > 0)
                .map(|n| n.to_string()),
            Self::AiModel | Self::RulesMessageId => (!v.is_empty()).then(|| v.to_string()),
            Self::OllamaHost => {
                (v.starts_with("http://") || v.starts_with("https://")).then(|| v.to_string())
            }
        }
    }
}

/// Modifier un paramètre de configuration.
#[poise::command(
    slash_command,
    rename = "set",
    ephemeral,
    required_permissions = "ADMINISTRATOR"
)]
pub async fn modconfig_set(
    ctx: Context<'_>,
    #[rename = "parameter"] param: ConfigParam,
    value: String,
) -> Result<(), Error> {
    let loc = ctx.locale();
    ctx.defer_ephemeral().await?;
    let name = poise::ChoiceParameter::name(&param);
    let text_channels: Vec<serenity::ChannelId> = ctx
        .guild()
        .map(|g| {
            g.channels
                .values()
                .filter(|c| c.kind == serenity::ChannelType::Text)
                .map(|c| c.id)
                .collect()
        })
        .unwrap_or_default();
    let Some(validated) = param.validate(&value, &text_channels) else {
        return reply_ephemeral(
            ctx,
            tr!("modconfig.invalid_value", loc, param = name, value = value),
        )
        .await;
    };
    let (g, col, v) = (
        ctx.guild_id().unwrap().to_string(),
        param.column(),
        validated.clone(),
    );
    match ctx
        .data()
        .db
        .run(move |c| m::set_config(c, &g, col, &v))
        .await
    {
        Ok(()) => {
            reply_ephemeral(
                ctx,
                tr!("modconfig.updated", loc, param = name, value = validated),
            )
            .await
        }
        Err(e) => {
            tracing::error!("Error in set_config command: {e:#}");
            reply_ephemeral(ctx, tr!("modconfig.set_error", loc, error = e.to_string())).await
        }
    }
}

// ---------------------------------------------------------------------------
// Menu contextuel « Warn User »
// ---------------------------------------------------------------------------

/// Avertir l'auteur d'un message (clic droit → Applications).
#[poise::command(
    context_menu_command = "Warn User",
    guild_only,
    default_member_permissions = "MODERATE_MEMBERS"
)]
pub async fn warn_user_context(ctx: Context<'_>, message: serenity::Message) -> Result<(), Error> {
    let poise::Context::Application(actx) = ctx else {
        return Ok(());
    };
    let loc = ctx.locale();
    let interaction = actx.interaction;
    let sctx = ctx.serenity_context();

    let can_moderate = interaction
        .member
        .as_ref()
        .and_then(|mb| mb.permissions)
        .is_some_and(|p| p.moderate_members());
    let early = if !can_moderate {
        Some("mod.ctx.no_perm")
    } else if message.author.bot {
        Some("mod.ctx.bot")
    } else if message.author.id == ctx.author().id {
        Some("mod.ctx.self")
    } else {
        None
    };
    if let Some(key) = early {
        return reply_ephemeral(ctx, tr!(key, loc)).await;
    }

    let guild = ctx.guild_id().unwrap();
    let (g, u) = (guild.to_string(), message.author.id.to_string());
    let current = ctx
        .data()
        .db
        .run(move |c| Ok(m::warning_count(c, &g, &u)?))
        .await?;

    let author_name = message
        .author
        .global_name
        .clone()
        .unwrap_or_else(|| message.author.name.clone());
    let title = tr!(
        "mod.modal.heading",
        loc,
        user = author_name,
        count = current,
        s = if current != 1 { "s" } else { "" }
    );
    let modal_id = ui::unique_id("warn_modal");
    let inputs = [
        ui::Input::short("reason", tr!("mod.modal.reason_label", loc))
            .placeholder(tr!("mod.modal.reason_placeholder", loc))
            .max(100),
        ui::Input::short("notes", tr!("mod.modal.notes_label", loc))
            .placeholder(tr!("mod.modal.notes_placeholder", loc))
            .paragraph()
            .optional()
            .max(500),
    ];
    interaction
        .create_response(sctx, ui::modal(&modal_id, &title, &inputs))
        .await?;
    actx.has_sent_initial_response
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let Some(mi) = ui::wait_modal(sctx, modal_id).await else {
        return Ok(());
    };
    mi.defer_ephemeral(sctx).await?;

    let mut reason = ui::modal_value(&mi, "reason").trim().to_string();
    let notes = ui::modal_value(&mi, "notes").trim().to_string();
    if !notes.is_empty() {
        reason = if reason.is_empty() {
            notes
        } else {
            format!("{reason} — {notes}")
        };
    }
    if reason.is_empty() {
        reason = tr!("mod.ctx.default_reason", loc);
    }

    let name = guild_name(ctx);
    let result = issue_warning(
        sctx,
        ctx.data(),
        guild,
        &name,
        message.author.id,
        ctx.author().id,
        &reason,
        loc,
        true,
    )
    .await;
    let (mut out, config) = match result {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Erreur lors de l'avertissement via menu contextuel: {e:#}");
            mi.create_followup(sctx, ui::followup(tr!("mod.ctx.error", loc)))
                .await?;
            return Ok(());
        }
    };

    match message.delete(sctx).await {
        Ok(()) => out.action += &tr!("mod.ctx.action_msg_deleted", loc),
        Err(e) => {
            tracing::error!("Erreur lors de la suppression du message: {e}");
            out.action += &tr!("mod.ctx.action_msg_delete_failed", loc);
        }
    }

    m::post_modlog(
        &sctx.http,
        config.as_ref(),
        m::modlog_embed(
            "warn",
            message.author.id,
            Some(ctx.author().id),
            &[
                ("reason", reason.clone()),
                ("warn_count", out.count.to_string()),
                ("message_link", message.link()),
            ],
            loc,
        ),
    )
    .await;

    let dm = tr!(
        if out.dm_sent {
            "mod.ctx.dm_sent"
        } else {
            "mod.ctx.dm_not_sent"
        },
        loc
    );
    mi.create_followup(
        sctx,
        ui::followup(tr!(
            "mod.ctx.warn_issued",
            loc,
            user = format!("<@{}>", message.author.id),
            reason = reason,
            count = out.count,
            dm = dm,
            action = out.action
        )),
    )
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// /appeal et revue
// ---------------------------------------------------------------------------

/// Faire appel de vos avertissements.
#[poise::command(slash_command, guild_only, ephemeral)]
pub async fn appeal(ctx: Context<'_>, reason: String) -> Result<(), Error> {
    let loc = ctx.locale();
    ctx.defer_ephemeral().await?;
    let guild = ctx.guild_id().unwrap();
    let user = ctx.author().id;

    if reason.chars().count() > 1000 {
        return reply_ephemeral(ctx, tr!("appeal.too_long", loc)).await;
    }

    enum Step {
        NoWarns,
        Cooldown(chrono::Duration),
        Pending,
        Created(i64, i64, Vec<m::HistoryEntry>, Option<ModConfig>),
    }
    let (g, u, r) = (guild.to_string(), user.to_string(), reason.clone());
    let step = ctx
        .data()
        .db
        .run(move |c| {
            let count = m::warning_count(c, &g, &u)?;
            if count <= 0 {
                return Ok(Step::NoWarns);
            }
            if let Some(rem) = m::appeal_cooldown(c, &g, &u)? {
                return Ok(Step::Cooldown(rem));
            }
            match m::create_appeal(c, &g, &u, &r)? {
                None => Ok(Step::Pending),
                Some(id) => Ok(Step::Created(
                    id,
                    count,
                    m::history(c, &g, Some(&u), 5)?,
                    m::get_config(c, &g)?,
                )),
            }
        })
        .await;

    let (appeal_id, count, hist, config) = match step {
        Ok(Step::NoWarns) => return reply_ephemeral(ctx, tr!("appeal.no_warns", loc)).await,
        Ok(Step::Cooldown(rem)) => {
            let secs = rem.num_seconds();
            return reply_ephemeral(
                ctx,
                tr!(
                    "appeal.cooldown",
                    loc,
                    hours = secs / 3600,
                    minutes = (secs % 3600) / 60
                ),
            )
            .await;
        }
        Ok(Step::Pending) => return reply_ephemeral(ctx, tr!("appeal.pending", loc)).await,
        Ok(Step::Created(id, n, h, c)) => (id, n, h, c),
        Err(e) => {
            tracing::error!("Error in appeal command: {e:#}");
            return reply_ephemeral(ctx, tr!("appeal.error", loc)).await;
        }
    };

    let e = serenity::CreateEmbed::new()
        .title(tr!("appeal.submitted_title", loc))
        .description(tr!("appeal.submitted_desc", loc))
        .colour(colors::GREEN)
        .timestamp(serenity::Timestamp::now())
        .field(tr!("mod.field.reason", loc), reason.clone(), false)
        .field(
            tr!("appeal.next_steps", loc),
            tr!("appeal.next_steps_value", loc),
            false,
        )
        .footer(serenity::CreateEmbedFooter::new(tr!("mod.footer", loc)));
    ctx.send(CreateReply::default().embed(e).ephemeral(true))
        .await?;

    let Some(channel) = config
        .as_ref()
        .and_then(|c| c.appeal_channel_id.as_deref())
        .and_then(|s| s.parse::<u64>().ok())
    else {
        return Ok(());
    };

    let mut e = serenity::CreateEmbed::new()
        .title(tr!("appeal.new_title", loc, id = appeal_id))
        .description(tr!(
            "appeal.new_desc",
            loc,
            user = format!("<@{user}> ({user})")
        ))
        .colour(colors::PURPLE)
        .timestamp(serenity::Timestamp::now())
        .field(tr!("appeal.current_warns", loc), count.to_string(), true)
        .field(tr!("appeal.reason_of_appeal", loc), reason, false);
    if !hist.is_empty() {
        let text = hist
            .iter()
            .map(|h| {
                format!(
                    "• {} <t:{}:R>",
                    m::title_case(&h.action),
                    m::ts(&h.created_at)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        e = e.field(tr!("appeal.recent_history", loc), text, false);
    }
    e = e.footer(serenity::CreateEmbedFooter::new(tr!(
        "appeal.review_footer",
        loc
    )));

    let loc_tag = crate::i18n::base_locale(loc);
    let buttons = serenity::CreateActionRow::Buttons(vec![
        serenity::CreateButton::new(format!("appeal_approve:{appeal_id}:{user}:{loc_tag}"))
            .label(tr!("appeal.btn_approve", loc))
            .style(serenity::ButtonStyle::Success),
        serenity::CreateButton::new(format!("appeal_deny:{appeal_id}:{user}:{loc_tag}"))
            .label(tr!("appeal.btn_deny", loc))
            .style(serenity::ButtonStyle::Danger),
    ]);
    if let Err(e) = serenity::ChannelId::new(channel)
        .send_message(
            ctx,
            serenity::CreateMessage::new()
                .embed(e)
                .components(vec![buttons]),
        )
        .await
    {
        tracing::error!("Error posting appeal to channel: {e}");
    }
    Ok(())
}

/// Gestion des boutons de revue d'appel (fonctionne aussi après un redémarrage).
pub async fn handle_component(
    ctx: &serenity::Context,
    data: &Data,
    ci: &serenity::ComponentInteraction,
) -> Result<bool, Error> {
    let mut parts = ci.data.custom_id.split(':');
    let kind = parts.next().unwrap_or_default();
    if kind != "appeal_approve" && kind != "appeal_deny" {
        return Ok(false);
    }
    let (Some(Ok(appeal_id)), Some(Ok(user_id))) = (
        parts.next().map(str::parse::<i64>),
        parts.next().map(str::parse::<u64>),
    ) else {
        return Ok(true);
    };
    let loc_owned = parts.next().map(str::to_string);
    let loc = loc_owned.as_deref();

    let can_moderate = ci
        .member
        .as_ref()
        .and_then(|mb| mb.permissions)
        .is_some_and(|p| p.moderate_members());
    if !can_moderate {
        ci.create_response(ctx, ui::ephemeral(tr!("appeal.no_perm", loc)))
            .await?;
        return Ok(true);
    }

    let (decision, moderator_decision, responder): (
        &str,
        String,
        Option<serenity::ModalInteraction>,
    ) = if kind == "appeal_approve" {
        ci.defer_ephemeral(ctx).await?;
        (
            "approved",
            tr!("appeal.decision_approved_reason", loc),
            None,
        )
    } else {
        let modal_id = ui::unique_id("appeal_deny_modal");
        let input = ui::Input::short("decision", tr!("appeal.deny_modal.label", loc))
            .placeholder(tr!("appeal.deny_modal.placeholder", loc))
            .paragraph()
            .max(500);
        ci.create_response(
            ctx,
            ui::modal(&modal_id, &tr!("appeal.deny_modal.title", loc), &[input]),
        )
        .await?;
        let Some(mi) = ui::wait_modal(ctx, modal_id).await else {
            return Ok(true);
        };
        mi.defer_ephemeral(ctx).await?;
        ("denied", ui::modal_value(&mi, "decision"), Some(mi))
    };

    let followup = |content: String| {
        let responder = responder.clone();
        async move {
            let r = match &responder {
                Some(mi) => mi.create_followup(ctx, ui::followup(content)).await,
                None => ci.create_followup(ctx, ui::followup(content)).await,
            };
            if let Err(e) = r {
                tracing::error!("Impossible de répondre à la revue d'appel: {e}");
            }
        }
    };

    let moderator = ci.user.id.to_string();
    let (mo, dec, md) = (
        moderator.clone(),
        decision.to_string(),
        moderator_decision.clone(),
    );
    let approved_db = tr!("appeal.decision_approved_db", loc);
    let result = data
        .db
        .run(move |c| {
            let Some((g, u)) = m::review_appeal(c, appeal_id, &mo, &dec, &md)? else {
                return Ok(None);
            };
            let new_count = if dec == "approved" {
                Some(m::decrement_warning(
                    c,
                    &g,
                    &u,
                    Some(&mo),
                    Some(&approved_db),
                )?)
            } else {
                None
            };
            Ok(Some((g, new_count)))
        })
        .await;
    let (guild_id, new_count) = match result {
        Ok(Some(v)) => v,
        Ok(None) => {
            followup(tr!("appeal.not_found", loc)).await;
            return Ok(true);
        }
        Err(e) => {
            tracing::error!("Error handling appeal decision: {e:#}");
            followup(tr!("appeal.error_generic", loc)).await;
            return Ok(true);
        }
    };
    let guild = serenity::GuildId::new(guild_id.parse()?);
    let user = serenity::UserId::new(user_id);
    let approved = decision == "approved";

    if new_count == Some(0) {
        lift_auto_mute_if_clean(
            ctx,
            data,
            guild,
            user,
            Some(moderator.clone()),
            tr!("appeal.timeout_reason", loc),
        )
        .await;
    }

    // MP à l'utilisateur.
    let guild_name = ctx
        .cache
        .guild(guild)
        .map(|g| g.name.clone())
        .unwrap_or_default();
    let mut e = serenity::CreateEmbed::new()
        .title(tr!(
            if approved {
                "appeal.approved_title"
            } else {
                "appeal.denied_title"
            },
            loc
        ))
        .description(tr!("appeal.reviewed_desc", loc, guild = guild_name))
        .colour(if approved { colors::GREEN } else { colors::RED })
        .timestamp(serenity::Timestamp::now())
        .field(tr!("appeal.mod_decision", loc), moderator_decision, false);
    e = if approved {
        e.field(
            tr!("appeal.new_warns", loc),
            new_count.unwrap_or(0).to_string(),
            true,
        )
        .field(
            tr!("mod.field.reason", loc),
            tr!("appeal.removed_msg", loc),
            false,
        )
    } else {
        e.field(
            tr!("appeal.next_steps", loc),
            tr!("appeal.deny_next_steps", loc),
            false,
        )
    };
    send_dm(
        ctx,
        user,
        e.footer(serenity::CreateEmbedFooter::new(tr!("mod.footer", loc))),
    )
    .await;

    // Mise à jour du message de revue : boutons désactivés + décision.
    let mut msg = ci.message.clone();
    if let Some(original) = msg.embeds.first().cloned() {
        let updated = serenity::CreateEmbed::from(original)
            .colour(if approved { colors::GREEN } else { colors::RED })
            .field(
                tr!("appeal.decision_field", loc),
                tr!(
                    if approved {
                        "appeal.approved_by"
                    } else {
                        "appeal.denied_by"
                    },
                    loc,
                    user = format!("<@{}>", ci.user.id)
                ),
                false,
            );
        let disabled = serenity::CreateActionRow::Buttons(vec![
            serenity::CreateButton::new("appeal_done_approve")
                .label(tr!("appeal.btn_approve", loc))
                .style(serenity::ButtonStyle::Success)
                .disabled(true),
            serenity::CreateButton::new("appeal_done_deny")
                .label(tr!("appeal.btn_deny", loc))
                .style(serenity::ButtonStyle::Danger)
                .disabled(true),
        ]);
        if let Err(e) = msg
            .edit(
                ctx,
                serenity::EditMessage::new()
                    .embed(updated)
                    .components(vec![disabled]),
            )
            .await
        {
            tracing::error!("Impossible de mettre à jour le message d'appel: {e}");
        }
    }

    let status = tr!(
        if approved {
            "appeal.result_approved"
        } else {
            "appeal.result_denied"
        },
        loc
    );
    followup(tr!("appeal.result", loc, status = status)).await;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Tâches de fond
// ---------------------------------------------------------------------------

/// Toutes les 6 h : expiration des avertissements.
pub async fn warning_decay_loop(ctx: serenity::Context, data: Data) {
    tracing::info!("Démarrage de la boucle d'expiration des avertissements");
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(6 * 3600));
    loop {
        interval.tick().await;
        if let Err(e) = decay_once(&ctx, &data).await {
            tracing::error!(
                "Erreur lors de la vérification d'expiration des avertissements: {e:#}"
            );
        }
    }
}

async fn decay_once(ctx: &serenity::Context, data: &Data) -> Result<(), Error> {
    let users = data.db.run(|c| Ok(m::users_for_decay(c)?)).await?;
    tracing::debug!(
        "[Modération] {} utilisateur(s) à faire expirer",
        users.len()
    );
    for (g, u, before) in users {
        let (g2, u2) = (g.clone(), u.clone());
        let new = data
            .db
            .run(move |c| {
                Ok(m::decrement_warning(
                    c,
                    &g2,
                    &u2,
                    None,
                    Some("Expiration automatique"),
                )?)
            })
            .await?;
        tracing::info!("[Modération] Avertissement expiré: {u} @ {g} ({before} -> {new})");
        let (Ok(gid), Ok(uid)) = (g.parse::<u64>(), u.parse::<u64>()) else {
            continue;
        };
        let (guild, user) = (serenity::GuildId::new(gid), serenity::UserId::new(uid));
        if new == 0 {
            lift_auto_mute_if_clean(
                ctx,
                data,
                guild,
                user,
                None,
                "Avertissements expirés".into(),
            )
            .await;
        }
        let name = ctx
            .cache
            .guild(guild)
            .map(|g| g.name.clone())
            .unwrap_or_default();
        send_dm(ctx, user, m::decay_embed(new, &name, None)).await;
        let config = m::load_config(data, guild).await;
        m::post_modlog(
            &ctx.http,
            config.as_ref(),
            m::modlog_embed(
                "decay",
                user,
                None,
                &[
                    ("warn_count_before", before.to_string()),
                    ("warn_count_after", new.to_string()),
                ],
                None,
            ),
        )
        .await;
    }
    Ok(())
}

/// Toutes les minutes : levée des mutes expirés.
pub async fn mute_expiration_loop(ctx: serenity::Context, data: Data) {
    tracing::info!("Démarrage de la boucle d'expiration des mutes");
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
    loop {
        interval.tick().await;
        if let Err(e) = expire_mutes_once(&ctx, &data).await {
            tracing::error!("Erreur lors de la vérification d'expiration des mutes: {e:#}");
        }
    }
}

async fn expire_mutes_once(ctx: &serenity::Context, data: &Data) -> Result<(), Error> {
    let mutes = data.db.run(|c| Ok(m::expired_mutes(c)?)).await?;
    for (g, u) in mutes {
        let (Ok(gid), Ok(uid)) = (g.parse::<u64>(), u.parse::<u64>()) else {
            continue;
        };
        let (guild, user) = (serenity::GuildId::new(gid), serenity::UserId::new(uid));
        if guild.member(ctx, user).await.is_err() {
            // Utilisateur parti : on nettoie simplement la base.
            data.db
                .run(move |c| Ok(m::remove_mute(c, &g, &u, None, "Utilisateur absent")?))
                .await?;
            continue;
        }
        if let Err(e) = m::lift_mute(&ctx.http, data, guild, user, None, "Mute expiré".into()).await
        {
            tracing::error!("[Modération] Erreur lors du retrait du timeout: {e:#}");
            // On retire quand même l'entrée pour ne pas boucler indéfiniment.
            let (g, u) = (g.clone(), u.clone());
            data.db
                .run(move |c| Ok(m::remove_mute(c, &g, &u, None, "Expiré")?))
                .await?;
        } else {
            tracing::info!("[Modération] Mute expiré: {u} @ {g}");
        }
        let name = ctx
            .cache
            .guild(guild)
            .map(|g| g.name.clone())
            .unwrap_or_default();
        send_dm(ctx, user, m::mute_expired_embed(&name)).await;
        let config = m::load_config(data, guild).await;
        m::post_modlog(
            &ctx.http,
            config.as_ref(),
            m::modlog_embed(
                "unmute",
                user,
                None,
                &[("reason", "Mute expiré automatiquement".into())],
                None,
            ),
        )
        .await;
    }
    Ok(())
}

//! Commandes du minijeu : configuration (/minigame), quêtes (/daily, /quest),
//! boutique (/shop, /inventory, /use), échanges (/trade), portefeuille,
//! historique, jeux (/capture, /duel) et statistiques (/stats).

use std::time::{Duration, Instant};

use poise::CreateReply;
use poise::serenity_prelude as serenity;
use rusqlite::Connection;

use crate::bot::{Context, Data, Error, colors};
use crate::economy::{self as eco, UserError, fmt_num};
use crate::features::is_enabled;
use crate::moderation::title_case;
use crate::quests;
use crate::tr;

type Loc<'a> = Option<&'a str>;

fn embed(title: String, color: u32) -> serenity::CreateEmbed {
    serenity::CreateEmbed::new().title(title).colour(color)
}

async fn send(ctx: Context<'_>, e: serenity::CreateEmbed, ephemeral: bool) -> Result<(), Error> {
    ctx.send(CreateReply::default().embed(e).ephemeral(ephemeral))
        .await?;
    Ok(())
}

/// Affiche une erreur métier (`UserError`) dans un embed rouge ; propage les autres.
async fn user_error(ctx: Context<'_>, title_key: &str, err: Error) -> Result<(), Error> {
    match err.downcast::<UserError>() {
        Ok(UserError(msg)) => {
            send(
                ctx,
                embed(tr!(title_key, ctx.locale()), colors::RED).description(msg),
                true,
            )
            .await
        }
        Err(other) => Err(other),
    }
}

async fn warn(
    ctx: Context<'_>,
    key: &str,
    args: Vec<(&str, crate::i18n::Arg)>,
) -> Result<(), Error> {
    ctx.send(
        CreateReply::default()
            .content(crate::i18n::t(key, ctx.locale(), args))
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Vérifie que le minijeu est actif et que la commande est lancée dans le bon salon.
/// Renvoie `(serveur, utilisateur)` si la commande peut continuer.
async fn gate(ctx: Context<'_>, allow_exceptions: bool) -> Result<Option<(String, String)>, Error> {
    if !is_enabled("minigame") {
        warn(ctx, "minigame.disabled_global", vec![]).await?;
        return Ok(None);
    }
    let Some(guild) = ctx.guild_id() else {
        warn(ctx, "minigame.common.guild_only", vec![]).await?;
        return Ok(None);
    };
    let (g, ch) = (guild.to_string(), ctx.channel_id().to_string());
    let (g2, ch2) = (g.clone(), ch.clone());
    let (settings, is_exception) = ctx
        .data()
        .db
        .tx(move |c| {
            let s = eco::guild_settings(c, &g2)?;
            let ex = quests::is_exception_channel(c, &g2, &ch2)?;
            Ok((s, ex))
        })
        .await?;
    if !settings.minigame_enabled {
        warn(ctx, "minigame.disabled_guild", vec![]).await?;
        return Ok(None);
    }
    let Some(mg_channel) = settings.minigame_channel_id else {
        warn(ctx, "minigame.no_channel_set", vec![]).await?;
        return Ok(None);
    };
    if ch == mg_channel || (allow_exceptions && is_exception) {
        return Ok(Some((g, ctx.author().id.to_string())));
    }
    warn(
        ctx,
        "minigame.channel_restriction",
        vec![("channel_mention", format!("<#{mg_channel}>").into())],
    )
    .await?;
    Ok(None)
}

/// Exécute une opération en base dans une transaction.
async fn db<T, F>(ctx: Context<'_>, f: F) -> Result<T, Error>
where
    T: Send + 'static,
    F: FnOnce(&Connection) -> Result<T, Error> + Send + 'static,
{
    ctx.data().db.tx(f).await
}

// ---------------------------------------------------------------------------
// /minigame (administration)
// ---------------------------------------------------------------------------

/// Gestion du système de mini-jeux.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "ADMINISTRATOR",
    subcommand_required,
    subcommands(
        "mg_enable",
        "mg_disable",
        "mg_set_channel",
        "mg_clear_channel",
        "mg_allow_channel",
        "mg_remove_channel",
        "mg_stats"
    )
)]
pub async fn minigame(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Activer les mini-jeux sur ce serveur.
#[poise::command(
    slash_command,
    rename = "enable",
    required_permissions = "ADMINISTRATOR"
)]
pub async fn mg_enable(ctx: Context<'_>) -> Result<(), Error> {
    if !is_enabled("minigame") {
        return warn(ctx, "minigame.disabled_global", vec![]).await;
    }
    let g = ctx.guild_id().unwrap().to_string();
    db(ctx, move |c| eco::set_minigame_enabled(c, &g, true)).await?;
    let loc = ctx.locale();
    send(
        ctx,
        embed(tr!("minigame.enable.title", loc), colors::GREEN)
            .description(tr!("minigame.enable.description", loc)),
        false,
    )
    .await
}

/// Désactiver les mini-jeux sur ce serveur.
#[poise::command(
    slash_command,
    rename = "disable",
    required_permissions = "ADMINISTRATOR"
)]
pub async fn mg_disable(ctx: Context<'_>) -> Result<(), Error> {
    let g = ctx.guild_id().unwrap().to_string();
    db(ctx, move |c| eco::set_minigame_enabled(c, &g, false)).await?;
    let loc = ctx.locale();
    send(
        ctx,
        embed(tr!("minigame.disable.title", loc), colors::ORANGE)
            .description(tr!("minigame.disable.description", loc)),
        false,
    )
    .await
}

/// Définir le salon des mini-jeux.
#[poise::command(
    slash_command,
    rename = "set-channel",
    required_permissions = "ADMINISTRATOR"
)]
pub async fn mg_set_channel(
    ctx: Context<'_>,
    #[channel_types("Text")] channel: serenity::GuildChannel,
) -> Result<(), Error> {
    let (g, ch) = (ctx.guild_id().unwrap().to_string(), channel.id.to_string());
    db(ctx, move |c| eco::set_minigame_channel(c, &g, Some(&ch))).await?;
    let loc = ctx.locale();
    send(
        ctx,
        embed(tr!("minigame.set_channel.title", loc), colors::GREEN).description(tr!(
            "minigame.set_channel.description",
            loc,
            channel_mention = format!("<#{}>", channel.id)
        )),
        false,
    )
    .await
}

/// Supprimer la restriction de salon.
#[poise::command(
    slash_command,
    rename = "clear-channel",
    required_permissions = "ADMINISTRATOR"
)]
pub async fn mg_clear_channel(ctx: Context<'_>) -> Result<(), Error> {
    let g = ctx.guild_id().unwrap().to_string();
    db(ctx, move |c| eco::set_minigame_channel(c, &g, None)).await?;
    let loc = ctx.locale();
    send(
        ctx,
        embed(tr!("minigame.clear_channel.title", loc), colors::GREEN)
            .description(tr!("minigame.clear_channel.description", loc)),
        true,
    )
    .await
}

/// Ajouter un salon d'exception pour les quêtes.
#[poise::command(
    slash_command,
    rename = "allow-channel",
    required_permissions = "ADMINISTRATOR"
)]
pub async fn mg_allow_channel(
    ctx: Context<'_>,
    #[channel_types("Text")] channel: serenity::GuildChannel,
) -> Result<(), Error> {
    let (g, ch) = (ctx.guild_id().unwrap().to_string(), channel.id.to_string());
    let added = db(ctx, move |c| eco::add_exception_channel(c, &g, &ch)).await?;
    let loc = ctx.locale();
    let mention = format!("<#{}>", channel.id);
    let e = if added {
        embed(
            tr!("minigame.allow_channel.added_title", loc),
            colors::GREEN,
        )
        .description(tr!(
            "minigame.allow_channel.added_description",
            loc,
            channel_mention = mention
        ))
    } else {
        embed(
            tr!("minigame.allow_channel.exists_title", loc),
            colors::BLUE,
        )
        .description(tr!(
            "minigame.allow_channel.exists_description",
            loc,
            channel_mention = mention
        ))
    };
    send(ctx, e, true).await
}

/// Retirer un salon d'exception.
#[poise::command(
    slash_command,
    rename = "remove-channel",
    required_permissions = "ADMINISTRATOR"
)]
pub async fn mg_remove_channel(
    ctx: Context<'_>,
    #[channel_types("Text")] channel: serenity::GuildChannel,
) -> Result<(), Error> {
    let (g, ch) = (ctx.guild_id().unwrap().to_string(), channel.id.to_string());
    let removed = db(ctx, move |c| eco::remove_exception_channel(c, &g, &ch)).await?;
    let loc = ctx.locale();
    let mention = format!("<#{}>", channel.id);
    let e = if removed {
        embed(
            tr!("minigame.remove_channel.removed_title", loc),
            colors::GREEN,
        )
        .description(tr!(
            "minigame.remove_channel.removed_description",
            loc,
            channel_mention = mention
        ))
    } else {
        embed(
            tr!("minigame.remove_channel.not_found_title", loc),
            colors::BLUE,
        )
        .description(tr!(
            "minigame.remove_channel.not_found_description",
            loc,
            channel_mention = mention
        ))
    };
    send(ctx, e, true).await
}

/// Voir la configuration des mini-jeux.
#[poise::command(
    slash_command,
    rename = "stats",
    required_permissions = "ADMINISTRATOR"
)]
pub async fn mg_stats(ctx: Context<'_>) -> Result<(), Error> {
    let loc = ctx.locale();
    let g = ctx.guild_id().unwrap().to_string();
    let (s, exceptions) = db(ctx, move |c| {
        Ok((
            eco::guild_settings(c, &g)?,
            eco::exception_channel_count(c, &g)?,
        ))
    })
    .await?;
    let status = if !is_enabled("minigame") {
        tr!("minigame.config.disabled_global", loc)
    } else if !s.minigame_enabled {
        tr!("minigame.config.disabled_guild", loc)
    } else {
        tr!("minigame.config.enabled", loc)
    };
    let channel = s
        .minigame_channel_id
        .map(|c| format!("<#{c}>"))
        .unwrap_or_else(|| tr!("minigame.config.channel_not_set", loc));
    let trading = tr!(
        if s.xp_trading_enabled {
            "minigame.config.enabled"
        } else {
            "minigame.config.disabled"
        },
        loc
    );
    let e = embed(tr!("minigame.config.title", loc), colors::BLUE)
        .field(tr!("minigame.config.system_status", loc), status, true)
        .field(tr!("minigame.config.channel", loc), channel, true)
        .field(
            tr!("minigame.config.exception_channels", loc),
            exceptions.to_string(),
            true,
        )
        .field(tr!("minigame.config.xp_trading", loc), trading, true)
        .field(
            tr!("minigame.config.trade_tax", loc),
            format!("{}%", fmt_num(s.trade_tax_percent)),
            true,
        )
        .field(
            tr!("minigame.config.duel_tax", loc),
            format!("{}%", fmt_num(s.duel_tax_percent)),
            true,
        )
        .field(
            tr!("minigame.config.daily_xp_cap", loc),
            tr!(
                "minigame.config.daily_xp_cap_value",
                loc,
                percent = fmt_num(s.daily_xp_transfer_cap_percent),
                limit = s.daily_xp_transfer_cap_max
            ),
            true,
        );
    send(ctx, e, true).await
}

// ---------------------------------------------------------------------------
// /daily et /quest
// ---------------------------------------------------------------------------

/// Quêtes quotidiennes.
#[poise::command(
    slash_command,
    guild_only,
    subcommand_required,
    subcommands("daily_claim", "daily_status")
)]
pub async fn daily(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

fn rewards_line(q: &quests::UserQuest, loc: Loc<'_>) -> String {
    tr!(
        "minigame.quest.rewards",
        loc,
        coins = q.reward_coins,
        xp = q.reward_xp
    )
}

fn percent(q: &quests::UserQuest) -> String {
    format!("{:.0}", q.progress as f64 / q.target.max(1) as f64 * 100.0)
}

/// Obtenir vos quêtes quotidiennes.
#[poise::command(slash_command, rename = "claim")]
pub async fn daily_claim(ctx: Context<'_>) -> Result<(), Error> {
    let Some((g, u)) = gate(ctx, false).await? else {
        return Ok(());
    };
    let loc = ctx.locale();
    enum Outcome {
        Existing(quests::DailyStatus),
        None,
        Assigned(Vec<quests::UserQuest>, i64),
    }
    let outcome = db(ctx, move |c| {
        let status = quests::daily_status(c, &g, &u)?;
        if !status.can_claim_new && !status.quests.is_empty() {
            return Ok(Outcome::Existing(status));
        }
        let assigned = quests::assign_daily_quests(c, &g, &u)?;
        if assigned.is_empty() {
            return Ok(Outcome::None);
        }
        let streak = quests::update_streak(c, &g, &u)?;
        Ok(Outcome::Assigned(assigned, streak))
    })
    .await?;

    let e = match outcome {
        Outcome::Existing(status) => {
            let mut e = embed(tr!("minigame.daily.quests_title", loc), colors::ORANGE)
                .description(tr!("minigame.daily.already", loc));
            for q in &status.quests {
                e = e.field(
                    format!("{} {}", if q.completed { "✅" } else { "🔄" }, q.name),
                    format!(
                        "{}\n{}\n{}",
                        q.description,
                        tr!(
                            "minigame.quest.progress",
                            loc,
                            progress = q.progress,
                            target = q.target
                        ),
                        rewards_line(q, loc)
                    ),
                    false,
                );
            }
            e.footer(serenity::CreateEmbedFooter::new(tr!(
                "minigame.common.streak",
                loc,
                streak = status.streak
            )))
        }
        Outcome::None => embed(tr!("minigame.daily.none_title", loc), colors::RED)
            .description(tr!("minigame.daily.none_description", loc)),
        Outcome::Assigned(list, streak) => {
            let mut e = embed(tr!("minigame.daily.assigned_title", loc), colors::GREEN)
                .description(tr!("minigame.daily.assigned_description", loc));
            for q in &list {
                e = e.field(
                    format!("🔄 {}", q.name),
                    format!("{}\n{}", q.description, rewards_line(q, loc)),
                    false,
                );
            }
            e.footer(serenity::CreateEmbedFooter::new(tr!(
                "minigame.common.streak_multiplier",
                loc,
                streak = streak,
                multiplier = fmt_num(quests::streak_multiplier(streak))
            )))
        }
    };
    send(ctx, e, false).await
}

/// Voir l'état de vos quêtes quotidiennes.
#[poise::command(slash_command, rename = "status")]
pub async fn daily_status(ctx: Context<'_>) -> Result<(), Error> {
    let Some((g, u)) = gate(ctx, false).await? else {
        return Ok(());
    };
    let loc = ctx.locale();
    let status = db(ctx, move |c| quests::daily_status(c, &g, &u)).await?;
    let footer = serenity::CreateEmbedFooter::new(tr!(
        "minigame.common.streak",
        loc,
        streak = status.streak
    ));
    if status.quests.is_empty() {
        let e = embed(tr!("minigame.daily.status_empty_title", loc), colors::BLUE)
            .description(tr!("minigame.daily.status_empty_description", loc))
            .footer(footer);
        return send(ctx, e, false).await;
    }
    let color = if status.all_completed() {
        colors::GREEN
    } else {
        colors::BLUE
    };
    let mut e = embed(tr!("minigame.daily.status_title", loc), color).description(tr!(
        "minigame.daily.status_description",
        loc,
        completed = status.completed,
        total = status.quests.len()
    ));
    for q in &status.quests {
        let (icon, text) = if q.completed {
            ("✅", tr!("minigame.daily.completed", loc))
        } else {
            (
                "🔄",
                tr!(
                    "minigame.daily.progress_value",
                    loc,
                    progress = q.progress,
                    target = q.target,
                    percent = percent(q)
                ),
            )
        };
        e = e.field(
            format!("{icon} {}", q.name),
            format!(
                "{}\n{}\n{}",
                q.description,
                tr!("minigame.quest.status", loc, status = text),
                rewards_line(q, loc)
            ),
            false,
        );
    }
    send(ctx, e.footer(footer), false).await
}

/// Gestion des quêtes.
#[poise::command(
    slash_command,
    guild_only,
    subcommand_required,
    subcommands("quest_list", "quest_claim")
)]
pub async fn quest(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Lister vos quêtes actives.
#[poise::command(slash_command, rename = "list")]
pub async fn quest_list(ctx: Context<'_>) -> Result<(), Error> {
    let Some((g, u)) = gate(ctx, true).await? else {
        return Ok(());
    };
    let loc = ctx.locale();
    let list = db(ctx, move |c| quests::active_quests(c, &g, &u)).await?;
    if list.is_empty() {
        let e = embed(tr!("minigame.quest.list_title", loc), colors::BLUE)
            .description(tr!("minigame.quest.list_empty", loc));
        return send(ctx, e, false).await;
    }
    let mut e = embed(tr!("minigame.quest.list_title", loc), colors::GREEN);
    for q in list.iter().take(25) {
        e = e.field(
            tr!(
                "minigame.quest.field_name",
                loc,
                icon = if q.completed { "✅" } else { "🔄" },
                name = &q.name,
                id = q.id
            ),
            format!(
                "{}\n{}\n{}",
                q.description,
                tr!(
                    "minigame.quest.progress_pct",
                    loc,
                    progress = q.progress,
                    target = q.target,
                    percent = percent(q)
                ),
                rewards_line(q, loc)
            ),
            false,
        );
    }
    send(ctx, e, false).await
}

/// Réclamer les récompenses de quêtes terminées.
#[poise::command(slash_command, rename = "claim")]
pub async fn quest_claim(ctx: Context<'_>, quest_id: Option<i64>) -> Result<(), Error> {
    let Some((g, u)) = gate(ctx, true).await? else {
        return Ok(());
    };
    let loc = ctx.locale();
    let res = db(ctx, move |c| match quest_id {
        Some(id) => Ok(vec![quests::claim_quest(c, &g, &u, id)?]),
        None => quests::claim_all(c, &g, &u),
    })
    .await;
    let results = match res {
        Ok(r) => r,
        Err(e) => return user_error(ctx, "minigame.common.error_title", e).await,
    };
    if results.is_empty() {
        let e = embed(tr!("minigame.quest.claim_none_title", loc), colors::BLUE)
            .description(tr!("minigame.quest.claim_none_description", loc));
        return send(ctx, e, false).await;
    }
    let mut e = embed(tr!("minigame.quest.claim_title", loc), colors::GOLD);
    let (mut coins, mut xp) = (0, 0);
    for r in results.iter().take(20) {
        coins += r.coins;
        xp += r.xp;
        e = e.field(
            format!("✅ {}", r.quest_name),
            format!("🪙 {} | ⭐ {} XP", r.coins, r.xp),
            true,
        );
    }
    e = e.field(
        tr!("minigame.quest.total_rewards", loc),
        tr!(
            "minigame.quest.total_rewards_value",
            loc,
            coins = coins,
            xp = xp
        ),
        false,
    );
    if let Some(level) = results.iter().filter_map(|r| r.new_level).max() {
        e = e.field(
            tr!("minigame.common.level_up_title", loc),
            tr!("minigame.common.level_up_value", loc, level = level),
            false,
        );
    }
    send(ctx, e, false).await
}

// ---------------------------------------------------------------------------
// Boutique, inventaire
// ---------------------------------------------------------------------------

/// Boutique.
#[poise::command(
    slash_command,
    guild_only,
    subcommand_required,
    subcommands("shop_list", "shop_buy")
)]
pub async fn shop(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

/// Voir les objets de la boutique.
#[poise::command(slash_command, rename = "list")]
pub async fn shop_list(ctx: Context<'_>) -> Result<(), Error> {
    if gate(ctx, false).await?.is_none() {
        return Ok(());
    }
    let loc = ctx.locale();
    let items = db(ctx, eco::shop_items).await?;
    if items.is_empty() {
        let e = embed(tr!("minigame.shop.title", loc), colors::BLUE)
            .description(tr!("minigame.shop.empty", loc));
        return send(ctx, e, false).await;
    }
    let mut e = embed(tr!("minigame.shop.title", loc), colors::GREEN)
        .description(tr!("minigame.shop.usage", loc));
    for it in items.iter().take(25) {
        let mut price = Vec::new();
        if it.price_coins > 0 {
            price.push(format!("🪙 {}", it.price_coins));
        }
        if it.price_xp > 0 {
            price.push(format!("⭐ {} XP", it.price_xp));
        }
        let price = if price.is_empty() {
            tr!("minigame.shop.free", loc)
        } else {
            price.join(" + ")
        };
        let stock = if it.stock != -1 {
            format!(" | {}", tr!("minigame.shop.stock", loc, stock = it.stock))
        } else {
            String::new()
        };
        e = e.field(
            format!("#{} - {}", it.id, it.name),
            tr!(
                "minigame.shop.field_value",
                loc,
                description = &it.description,
                price = price,
                stock = stock
            ),
            false,
        );
    }
    send(ctx, e, false).await
}

/// Acheter un objet.
#[poise::command(slash_command, rename = "buy")]
pub async fn shop_buy(ctx: Context<'_>, item_id: i64, quantity: Option<i64>) -> Result<(), Error> {
    let Some((g, u)) = gate(ctx, false).await? else {
        return Ok(());
    };
    let loc = ctx.locale();
    let qty = quantity.unwrap_or(1);
    let (g2, u2) = (g.clone(), u.clone());
    let p = match db(ctx, move |c| eco::buy_item(c, &g2, &u2, item_id, qty)).await {
        Ok(p) => p,
        Err(e) => return user_error(ctx, "minigame.shop.buy_error_title", e).await,
    };
    if p.coins_spent > 0 {
        let spent = p.coins_spent;
        if let Err(e) = db(ctx, move |c| {
            quests::increment_progress_in(c, &g, &u, "coins_spent", spent)
        })
        .await
        {
            tracing::error!("Erreur lors de la progression de quête (shop): {e:#}");
        }
    }
    let mut cost = Vec::new();
    if p.coins_spent > 0 {
        cost.push(format!("🪙 {}", p.coins_spent));
    }
    if p.xp_spent > 0 {
        cost.push(format!("⭐ {} XP", p.xp_spent));
    }
    let mut e = embed(tr!("minigame.shop.buy_success_title", loc), colors::GREEN)
        .description(tr!(
            "minigame.shop.buy_success_description",
            loc,
            quantity = p.quantity,
            item = &p.item_name
        ))
        .field(
            tr!("minigame.shop.cost", loc),
            if cost.is_empty() {
                "-".into()
            } else {
                cost.join(" + ")
            },
            true,
        );
    if p.consumable {
        e = e.field(
            tr!("minigame.common.inventory_field", loc),
            tr!("minigame.shop.inventory_value", loc),
            false,
        );
    }
    if let Some(level) = p.level_down {
        e = e.field(
            tr!("minigame.shop.level_changed_title", loc),
            tr!("minigame.shop.level_changed_value", loc, level = level),
            false,
        );
    }
    send(ctx, e, false).await
}

/// Voir votre inventaire.
#[poise::command(slash_command, guild_only)]
pub async fn inventory(ctx: Context<'_>) -> Result<(), Error> {
    let Some((g, u)) = gate(ctx, false).await? else {
        return Ok(());
    };
    let loc = ctx.locale();
    let items = db(ctx, move |c| eco::inventory(c, &g, &u)).await?;
    let mut e = embed(tr!("minigame.common.inventory_field", loc), colors::BLUE);
    if items.is_empty() {
        return send(
            ctx,
            e.description(tr!("minigame.inventory.empty", loc)),
            false,
        )
        .await;
    }
    for it in items.iter().take(25) {
        e = e.field(
            format!("{} x{}", it.name, it.quantity),
            format!("{}\nID: {}", it.description, it.item_id),
            false,
        );
    }
    send(
        ctx,
        e.footer(serenity::CreateEmbedFooter::new(tr!(
            "minigame.inventory.footer",
            loc
        ))),
        false,
    )
    .await
}

/// Utiliser un objet de votre inventaire.
#[poise::command(slash_command, guild_only, rename = "use")]
pub async fn use_item(ctx: Context<'_>, item_id: i64) -> Result<(), Error> {
    let Some((g, u)) = gate(ctx, false).await? else {
        return Ok(());
    };
    let loc = ctx.locale();
    let used = match db(ctx, move |c| eco::use_item(c, &g, &u, item_id)).await {
        Ok(r) => r,
        Err(e) => return user_error(ctx, "minigame.use.error_title", e).await,
    };
    let mut e = embed(tr!("minigame.use.title", loc), colors::GREEN)
        .description(format!("Used {}!", used.item_name));
    if let Some(effect) = &used.effect {
        e = e.field(
            tr!("minigame.use.effect_field", loc),
            tr!(
                "minigame.use.effect_value",
                loc,
                effect = effect,
                duration = used.duration_minutes
            ),
            false,
        );
    }
    send(ctx, e, false).await
}

// ---------------------------------------------------------------------------
// Échanges
// ---------------------------------------------------------------------------

/// Échanges entre joueurs.
#[poise::command(
    slash_command,
    guild_only,
    subcommand_required,
    subcommands("trade_offer", "trade_accept", "trade_cancel", "trade_pending")
)]
pub async fn trade(_ctx: Context<'_>) -> Result<(), Error> {
    Ok(())
}

fn coins_xp_lines(coins: i64, xp: i64, loc: Loc<'_>) -> Vec<String> {
    let mut v = Vec::new();
    if coins > 0 {
        v.push(tr!("minigame.trade.coins_part", loc, coins = coins));
    }
    if xp > 0 {
        v.push(format!("⭐ {xp} XP"));
    }
    v
}

/// Proposer un échange de pièces et/ou d'XP.
#[poise::command(slash_command, rename = "offer")]
pub async fn trade_offer(
    ctx: Context<'_>,
    user: serenity::Member,
    coins: Option<i64>,
    xp: Option<i64>,
) -> Result<(), Error> {
    let Some((g, from)) = gate(ctx, false).await? else {
        return Ok(());
    };
    let loc = ctx.locale();
    let (coins, xp) = (coins.unwrap_or(0), xp.unwrap_or(0));
    let to = user.user.id.to_string();

    // Perte de niveau : on demande de relancer la même commande sous 30 s.
    if xp > 0 {
        let (g2, f2) = (g.clone(), from.clone());
        let warning = db(ctx, move |c| eco::xp_transfer_warning(c, &g2, &f2, xp)).await?;
        if warning.new_level < warning.current_level {
            let key = (ctx.guild_id().unwrap().get(), ctx.author().id.get());
            let target = (user.user.id.get(), coins, xp);
            let confirmed = {
                let mut map = ctx.data().xp_confirmations.lock().unwrap();
                map.retain(|_, v| v.3.elapsed() <= Duration::from_secs(30));
                match map.get(&key) {
                    Some(p) if (p.0, p.1, p.2) == target => {
                        map.remove(&key);
                        true
                    }
                    _ => {
                        map.insert(key, (target.0, coins, xp, Instant::now()));
                        false
                    }
                }
            };
            if !confirmed {
                let e = embed(tr!("minigame.xp_warning.title", loc), colors::ORANGE)
                    .description(tr!(
                        "minigame.xp_warning.description",
                        loc,
                        current_xp = fmt_num(warning.current_xp),
                        xp_to_transfer = xp,
                        remaining_xp = fmt_num(warning.remaining_xp),
                        current_level = warning.current_level,
                        new_level = warning.new_level,
                        levels_lost = warning.levels_lost()
                    ))
                    .footer(serenity::CreateEmbedFooter::new(tr!(
                        "minigame.xp_warning.footer",
                        loc
                    )));
                return send(ctx, e, true).await;
            }
        }
    }

    let t = match db(ctx, move |c| {
        eco::create_trade(c, &g, &from, &to, coins, xp)
    })
    .await
    {
        Ok(t) => t,
        Err(e) => return user_error(ctx, "minigame.trade.offer_error_title", e).await,
    };
    let net = coins_xp_lines(t.coins - t.tax_coins, t.xp - t.tax_xp, loc).join("\n")
        + &tr!(
            "minigame.trade.tax_line",
            loc,
            coins = t.tax_coins,
            xp = t.tax_xp
        );
    let e = embed(tr!("minigame.trade.offer_title", loc), colors::GREEN)
        .description(tr!(
            "minigame.trade.offer_description",
            loc,
            mention = format!("<@{}>", user.user.id)
        ))
        .field(tr!("minigame.trade.trade_id", loc), t.id.to_string(), true)
        .field(
            tr!("minigame.trade.offering", loc),
            coins_xp_lines(t.coins, t.xp, loc).join("\n"),
            true,
        )
        .field(tr!("minigame.trade.after_tax", loc), net, true)
        .footer(serenity::CreateEmbedFooter::new(tr!(
            "minigame.trade.offer_footer",
            loc,
            user = user.display_name(),
            trade_id = t.id
        )));
    send(ctx, e, false).await
}

/// Accepter un échange.
#[poise::command(slash_command, rename = "accept")]
pub async fn trade_accept(ctx: Context<'_>, trade_id: i64) -> Result<(), Error> {
    let Some((g, u)) = gate(ctx, false).await? else {
        return Ok(());
    };
    let loc = ctx.locale();
    let t = match db(ctx, move |c| eco::accept_trade(c, &g, &u, trade_id)).await? {
        Ok(t) => t,
        Err(msg) => {
            return user_error(
                ctx,
                "minigame.trade.accept_error_title",
                UserError(msg).into(),
            )
            .await;
        }
    };
    let e = embed(tr!("minigame.trade.accepted_title", loc), colors::GREEN)
        .description(tr!("minigame.trade.accepted_description", loc))
        .field(tr!("minigame.trade.trade_id", loc), t.id.to_string(), true)
        .field(
            tr!("minigame.trade.escrow_release", loc),
            tr!(
                "minigame.trade.escrow_release_value",
                loc,
                minutes = eco::ESCROW_MINUTES
            ),
            true,
        )
        .field(
            tr!("minigame.trade.will_receive", loc),
            coins_xp_lines(t.coins - t.tax_coins, t.xp - t.tax_xp, loc).join("\n"),
            false,
        )
        .footer(serenity::CreateEmbedFooter::new(tr!(
            "minigame.trade.accepted_footer",
            loc
        )));
    send(ctx, e, false).await
}

/// Annuler un échange en attente ou en séquestre.
#[poise::command(slash_command, rename = "cancel")]
pub async fn trade_cancel(ctx: Context<'_>, trade_id: i64) -> Result<(), Error> {
    let Some((g, u)) = gate(ctx, false).await? else {
        return Ok(());
    };
    let loc = ctx.locale();
    let refunded = match db(ctx, move |c| eco::cancel_trade(c, &g, &u, trade_id)).await {
        Ok(r) => r,
        Err(e) => return user_error(ctx, "minigame.trade.cancel_error_title", e).await,
    };
    let e = embed(tr!("minigame.trade.canceled_title", loc), colors::GREEN).description(tr!(
        if refunded {
            "minigame.trade.canceled_refunded"
        } else {
            "minigame.trade.canceled"
        },
        loc
    ));
    send(ctx, e, false).await
}

/// Voir vos échanges en attente.
#[poise::command(slash_command, rename = "pending")]
pub async fn trade_pending(ctx: Context<'_>) -> Result<(), Error> {
    let Some((g, u)) = gate(ctx, false).await? else {
        return Ok(());
    };
    let loc = ctx.locale();
    let (sent, received) = db(ctx, move |c| eco::pending_trades(c, &g, &u)).await?;
    let fmt = |list: &[eco::Trade], arrow: &str, sent: bool| {
        if list.is_empty() {
            return tr!("minigame.trade.none", loc);
        }
        list.iter()
            .take(5)
            .map(|t| {
                let other = if sent { &t.to } else { &t.from };
                format!(
                    "**#{}** {arrow} <@{other}>\n🪙 {} | ⭐ {} XP",
                    t.id, t.coins, t.xp
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    let mut e = embed(tr!("minigame.trade.pending_title", loc), colors::BLUE)
        .field(
            tr!("minigame.trade.sent_field", loc),
            fmt(&sent, "→", true),
            true,
        )
        .field(
            tr!("minigame.trade.received_field", loc),
            fmt(&received, "←", false),
            true,
        );
    if sent.is_empty() && received.is_empty() {
        e = e.description(tr!("minigame.trade.pending_empty", loc));
    }
    send(ctx, e, false).await
}

// ---------------------------------------------------------------------------
// Portefeuille, historique
// ---------------------------------------------------------------------------

async fn display_name(ctx: Context<'_>) -> String {
    match ctx.author_member().await {
        Some(m) => m.display_name().to_string(),
        None => ctx.author().display_name().to_string(),
    }
}

/// Voir votre portefeuille.
#[poise::command(slash_command, guild_only)]
pub async fn wallet(ctx: Context<'_>) -> Result<(), Error> {
    let Some((g, u)) = gate(ctx, true).await? else {
        return Ok(());
    };
    let loc = ctx.locale();
    let (b, inv) = db(ctx, move |c| {
        Ok((eco::ensure_user(c, &g, &u)?, eco::inventory(c, &g, &u)?))
    })
    .await?;
    let mut e = embed(
        tr!("minigame.wallet.title", loc, user = display_name(ctx).await),
        colors::GOLD,
    )
    .field(
        tr!("minigame.wallet.coins", loc),
        format!("{:.0}", b.coins),
        true,
    )
    .field(tr!("minigame.wallet.xp", loc), format!("{:.0}", b.xp), true)
    .field(tr!("minigame.wallet.level", loc), b.level.to_string(), true);
    if !inv.is_empty() {
        let count: i64 = inv.iter().map(|i| i.quantity).sum();
        e = e.field(
            tr!("minigame.common.inventory_field", loc),
            tr!("minigame.wallet.inventory_value", loc, count = count),
            true,
        );
    }
    send(ctx, e, false).await
}

#[derive(Debug, poise::ChoiceParameter)]
pub enum HistoryKind {
    #[name = "all"]
    All,
    #[name = "quests"]
    Quests,
    #[name = "shop"]
    Shop,
    #[name = "trades"]
    Trades,
    #[name = "captures"]
    Captures,
    #[name = "duels"]
    Duels,
}

impl HistoryKind {
    fn prefix(&self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Quests => Some("quest_reward"),
            Self::Shop => Some("shop_purchase"),
            Self::Trades => Some("trade"),
            Self::Captures => Some("capture"),
            Self::Duels => Some("duel"),
        }
    }
}

/// Voir l'historique de vos transactions.
#[poise::command(slash_command, guild_only)]
pub async fn history(
    ctx: Context<'_>,
    #[rename = "type"] kind: Option<HistoryKind>,
) -> Result<(), Error> {
    let Some((g, u)) = gate(ctx, true).await? else {
        return Ok(());
    };
    let loc = ctx.locale();
    let prefix = kind.as_ref().and_then(HistoryKind::prefix);
    let txs = db(ctx, move |c| eco::user_transactions(c, &g, &u, 10, prefix)).await?;
    let filter = prefix.map(|p| format!(" ({p})")).unwrap_or_default();
    let mut e = embed(tr!("minigame.history.title", loc), colors::BLUE).description(tr!(
        "minigame.history.description",
        loc,
        filter = filter
    ));
    if txs.is_empty() {
        return send(
            ctx,
            e.description(tr!("minigame.history.empty", loc)),
            false,
        )
        .await;
    }
    for t in &txs {
        let symbol = if t.currency == "coins" { "🪙" } else { "⭐" };
        let amount = if t.amount >= 0.0 {
            format!("+{:.0} {symbol}", t.amount)
        } else {
            format!("{:.0} {symbol}", t.amount)
        };
        let ts = eco::parse_naive(&t.created_at)
            .map(|d| d.and_utc().timestamp())
            .unwrap_or(0);
        e = e.field(title_case(&t.kind), format!("{amount}\n<t:{ts}:R>"), true);
    }
    send(ctx, e, false).await
}

// ---------------------------------------------------------------------------
// Jeux
// ---------------------------------------------------------------------------

async fn track(ctx: Context<'_>, g: &str, u: &str, kind: &'static str, amount: i64) {
    let (g, u) = (g.to_string(), u.to_string());
    if let Err(e) = db(ctx, move |c| {
        quests::increment_progress_in(c, &g, &u, kind, amount)
    })
    .await
    {
        tracing::error!("Erreur lors de la progression de quête ({kind}): {e:#}");
    }
}

/// Tenter une capture en misant des pièces.
#[poise::command(slash_command, guild_only)]
pub async fn capture(ctx: Context<'_>, stake: i64) -> Result<(), Error> {
    let Some((g, u)) = gate(ctx, false).await? else {
        return Ok(());
    };
    let loc = ctx.locale();
    let (g2, u2) = (g.clone(), u.clone());
    let res = db(ctx, move |c| {
        let luck = eco::active_effect(c, &g2, &u2, "capture_luck")?
            .and_then(|d| d["bonus"].as_f64())
            .unwrap_or(0.0);
        eco::capture(c, &g2, &u2, stake, luck)
    })
    .await;
    let r = match res {
        Ok(r) => r,
        Err(e) => return user_error(ctx, "minigame.capture.error_title", e).await,
    };
    track(ctx, &g, &u, "capture_attempt", 1).await;
    if r.success {
        track(ctx, &g, &u, "coins_won_capture", r.net_gain).await;
    }
    let mut e = if r.success {
        embed(tr!("minigame.capture.success_title", loc), colors::GREEN)
            .description(tr!("minigame.capture.success_description", loc))
            .field(
                tr!("minigame.capture.stake", loc),
                format!("🪙 {}", r.stake),
                true,
            )
            .field(
                tr!("minigame.capture.winnings", loc),
                format!("🪙 {}", r.winnings),
                true,
            )
            .field(
                tr!("minigame.capture.net_gain", loc),
                format!("+🪙 {}", r.net_gain),
                true,
            )
            .field(
                tr!("minigame.capture.xp_earned", loc),
                format!("+⭐ {}", r.xp_gained),
                true,
            )
    } else {
        embed(tr!("minigame.capture.fail_title", loc), colors::RED)
            .description(tr!("minigame.capture.fail_description", loc))
            .field(
                tr!("minigame.capture.coins_lost", loc),
                format!("-🪙 {}", r.stake),
                true,
            )
            .field(
                tr!("minigame.capture.consolation_xp", loc),
                format!("+⭐ {}", r.xp_gained),
                true,
            )
    };
    e = e.field(
        tr!("minigame.common.odds_field", loc),
        tr!(
            "minigame.capture.odds_value",
            loc,
            odds = format!("{:.1}", r.odds * 100.0),
            roll = format!("{:.1}", r.roll * 100.0)
        ),
        false,
    );
    if let Some(level) = r.level_up {
        e = e.field(
            tr!("minigame.common.level_up_title", loc),
            tr!("minigame.common.level_up_value", loc, level = level),
            false,
        );
    }
    send(ctx, e, false).await
}

/// Défier un joueur en duel.
#[poise::command(slash_command, guild_only)]
pub async fn duel(ctx: Context<'_>, opponent: serenity::Member, bet: i64) -> Result<(), Error> {
    let Some((g, u)) = gate(ctx, false).await? else {
        return Ok(());
    };
    let loc = ctx.locale();
    let (g2, u2, o) = (g.clone(), u.clone(), opponent.user.id.to_string());
    let r = match db(ctx, move |c| eco::duel(c, &g2, &u2, &o, bet)).await {
        Ok(r) => r,
        Err(e) => return user_error(ctx, "minigame.duel.error_title", e).await,
    };
    track(ctx, &g, &u, "duel_challenge", 1).await;
    let (w, l) = (format!("<@{}>", r.winner), format!("<@{}>", r.loser));
    let e = embed(tr!("minigame.duel.results_title", loc), colors::GOLD)
        .description(tr!(
            "minigame.duel.results_description",
            loc,
            winner = &w,
            loser = &l
        ))
        .field(
            tr!("minigame.duel.bet_field", loc),
            tr!("minigame.duel.bet_value", loc, amount = r.bet),
            true,
        )
        .field(tr!("minigame.duel.pot", loc), format!("🪙 {}", r.pot), true)
        .field(tr!("minigame.duel.tax", loc), format!("🪙 {}", r.tax), true)
        .field(
            tr!("minigame.duel.winner", loc),
            format!("{w}\n+🪙 {} | +⭐ {}", r.net_gain, r.winner_xp),
            true,
        )
        .field(
            tr!("minigame.duel.loser", loc),
            format!("{l}\n-🪙 {} | +⭐ {}", r.bet, r.loser_xp),
            true,
        )
        .field(
            tr!("minigame.common.odds_field", loc),
            format!(
                "<@{u}>: {:.1}% | <@{}>: {:.1}%",
                r.odds.0 * 100.0,
                opponent.user.id,
                r.odds.1 * 100.0
            ),
            false,
        );
    send(ctx, e, false).await
}

/// Voir vos statistiques de jeu.
#[poise::command(slash_command, guild_only)]
pub async fn stats(ctx: Context<'_>) -> Result<(), Error> {
    let Some((g, u)) = gate(ctx, true).await? else {
        return Ok(());
    };
    let loc = ctx.locale();
    let (cap, du) = db(ctx, move |c| {
        Ok((
            eco::game_stats(c, &g, &u, "capture_win", "capture_loss")?,
            eco::game_stats(c, &g, &u, "duel_win", "duel_loss")?,
        ))
    })
    .await?;
    let block = |first: String, s: &eco::GameStats| {
        format!(
            "{first}\n{}\n{}",
            tr!(
                "minigame.stats.wins",
                loc,
                wins = s.wins,
                rate = fmt_num(s.win_rate)
            ),
            tr!(
                "minigame.stats.net",
                loc,
                net = format!("{:+.0}", s.net_profit)
            )
        )
    };
    let e = embed(
        tr!("minigame.stats.title", loc, user = display_name(ctx).await),
        colors::BLUE,
    )
    .field(
        tr!("minigame.stats.captures_field", loc),
        block(
            tr!("minigame.stats.attempts", loc, attempts = cap.total),
            &cap,
        ),
        true,
    )
    .field(
        tr!("minigame.stats.duels_field", loc),
        block(tr!("minigame.stats.fights", loc, fights = du.total), &du),
        true,
    );
    send(ctx, e, false).await
}

/// Toutes les minutes : finalisation des échanges dont le séquestre est écoulé.
pub async fn trade_checker_loop(data: Data) {
    crate::tasks::every(
        "de finalisation des échanges",
        Some("minigame"),
        Duration::from_secs(60),
        || async {
            match data.db.run(eco::complete_ready_trades).await {
                Ok(done) if !done.is_empty() => {
                    tracing::info!("{} échange(s) finalisé(s) automatiquement", done.len())
                }
                Ok(_) => {}
                Err(e) => tracing::error!("Erreur du vérificateur d'échanges: {e:#}"),
            }
        },
    )
    .await;
}

pub fn commands() -> Vec<crate::bot::Command> {
    vec![
        minigame(),
        daily(),
        quest(),
        shop(),
        trade(),
        wallet(),
        history(),
        inventory(),
        use_item(),
        capture(),
        duel(),
        stats(),
    ]
}

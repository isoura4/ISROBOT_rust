//! Démarrage des tâches de fond (équivalent des boucles de `main.py`).
//!
//! Toutes les boucles sont lancées au démarrage ; celles d'un module désactivé
//! (même à chaud via `/feature_disable`) ne font simplement rien.

use std::future::Future;
use std::time::Duration;

use poise::serenity_prelude as serenity;

use crate::bot::Data;
use crate::features::is_enabled;
use crate::{backup, commands};

fn spawn<F>(name: &'static str, fut: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    tracing::debug!("Tâche de fond démarrée: {name}");
    tokio::spawn(fut);
}

/// Exécute `tick` toutes les `period` tant que la fonctionnalité est activée.
pub async fn every<F, Fut>(
    name: &'static str,
    feature: Option<&'static str>,
    period: Duration,
    mut tick: F,
) where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    tracing::info!("Démarrage de la boucle {name}");
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        if feature.is_none_or(is_enabled) {
            tick().await;
        }
    }
}

/// Lance toutes les boucles de fond.
pub fn spawn_all(ctx: serenity::Context, data: Data) {
    spawn(
        "xp_voice",
        commands::xp_voice::award_loop(ctx.clone(), data.clone()),
    );
    spawn(
        "twitch",
        commands::stream::check_loop(ctx.clone(), data.clone()),
    );
    spawn(
        "youtube",
        commands::youtube::check_loop(ctx.clone(), data.clone()),
    );
    spawn(
        "bluesky",
        commands::bluesky::check_loop(ctx.clone(), data.clone()),
    );
    spawn(
        "warning_decay",
        commands::moderation::warning_decay_loop(ctx.clone(), data.clone()),
    );
    spawn(
        "mute_expiration",
        commands::moderation::mute_expiration_loop(ctx.clone(), data.clone()),
    );
    spawn(
        "trades",
        commands::minigame::trade_checker_loop(data.clone()),
    );
    spawn("backup", backup::scheduled_loop(data));
}

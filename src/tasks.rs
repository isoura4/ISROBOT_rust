//! Démarrage des tâches de fond (équivalent des boucles de `main.py`).

use std::future::Future;

use poise::serenity_prelude as serenity;

use crate::bot::Data;
use crate::commands;
use crate::features::is_enabled;

fn spawn<F>(name: &'static str, fut: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    tracing::debug!("Tâche de fond démarrée: {name}");
    tokio::spawn(fut);
}

/// Lance toutes les boucles de fond des modules activés.
pub fn spawn_all(ctx: serenity::Context, data: Data) {
    if is_enabled("xp_voice") {
        spawn(
            "xp_voice",
            commands::xp_voice::award_loop(ctx.clone(), data.clone()),
        );
    }
    // Les boucles de modération tournent toujours, comme en Python.
    spawn(
        "warning_decay",
        commands::moderation::warning_decay_loop(ctx.clone(), data.clone()),
    );
    spawn(
        "mute_expiration",
        commands::moderation::mute_expiration_loop(ctx.clone(), data.clone()),
    );
}

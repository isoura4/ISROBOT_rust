//! Enregistrement des commandes et localisation de leurs descriptions.

pub mod basic;
pub mod count;
pub mod xp_system;

use std::borrow::Cow;

use crate::bot::Command;
use crate::features::is_enabled;
use crate::i18n::{DEFAULT_LOCALE, DISCORD_EN_LOCALES, raw};

/// Liste des commandes des modules activés dans le `.env`.
pub fn all() -> Vec<Command> {
    type Builder = fn() -> Vec<Command>;
    let modules: [(&str, Builder); 5] = [
        ("ping", || vec![basic::ping()]),
        ("ping_bot", || vec![basic::ping_bot()]),
        ("coinflip", || vec![basic::coinflip()]),
        ("count", || vec![count::count()]),
        ("xp_system", || {
            vec![xp_system::level(), xp_system::leaderboard()]
        }),
    ];

    let mut cmds = Vec::new();
    for (feature, build) in modules {
        if is_enabled(feature) {
            cmds.extend(build());
        } else {
            tracing::info!(
                "Module désactivé via .env: {}",
                crate::features::feature_label(feature, None)
            );
        }
    }
    for cmd in &mut cmds {
        localize(cmd);
    }
    cmds
}

/// Applique les descriptions `cmd.<nom>.desc` des fichiers de traduction
/// (français par défaut, anglais pour les clients Discord en anglais).
fn localize(cmd: &mut Command) {
    let key = format!("cmd.{}.desc", cmd.name);
    if let Some(fr) = raw(&key, DEFAULT_LOCALE) {
        cmd.description = Some(Cow::Borrowed(fr));
    }
    if let Some(en) = raw(&key, "en") {
        let mut locs = cmd.description_localizations.to_vec();
        for code in DISCORD_EN_LOCALES {
            locs.push((Cow::Borrowed(code), Cow::Borrowed(en)));
        }
        cmd.description_localizations = Cow::Owned(locs);
    }
}

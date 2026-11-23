//! Enregistrement des commandes et localisation de leurs descriptions.

pub mod ai;
pub mod basic;
pub mod count;
pub mod moderation;
pub mod xp_system;
pub mod xp_voice;

use std::borrow::Cow;

use crate::bot::Command;
use crate::features::is_enabled;
use crate::i18n::{DEFAULT_LOCALE, DISCORD_EN_LOCALES, raw};

type Builder = fn() -> Vec<Command>;

/// (clé de fonctionnalité `COG_*`, commandes du module)
fn modules() -> Vec<(&'static str, Builder)> {
    vec![
        ("ai", || vec![ai::ai()]),
        ("ping", || vec![basic::ping()]),
        ("ping_bot", || vec![basic::ping_bot()]),
        ("coinflip", || vec![basic::coinflip()]),
        ("count", || vec![count::count()]),
        ("xp_system", || {
            vec![xp_system::level(), xp_system::leaderboard()]
        }),
        ("moderation", || {
            vec![
                moderation::warn(),
                moderation::warns(),
                moderation::unwarn(),
                moderation::mute(),
                moderation::unmute(),
                moderation::modlog(),
            ]
        }),
        ("moderation_config", || vec![moderation::modconfig()]),
        ("moderation_context", || {
            vec![moderation::warn_user_context()]
        }),
        ("user_moderation", || vec![moderation::appeal()]),
    ]
}

/// Liste des commandes des modules activés dans le `.env`.
pub fn all() -> Vec<Command> {
    let mut cmds = Vec::new();
    for (feature, build) in modules() {
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
        let path = cmd.name.to_string();
        localize(cmd, &path);
    }
    cmds
}

/// Clé de traduction d'un chemin de commande (`trade offer` → `trade.offer`).
fn key_path(path: &str) -> String {
    path.replace(' ', ".").replace('-', "_")
}

/// Renseigne la valeur française et les traductions anglaises d'une clé.
fn apply(
    key: &str,
    target: &mut Option<Cow<'static, str>>,
    locs: &mut Cow<'static, [(Cow<'static, str>, Cow<'static, str>)]>,
) -> bool {
    let Some(fr) = raw(key, DEFAULT_LOCALE) else {
        return false;
    };
    *target = Some(Cow::Borrowed(fr));
    if let Some(en) = raw(key, "en") {
        let mut v = locs.to_vec();
        for code in DISCORD_EN_LOCALES {
            v.push((Cow::Borrowed(code), Cow::Borrowed(en)));
        }
        *locs = Cow::Owned(v);
    }
    true
}

/// Applique les traductions `cmd.<chemin>.desc`, `cmd.<chemin>.<paramètre>`
/// (ou `cmd.<chemin>.param` pour un paramètre unique) et
/// `cmd.<chemin>.choice.<valeur>` aux commandes, sous-commandes et paramètres.
fn localize(cmd: &mut Command, path: &str) {
    let base = format!("cmd.{}", key_path(path));
    if cmd.context_menu_name.is_none() {
        apply(
            &format!("{base}.desc"),
            &mut cmd.description,
            &mut cmd.description_localizations,
        );
    }
    for p in &mut cmd.parameters {
        let specific = format!("{base}.{}", p.name);
        if !apply(
            &specific,
            &mut p.description,
            &mut p.description_localizations,
        ) {
            apply(
                &format!("{base}.param"),
                &mut p.description,
                &mut p.description_localizations,
            );
        }
        let mut choices = p.choices.to_vec();
        for c in &mut choices {
            let key = format!("{base}.choice.{}", c.name);
            if let Some(fr) = raw(&key, DEFAULT_LOCALE) {
                let mut v = c.localizations.to_vec();
                v.push((Cow::Borrowed("fr"), Cow::Borrowed(fr)));
                if let Some(en) = raw(&key, "en") {
                    for code in DISCORD_EN_LOCALES {
                        v.push((Cow::Borrowed(code), Cow::Borrowed(en)));
                    }
                }
                c.localizations = Cow::Owned(v);
            }
        }
        p.choices = Cow::Owned(choices);
    }
    for sub in &mut cmd.subcommands {
        let sub_path = format!("{path} {}", sub.name);
        localize(sub, &sub_path);
    }
}

/// Route les interactions de composants (boutons...) vers leur module.
pub async fn handle_component(
    ctx: &poise::serenity_prelude::Context,
    data: &crate::bot::Data,
    ci: &poise::serenity_prelude::ComponentInteraction,
) -> Result<(), crate::bot::Error> {
    if moderation::handle_component(ctx, data, ci).await? {
        return Ok(());
    }
    Ok(())
}

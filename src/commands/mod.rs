//! Enregistrement des commandes et localisation de leurs descriptions.

pub mod admin;
pub mod ai;
pub mod basic;
pub mod bluesky;
pub mod count;
pub mod stream;
pub mod xp_system;
pub mod xp_voice;
pub mod youtube;

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
        ("stream", || {
            vec![
                stream::stream_add(),
                stream::stream_remove(),
                stream::stream_list(),
            ]
        }),
        ("youtube", || {
            vec![youtube::youtube_add(), youtube::youtube_remove()]
        }),
        ("bluesky", || {
            vec![
                bluesky::bluesky_add(),
                bluesky::bluesky_remove(),
                bluesky::bluesky_list(),
            ]
        }),
    ]
}

/// Toutes les commandes. Chacune porte sa fonctionnalité dans `category`,
/// ce qui permet de les activer/désactiver à chaud (`/feature_*`, `/reload`).
pub fn all() -> Vec<Command> {
    let mut cmds = Vec::new();
    for (feature, build) in modules() {
        for mut cmd in build() {
            set_feature(&mut cmd, feature);
            let path = cmd.name.to_string();
            localize(&mut cmd, &path);
            cmds.push(cmd);
        }
    }
    cmds.extend(admin::commands().into_iter().map(|mut c| {
        let path = c.name.to_string();
        localize(&mut c, &path);
        c
    }));
    cmds
}

fn set_feature(cmd: &mut Command, feature: &'static str) {
    cmd.category = Some(Cow::Borrowed(feature));
    for sub in &mut cmd.subcommands {
        set_feature(sub, feature);
    }
}

/// La commande appartient-elle à une fonctionnalité activée ?
pub fn command_enabled(cmd: &Command) -> bool {
    cmd.category.as_deref().is_none_or(is_enabled)
}

/// Déclare sur le serveur les commandes des fonctionnalités activées.
pub async fn sync(
    http: impl AsRef<poise::serenity_prelude::Http>,
    cmds: &[Command],
    guild: poise::serenity_prelude::GuildId,
) -> Result<usize, crate::bot::Error> {
    let enabled: Vec<&Command> = cmds.iter().filter(|c| command_enabled(c)).collect();
    for (feature, _) in modules() {
        if !is_enabled(feature) {
            tracing::info!(
                "Module désactivé via .env: {}",
                crate::features::feature_label(feature, None)
            );
        }
    }
    let n = enabled.len();
    poise::builtins::register_in_guild(http, enabled, guild).await?;
    Ok(n)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn check(cmd: &Command, path: &str, problems: &mut Vec<String>) {
        if cmd.context_menu_name.is_none() {
            match &cmd.description {
                Some(d) if !d.is_empty() && d.chars().count() <= 100 => {}
                other => problems.push(format!("{path}: description {other:?}")),
            }
            for (loc, d) in cmd.description_localizations.iter() {
                if d.chars().count() > 100 {
                    problems.push(format!("{path} [{loc}]: description trop longue"));
                }
            }
        }
        for p in cmd
            .parameters
            .iter()
            .filter(|_| cmd.context_menu_name.is_none())
        {
            match &p.description {
                Some(d) if !d.is_empty() && d.chars().count() <= 100 => {}
                other => problems.push(format!("{path} {}: description {other:?}", p.name)),
            }
        }
        for sub in &cmd.subcommands {
            check(sub, &format!("{path} {}", sub.name), problems);
        }
    }

    /// Discord refuse l'enregistrement si une description manque ou dépasse 100 caractères.
    #[test]
    fn all_commands_are_registrable() {
        let cmds = all();
        let mut problems = Vec::new();
        for c in &cmds {
            check(c, &c.name, &mut problems);
        }
        assert!(problems.is_empty(), "{problems:#?}");
        // La conversion vers le format Discord ne doit pas paniquer.
        let built = poise::builtins::create_application_commands(&cmds);
        assert_eq!(built.len(), cmds.len());
        let mut names: Vec<_> = cmds.iter().map(|c| c.name.to_string()).collect();
        names.sort();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "noms de commandes en double");
    }
}

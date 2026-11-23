//! Activation/désactivation des modules via `COG_<NOM>_ENABLED` dans le `.env`.
//!
//! Mêmes clés que la version Python, pour pouvoir réutiliser le même `.env`.

use crate::config::env_bool;

/// (clé, libellé par défaut)
pub const FEATURES: &[(&str, &str)] = &[
    ("ai", "Commande IA"),
    ("coinflip", "Coinflip"),
    ("count", "Compteur"),
    ("ping", "Ping"),
    ("ping_bot", "Ping bot"),
    ("stream", "Streams Twitch"),
    ("xp_system", "XP texte"),
    ("xp_voice", "XP vocal"),
    ("youtube", "YouTube"),
    ("bluesky", "Bluesky"),
];

pub fn feature_env_key(feature: &str) -> String {
    format!("COG_{}_ENABLED", feature.to_uppercase())
}

/// Indique si une fonctionnalité est activée (défaut : activée).
pub fn is_enabled(feature: &str) -> bool {
    let mut enabled = env_bool(&feature_env_key(feature), true);
    match feature {
        "ai" => {
            enabled =
                enabled && env_bool("AI_ENABLED", true) && env_bool("AI_COMMAND_ENABLED", true)
        }
        _ => {}
    }
    enabled
}

/// Libellé localisé d'une fonctionnalité.
pub fn feature_label(feature: &str, locale: Option<&str>) -> String {
    let key = format!("feature.{feature}");
    let label = crate::i18n::t(&key, locale, Vec::new());
    if label == key {
        FEATURES
            .iter()
            .find(|(k, _)| *k == feature)
            .map(|(_, l)| l.to_string())
            .unwrap_or(label)
    } else {
        label
    }
}

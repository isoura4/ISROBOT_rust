//! Traductions basées sur les fichiers `locales/*.json` (embarqués dans le binaire).
//!
//! Les fichiers sont les mêmes que ceux de la version Python, avec la même
//! syntaxe de placeholders (`{nom}` et `{nom:.2f}`).

use std::collections::HashMap;
use std::sync::LazyLock;

pub const DEFAULT_LOCALE: &str = "fr";
pub const SUPPORTED_LOCALES: [&str; 2] = ["fr", "en"];

static LOCALES: LazyLock<HashMap<&'static str, HashMap<String, String>>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    m.insert("fr", load(include_str!("../locales/fr.json")));
    m.insert("en", load(include_str!("../locales/en.json")));
    m
});

fn load(raw: &str) -> HashMap<String, String> {
    serde_json::from_str::<HashMap<String, serde_json::Value>>(raw)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
        .collect()
}

/// Normalise une locale Discord (`en-US`) vers une locale supportée.
pub fn base_locale(locale: Option<&str>) -> &'static str {
    let Some(l) = locale else {
        return DEFAULT_LOCALE;
    };
    let base = l.replace('_', "-").to_lowercase();
    let base = base.split('-').next().unwrap_or_default();
    SUPPORTED_LOCALES
        .iter()
        .find(|s| **s == base)
        .copied()
        .unwrap_or(DEFAULT_LOCALE)
}

/// Retourne la traduction brute (sans repli) pour une locale précise.
pub fn raw(key: &str, locale: &str) -> Option<&'static str> {
    LOCALES.get(locale)?.get(key).map(String::as_str)
}

/// Une valeur à injecter dans une traduction.
pub enum Arg {
    Str(String),
    Float(f64),
}

impl From<&str> for Arg {
    fn from(v: &str) -> Self {
        Arg::Str(v.to_string())
    }
}
impl From<String> for Arg {
    fn from(v: String) -> Self {
        Arg::Str(v)
    }
}
impl From<&String> for Arg {
    fn from(v: &String) -> Self {
        Arg::Str(v.clone())
    }
}
impl From<f64> for Arg {
    fn from(v: f64) -> Self {
        Arg::Float(v)
    }
}
macro_rules! arg_from_int {
    ($($t:ty),*) => {$(
        impl From<$t> for Arg {
            fn from(v: $t) -> Self { Arg::Str(v.to_string()) }
        }
    )*};
}
arg_from_int!(i32, i64, u32, u64, usize);

/// Remplace les placeholders `{nom}` / `{nom:.Nf}` (sous-ensemble de `str.format`).
fn format(template: &str, args: &[(&str, Arg)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let inner = &after[..end];
        let (name, spec) = inner.split_once(':').unwrap_or((inner, ""));
        match args.iter().find(|(k, _)| *k == name) {
            Some((_, value)) => out.push_str(&render(value, spec)),
            None => {
                out.push('{');
                out.push_str(inner);
                out.push('}');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

fn render(value: &Arg, spec: &str) -> String {
    match value {
        Arg::Str(s) => s.clone(),
        Arg::Float(f) => {
            let precision = spec
                .strip_prefix('.')
                .and_then(|s| s.strip_suffix('f'))
                .and_then(|p| p.parse::<usize>().ok());
            match precision {
                Some(p) => format!("{f:.p$}"),
                None => f.to_string(),
            }
        }
    }
}

/// Traduit `key` dans `locale` (repli sur le français puis sur la clé elle-même).
pub fn t(key: &str, locale: Option<&str>, args: Vec<(&str, Arg)>) -> String {
    let loc = base_locale(locale);
    let value = raw(key, loc).or_else(|| raw(key, DEFAULT_LOCALE));
    match value {
        Some(v) if args.is_empty() => v.to_string(),
        Some(v) => format(v, &args),
        None => {
            tracing::debug!("Clé de traduction manquante: {key} (locale={loc})");
            key.to_string()
        }
    }
}

/// Raccourci : `tr!("cle", locale)` ou `tr!("cle", locale, nom = valeur, ...)`.
#[macro_export]
macro_rules! tr {
    ($key:expr, $locale:expr) => {
        $crate::i18n::t($key, $locale, Vec::new())
    };
    ($key:expr, $locale:expr, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::t(
            $key,
            $locale,
            vec![$((stringify!($name), $crate::i18n::Arg::from($value))),+],
        )
    };
}

/// Codes de locale Discord à renseigner pour l'anglais.
pub const DISCORD_EN_LOCALES: [&str; 2] = ["en-US", "en-GB"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_normalization() {
        assert_eq!(base_locale(Some("en-US")), "en");
        assert_eq!(base_locale(Some("fr")), "fr");
        assert_eq!(base_locale(Some("de")), "fr");
        assert_eq!(base_locale(None), "fr");
    }

    #[test]
    fn formatting() {
        assert_eq!(
            tr!("ping_bot.response", Some("fr"), latency = 12.3456),
            "Pong ! Latence : 12.35 ms"
        );
        assert_eq!(
            tr!("count.wrong_number", Some("fr"), expected = 3, actual = "5"),
            "Mauvais chiffre ! Le bon chiffre était 3, mais vous avez mis 5."
        );
    }

    #[test]
    fn fallback_to_key() {
        assert_eq!(tr!("does.not.exist", Some("en")), "does.not.exist");
    }

    #[test]
    fn same_keys_in_all_locales() {
        let fr: std::collections::BTreeSet<_> = LOCALES["fr"].keys().collect();
        let en: std::collections::BTreeSet<_> = LOCALES["en"].keys().collect();
        assert_eq!(fr, en);
    }
}

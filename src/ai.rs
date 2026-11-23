//! Intelligence artificielle : fournisseurs (Ollama / API compatible OpenAI),
//! interrupteurs `AI_*` et filtre de contenu.
//!
//! Portage de `utils/ai_provider.py`, `utils/ai_toggle.py`,
//! et du filtre de `commands/ai.py`.

use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use serde_json::{Value, json};
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

use crate::config::env_bool;

// ---------------------------------------------------------------------------
// Interrupteurs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    Command,
    ContentFilter,
}

/// `AI_ENABLED` est l'interrupteur général, chaque fonctionnalité a le sien.
pub fn enabled(feature: Feature) -> bool {
    let key = match feature {
        Feature::Command => "AI_COMMAND_ENABLED",
        Feature::ContentFilter => "AI_CONTENT_FILTER_ENABLED",
    };
    env_bool("AI_ENABLED", true) && env_bool(key, true)
}

// ---------------------------------------------------------------------------
// Fournisseurs
// ---------------------------------------------------------------------------

const DEFAULT_OLLAMA_HOST: &str = "http://localhost:11434";
const DEFAULT_OLLAMA_MODEL: &str = "llama2";
const DEFAULT_OPENAI_BASE_URL: &str = "https://openrouter.ai/api/v1";
const DEFAULT_OPENAI_MODEL: &str = "openai/gpt-4o-mini";

#[derive(Debug)]
pub enum AiError {
    Config(String),
    Connection(String),
    Timeout(String),
    Other(String),
}

impl std::fmt::Display for AiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AiError::Config(s)
            | AiError::Connection(s)
            | AiError::Timeout(s)
            | AiError::Other(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for AiError {}

fn env_or(key: &str, default: &str) -> String {
    crate::config::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

pub fn provider() -> String {
    env_or("AI_PROVIDER", "ollama").to_lowercase()
}

/// Modèle utilisé par le fournisseur courant.
pub fn active_model() -> String {
    if provider() == "openai" {
        env_or("openai_model", DEFAULT_OPENAI_MODEL)
    } else {
        env_or("ollama_model", DEFAULT_OLLAMA_MODEL)
    }
}

#[derive(Default)]
pub struct ChatOptions {
    /// Modèle explicite (Ollama uniquement pour la modération).
    pub model: Option<String>,
    /// Hôte Ollama explicite.
    pub host: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    pub timeout: Option<Duration>,
}

fn non_empty_str(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

pub fn parse_ollama(data: &Value) -> Option<String> {
    non_empty_str(data.get("message").and_then(|m| m.get("content")))
}

pub fn parse_openai(data: &Value) -> Option<String> {
    non_empty_str(
        data.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content")),
    )
}

async fn post_json(
    http: &reqwest::Client,
    url: &str,
    bearer: Option<&str>,
    body: &Value,
    timeout: Duration,
) -> Result<Value, AiError> {
    let mut req = http.post(url).json(body).timeout(timeout);
    if let Some(key) = bearer {
        req = req.bearer_auth(key);
    }
    let resp = req.send().await.map_err(|e| {
        if e.is_timeout() {
            AiError::Timeout(format!("Délai dépassé pour {url}"))
        } else {
            AiError::Connection(format!("Connexion impossible à {url}"))
        }
    })?;
    let status = resp.status();
    if !status.is_success() {
        let text: String = resp
            .text()
            .await
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect();
        return Err(AiError::Other(format!(
            "HTTP {} depuis {url}: {text}",
            status.as_u16()
        )));
    }
    resp.json::<Value>().await.map_err(|e| {
        if e.is_timeout() {
            AiError::Timeout(format!("Délai dépassé pour {url}"))
        } else {
            AiError::Other(format!("Réponse JSON invalide depuis {url}"))
        }
    })
}

/// Envoie une conversation au fournisseur et renvoie le texte de la réponse
/// (`None` si la réponse est vide ou mal formée).
pub async fn chat(
    http: &reqwest::Client,
    messages: Value,
    opts: ChatOptions,
) -> Result<Option<String>, AiError> {
    let timeout = opts.timeout.unwrap_or(Duration::from_secs(30));
    match provider().as_str() {
        "openai" => {
            let base = env_or("openai_base_url", DEFAULT_OPENAI_BASE_URL);
            let key = crate::config::var("openai_api_key")
                .unwrap_or_default()
                .trim()
                .to_string();
            if key.is_empty() {
                return Err(AiError::Config(
                    "openai_api_key est requis lorsque AI_PROVIDER=openai".into(),
                ));
            }
            let mut body = json!({
                "model": env_or("openai_model", DEFAULT_OPENAI_MODEL),
                "messages": messages,
            });
            if let Some(t) = opts.temperature {
                body["temperature"] = json!(t);
            }
            if let Some(m) = opts.max_tokens {
                body["max_tokens"] = json!(m);
            }
            let url = format!("{}/chat/completions", base.trim_end_matches('/'));
            Ok(parse_openai(
                &post_json(http, &url, Some(&key), &body, timeout).await?,
            ))
        }
        "ollama" => {
            let model = opts
                .model
                .unwrap_or_else(|| env_or("ollama_model", DEFAULT_OLLAMA_MODEL));
            let host = opts
                .host
                .unwrap_or_else(|| env_or("ollama_host", DEFAULT_OLLAMA_HOST));
            let mut body = json!({ "model": model, "messages": messages, "stream": false });
            if let Some(t) = opts.temperature {
                body["options"] = json!({ "temperature": t });
            }
            let url = format!("{}/api/chat", host.trim_end_matches('/'));
            Ok(parse_ollama(
                &post_json(http, &url, None, &body, timeout).await?,
            ))
        }
        other => Err(AiError::Config(format!("Provider IA inconnu: '{other}'"))),
    }
}

// ---------------------------------------------------------------------------
// Filtre de contenu de /ai
// ---------------------------------------------------------------------------

const INAPPROPRIATE_KEYWORDS: &[&str] = &[
    "porn",
    "nsfw",
    "sex",
    "nude",
    "naked",
    "explicit",
    "xxx",
    "drug",
    "cocaine",
    "heroin",
    "meth",
    "illegal",
    "kill",
    "murder",
    "suicide",
    "bomb",
    "weapon",
    "terrorist",
    "hack",
    "exploit",
    "malware",
    "virus",
    "ddos",
    "pornographique",
    "sexuel",
    "nu",
    "explicite",
    "drogue",
    "cocaïne",
    "héroïne",
    "illégal",
    "illégale",
    "tuer",
    "meurtre",
    "bombe",
    "arme",
    "terroriste",
    "piratage",
    "malveillant",
];

/// Retire caractères invisibles et accents, met en minuscules et décode le leetspeak.
pub fn normalize_for_filter(text: &str) -> String {
    text.chars()
        .filter(|c| {
            !matches!(
                c,
                '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{2060}' | '\u{feff}'
            )
        })
        .collect::<String>()
        .nfkd()
        .filter(|c| !is_combining_mark(*c))
        .collect::<String>()
        .to_lowercase()
        .chars()
        .map(|c| match c {
            '0' => 'o',
            '1' | '!' => 'i',
            '3' => 'e',
            '4' | '@' => 'a',
            '5' | '$' => 's',
            '7' => 't',
            c => c,
        })
        .collect()
}

/// Motifs tolérant les lettres répétées (« ssseeexxx ») et la ponctuation
/// intercalée (« s.e.x »). L'espace n'est pas un séparateur.
static FILTER_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    let mut keywords: Vec<String> = INAPPROPRIATE_KEYWORDS
        .iter()
        .map(|k| normalize_for_filter(k))
        .collect();
    keywords.sort();
    keywords.dedup();
    keywords
        .iter()
        .filter(|k| !k.is_empty())
        .map(|k| {
            let body: String = k
                .chars()
                .map(|ch| format!("{}+(?:[^\\w\\s]|_)*", regex::escape(&ch.to_string())))
                .collect();
            Regex::new(&format!(r"\b{body}\b")).expect("motif de filtre")
        })
        .collect()
});

pub fn contains_inappropriate_content(text: &str) -> bool {
    let normalized = normalize_for_filter(text);
    FILTER_PATTERNS.iter().any(|p| p.is_match(&normalized))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_catches_obfuscation_but_not_innocent_words() {
        assert!(contains_inappropriate_content("how to make a b0mb"));
        assert!(contains_inappropriate_content("s.e.x"));
        assert!(contains_inappropriate_content("sssseeexxx"));
        assert!(contains_inappropriate_content("de la drögue"));
        assert!(contains_inappropriate_content("hé\u{200b}roïne"));
        assert!(!contains_inappropriate_content("Bonjour, comment ça va ?"));
        assert!(!contains_inappropriate_content("Sussex is a county"));
        assert!(!contains_inappropriate_content("numéro"));
    }

    #[test]
    fn provider_responses() {
        assert_eq!(
            parse_ollama(&json!({"message": {"content": "hi"}})),
            Some("hi".into())
        );
        assert_eq!(parse_ollama(&json!({"message": {"content": "  "}})), None);
        assert_eq!(
            parse_openai(&json!({"choices": [{"message": {"content": "yo"}}]})),
            Some("yo".into())
        );
        assert_eq!(parse_openai(&json!({"choices": []})), None);
    }
}

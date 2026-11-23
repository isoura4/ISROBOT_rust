//! Chargement et validation de la configuration (.env).
//!
//! Équivalent Rust de `ensure_env_variables()` et
//! `validate_environment_variables()` de la version Python.

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};

use anyhow::{Result, bail};

/// URL de base de l'API REST GameVox (compatible Discord).
pub const GAMEVOX_API_BASE: &str = "https://bot-api.gamevox.com";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Discord,
    GameVox,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub platform: Platform,
    pub app_id: u64,
    pub token: String,
    pub server_id: u64,
    pub db_path: PathBuf,
    /// Dossier du bot (contient `.env`).
    pub root: PathBuf,
}

/// Lit les paires `CLÉ=valeur` non commentées d'un fichier .env.
fn parse_env_entries(content: &str) -> Vec<(String, String)> {
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            l.split_once('=')
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

/// Ajoute dans `.env` les variables présentes dans `.env.example` mais absentes.
pub fn ensure_env_variables(root: &Path) -> Result<()> {
    let example_path = root.join(".env.example");
    let env_path = root.join(".env");
    let Ok(example) = fs::read_to_string(&example_path) else {
        return Ok(());
    };

    let existing: HashSet<String> = fs::read_to_string(&env_path)
        .map(|c| parse_env_entries(&c).into_iter().map(|(k, _)| k).collect())
        .unwrap_or_default();

    let mut seen = HashSet::new();
    let missing: Vec<(String, String)> = parse_env_entries(&example)
        .into_iter()
        .filter(|(k, _)| !existing.contains(k) && seen.insert(k.clone()))
        .collect();

    if !missing.is_empty() {
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&env_path)?;
        writeln!(
            f,
            "\n# Variables ajoutées automatiquement depuis .env.example"
        )?;
        for (k, v) in missing {
            writeln!(f, "{k}={v}")?;
        }
    }
    Ok(())
}

/// Valeurs relues depuis `.env` pendant l'exécution (`/reload`, `/feature_*`).
/// Elles priment sur l'environnement du processus, qu'on ne modifie jamais
/// (ce n'est pas sûr dans un programme multi-thread).
static OVERRIDES: LazyLock<RwLock<HashMap<String, String>>> = LazyLock::new(RwLock::default);

/// Comme `std::env::var`, mais tient compte des valeurs rechargées.
pub fn var(key: &str) -> Result<String, env::VarError> {
    if let Some(v) = OVERRIDES.read().unwrap().get(key) {
        return Ok(v.clone());
    }
    env::var(key)
}

/// Relit le fichier `.env` (les nouvelles valeurs sont prises en compte immédiatement).
pub fn reload_env(root: &Path) -> std::io::Result<usize> {
    let content = fs::read_to_string(root.join(".env"))?;
    let entries = parse_env_entries(&content);
    let n = entries.len();
    let mut map = OVERRIDES.write().unwrap();
    for (k, v) in entries {
        map.insert(k, unquote(&v));
    }
    Ok(n)
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    for q in ['"', '\''] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return v[1..v.len() - 1].to_string();
        }
    }
    v.to_string()
}

/// Écrit `KEY=valeur` dans `.env` (remplace la ligne existante ou l'ajoute),
/// puis applique la valeur immédiatement.
pub fn set_env_key(root: &Path, key: &str, value: &str) -> std::io::Result<()> {
    let path = root.join(".env");
    let content = fs::read_to_string(&path).unwrap_or_default();
    let mut found = false;
    let mut lines: Vec<String> = content
        .lines()
        .map(|line| {
            let t = line.trim_start();
            let is_key =
                !t.starts_with('#') && t.split_once('=').is_some_and(|(k, _)| k.trim() == key);
            if is_key && !found {
                found = true;
                format!("{key}={value}")
            } else {
                line.to_string()
            }
        })
        .collect();
    if !found {
        lines.push(format!("{key}={value}"));
    }
    fs::write(&path, lines.join("\n") + "\n")?;
    OVERRIDES
        .write()
        .unwrap()
        .insert(key.to_string(), value.to_string());
    Ok(())
}

/// Interprète une variable booléenne (`true`, `1`, `yes`, `on`, `enabled`).
pub fn env_bool(key: &str, default: bool) -> bool {
    match var(key) {
        Ok(v) => matches!(
            v.trim().to_lowercase().as_str(),
            "1" | "true" | "yes" | "on" | "enabled"
        ),
        Err(_) => default,
    }
}

fn non_empty(key: &str) -> Option<String> {
    var(key).ok().filter(|v| !v.trim().is_empty())
}

impl Config {
    /// Charge et valide la configuration depuis les variables d'environnement.
    pub fn from_env(root: &Path) -> Result<Self> {
        let platform = match var("PLATFORM")
            .unwrap_or_else(|_| "discord".into())
            .to_lowercase()
            .as_str()
        {
            "gamevox" => Platform::GameVox,
            _ => Platform::Discord,
        };

        let name = match platform {
            Platform::Discord => "Discord",
            Platform::GameVox => "GameVox",
        };
        let token_key = match platform {
            Platform::Discord => "secret_key",
            Platform::GameVox => "gamevox_bot_token",
        };

        let mut missing = Vec::new();
        let mut invalid = Vec::new();

        let db_path = non_empty("db_path");
        if db_path.is_none() {
            missing.push("  - db_path: Le chemin de la base de données est requis".to_string());
        }
        let token = non_empty(token_key);
        if token.is_none() {
            missing.push(format!(
                "  - {token_key}: Le token du bot {name} est requis"
            ));
        }

        let mut parse_id = |key: &str, label: &str| -> u64 {
            match non_empty(key) {
                None => {
                    missing.push(format!("  - {key}: L'ID {label} {name} est requis"));
                    0
                }
                Some(v) => match v.trim().parse::<i64>() {
                    Ok(n) if n > 0 => n as u64,
                    Ok(_) => {
                        invalid.push(format!("  - {key}: Doit être un nombre positif"));
                        0
                    }
                    Err(_) => {
                        invalid.push(format!("  - {key}: Doit être un nombre valide"));
                        0
                    }
                },
            }
        };
        let app_id = parse_id("app_id", "de l'application");
        let server_id = parse_id("server_id", "du serveur");

        if !missing.is_empty() || !invalid.is_empty() {
            let mut msg = String::from("❌ Erreur de configuration:\n");
            if !missing.is_empty() {
                msg += &format!("\nVariables manquantes:\n{}", missing.join("\n"));
            }
            if !invalid.is_empty() {
                msg += &format!("\nVariables invalides:\n{}", invalid.join("\n"));
            }
            msg += "\n\nVeuillez vérifier votre fichier .env et vous assurer que toutes \
                    les variables requises sont définies correctement.";
            bail!(msg);
        }

        let db_path = PathBuf::from(db_path.unwrap());
        let db_path = if db_path.is_absolute() {
            db_path
        } else {
            root.join(db_path)
        };

        Ok(Self {
            platform,
            app_id,
            token: token.unwrap(),
            server_id,
            db_path,
            root: root.to_path_buf(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_key_replaces_or_appends() {
        let dir = std::env::temp_dir().join(format!("isrobot-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".env"), "# c\nA=1\nCOG_PING_ENABLED=true\n").unwrap();
        set_env_key(&dir, "COG_PING_ENABLED", "false").unwrap();
        set_env_key(&dir, "NEW_KEY", "x").unwrap();
        let c = std::fs::read_to_string(dir.join(".env")).unwrap();
        assert_eq!(c, "# c\nA=1\nCOG_PING_ENABLED=false\nNEW_KEY=x\n");
        assert_eq!(var("COG_PING_ENABLED").unwrap(), "false");
        std::fs::write(dir.join(".env"), "TEST_RELOAD_KEY=\"quoted\"\n").unwrap();
        reload_env(&dir).unwrap();
        assert_eq!(var("TEST_RELOAD_KEY").unwrap(), "quoted");
    }

    #[test]
    fn parse_entries_skips_comments() {
        let e = parse_env_entries("# c\nA=1\n\n B = x=y \nnoequals\n");
        assert_eq!(
            e,
            vec![("A".into(), "1".into()), ("B".into(), "x=y".into())]
        );
    }
}

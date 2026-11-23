//! État partagé du bot et utilitaires communs aux commandes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use poise::serenity_prelude as serenity;

use crate::config::Config;
use crate::db::Db;

pub type Error = anyhow::Error;
pub type Context<'a> = poise::Context<'a, Data, Error>;
pub type Command = poise::Command<Data, Error>;
type CounterLocks = HashMap<(u64, u64), Arc<tokio::sync::Mutex<()>>>;

/// Couleurs reprises de `discord.Color`.
pub mod colors {
    pub const BLUE: u32 = 0x3498db;
    pub const GOLD: u32 = 0xf1c40f;
    pub const RED: u32 = 0xe74c3c;
    pub const GREEN: u32 = 0x2ecc71;
    pub const ORANGE: u32 = 0xe67e22;
    pub const PURPLE: u32 = 0x9b59b6;
    pub const LIGHT_GRAY: u32 = 0x979c9f;
    pub const DARK_GRAY: u32 = 0x607d8b;
    pub const TWITCH: u32 = 0x9146ff;
    pub const YOUTUBE: u32 = 0xff0000;
    pub const BLUESKY: u32 = 0x0085ff;
}

/// État partagé, clonable à faible coût (utilisé aussi par les tâches de fond).
#[derive(Clone)]
pub struct Data(Arc<State>);

impl std::ops::Deref for Data {
    type Target = State;
    fn deref(&self) -> &State {
        &self.0
    }
}

pub struct State {
    pub config: Config,
    /// Client HTTP partagé pour les API externes (Twitch, YouTube, IA...).
    pub http: reqwest::Client,
    /// Sessions vocales : dernier crédit d'XP par (serveur, utilisateur), en secondes epoch.
    pub voice_sessions: Mutex<HashMap<(u64, u64), i64>>,
    pub db: Db,
    /// Anti-spam XP : dernier gain par (serveur, utilisateur).
    pub xp_cooldowns: Mutex<HashMap<(u64, u64), Instant>>,
    /// Un verrou par salon de comptage pour éviter les courses.
    pub counter_locks: Mutex<CounterLocks>,
}

impl Data {
    pub fn new(config: Config, db: Db) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .user_agent(concat!("ISROBOT/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("client HTTP");
        Self(Arc::new(State {
            config,
            http,
            voice_sessions: Mutex::default(),
            db,
            xp_cooldowns: Mutex::default(),
            counter_locks: Mutex::default(),
        }))
    }
}

impl State {
    pub fn counter_lock(&self, guild: u64, channel: u64) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.counter_locks.lock().unwrap();
        if locks.len() > 256 {
            // Purge des verrous inutilisés (seul le HashMap en détient une copie).
            locks.retain(|_, l| Arc::strong_count(l) > 1);
        }
        locks.entry((guild, channel)).or_default().clone()
    }
}

/// Locale de l'utilisateur pour une interaction.
pub fn locale(ctx: Context<'_>) -> Option<&str> {
    ctx.locale()
}

pub fn embed(title: impl Into<String>, color: u32) -> serenity::CreateEmbed {
    serenity::CreateEmbed::new().title(title).colour(color)
}

/// Envoie un MP ; renvoie `false` si l'utilisateur n'accepte pas les MP.
pub async fn send_dm(
    http: impl serenity::CacheHttp,
    user: serenity::UserId,
    embed: serenity::CreateEmbed,
) -> bool {
    match user
        .direct_message(http, serenity::CreateMessage::new().embed(embed))
        .await
    {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("Impossible d'envoyer un MP à {user}: {e}");
            false
        }
    }
}

/// Échappe le markdown Discord et neutralise les mentions (`@`, `#`).
pub fn escape_user_content(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    for c in content.chars() {
        if matches!(c, '\\' | '*' | '_' | '~' | '`' | '|' | '>' | '@' | '#') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Tronque une chaîne à `max` caractères (avec « ... »).
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(3)).collect();
        t.push_str("...");
        t
    }
}

/// Défère la réponse d'une commande slash en mode éphémère.
pub async fn defer_ephemeral(ctx: Context<'_>) -> Result<(), Error> {
    ctx.defer_ephemeral().await?;
    Ok(())
}

/// Répond en message éphémère.
pub async fn reply_ephemeral(ctx: Context<'_>, content: impl Into<String>) -> Result<(), Error> {
    ctx.send(
        poise::CreateReply::default()
            .content(content)
            .ephemeral(true),
    )
    .await?;
    Ok(())
}

/// Répond avec un embed éphémère.
pub async fn reply_embed_ephemeral(
    ctx: Context<'_>,
    e: serenity::CreateEmbed,
) -> Result<(), Error> {
    ctx.send(poise::CreateReply::default().embed(e).ephemeral(true))
        .await?;
    Ok(())
}

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
}

pub struct Data {
    pub config: Config,
    pub db: Db,
    /// Anti-spam XP : dernier gain par (serveur, utilisateur).
    pub xp_cooldowns: Mutex<HashMap<(u64, u64), Instant>>,
    /// Un verrou par salon de comptage pour éviter les courses.
    pub counter_locks: Mutex<CounterLocks>,
}

impl Data {
    pub fn new(config: Config, db: Db) -> Self {
        Self {
            config,
            db,
            xp_cooldowns: Mutex::default(),
            counter_locks: Mutex::default(),
        }
    }

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

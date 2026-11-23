//! Économie du minijeu : soldes, journal des transactions, réglages par serveur,
//! délais, boutique, échanges et jeux (capture, duel).
//!
//! Portage de `db_helpers.py`, `shop.py`, `trades.py` et `minigame_engine.py`.
//! Toutes les fonctions prennent une connexion : l'appelant ouvre une
//! transaction (`conn.transaction()`) pour garantir l'atomicité.

use chrono::{Duration, NaiveDateTime, Utc};
use rand::Rng;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use crate::db::utcnow_iso;
use crate::xp::level_from_xp;

/// Erreur « métier » affichée telle quelle à l'utilisateur.
#[derive(Debug)]
pub struct UserError(pub String);

impl std::fmt::Display for UserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for UserError {}

macro_rules! user_err {
    ($($arg:tt)*) => { return Err(UserError(format!($($arg)*)).into()) };
}

pub type Result<T> = anyhow::Result<T>;

fn now_naive() -> NaiveDateTime {
    Utc::now().naive_utc()
}

/// Lit une date naïve UTC stockée par la version Python (`isoformat()`).
pub fn parse_naive(s: &str) -> Option<NaiveDateTime> {
    crate::moderation::parse_iso(s).map(|d| d.naive_utc())
}

// ---------------------------------------------------------------------------
// Soldes et journal
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Balance {
    pub coins: f64,
    pub xp: f64,
    pub level: i64,
}

/// Crée l'utilisateur au besoin et renvoie son solde.
pub fn ensure_user(conn: &Connection, guild: &str, user: &str) -> Result<Balance> {
    let found = conn
        .query_row(
            "SELECT COALESCE(coins, 0), COALESCE(xp, 0), COALESCE(level, 1) FROM users WHERE guildId = ?1 AND userId = ?2",
            params![guild, user],
            |r| Ok(Balance { coins: r.get(0)?, xp: r.get(1)?, level: r.get(2)? }),
        )
        .optional()?;
    if let Some(b) = found {
        return Ok(b);
    }
    conn.execute(
        "INSERT INTO users (guildId, userId, xp, level, messages, coins) VALUES (?1, ?2, 0, 1, 0, 0)",
        params![guild, user],
    )?;
    Ok(Balance {
        coins: 0.0,
        xp: 0.0,
        level: 1,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn log_transaction(
    conn: &Connection,
    guild: &str,
    user: &str,
    kind: &str,
    amount: f64,
    currency: &str,
    balance_after: Option<f64>,
    metadata: Option<&Value>,
    related: Option<(i64, &str)>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO transactions (guildId, userId, kind, amount, currency, balance_after,
            metadata, related_id, related_type, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            guild,
            user,
            kind,
            amount,
            currency,
            balance_after,
            metadata
                .map(|m| m.to_string())
                .unwrap_or_else(|| "{}".into()),
            related.map(|r| r.0),
            related.map(|r| r.1),
            utcnow_iso()
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Ajoute (ou retire si négatif et autorisé) des pièces ; renvoie le nouveau solde.
pub fn add_coins(
    conn: &Connection,
    guild: &str,
    user: &str,
    amount: f64,
    reason: &str,
    related: Option<(i64, &str)>,
    metadata: Option<&Value>,
) -> Result<f64> {
    let b = ensure_user(conn, guild, user)?;
    let new = b.coins + amount;
    conn.execute(
        "UPDATE users SET coins = ?1 WHERE guildId = ?2 AND userId = ?3",
        params![new, guild, user],
    )?;
    log_transaction(
        conn,
        guild,
        user,
        reason,
        amount,
        "coins",
        Some(new),
        metadata,
        related,
    )?;
    Ok(new)
}

pub fn spend_coins(
    conn: &Connection,
    guild: &str,
    user: &str,
    amount: f64,
    reason: &str,
    related: Option<(i64, &str)>,
) -> Result<f64> {
    let b = ensure_user(conn, guild, user)?;
    if b.coins < amount {
        user_err!(
            "Insufficient coins: have {}, need {}",
            fmt_num(b.coins),
            fmt_num(amount)
        );
    }
    let new = b.coins - amount;
    conn.execute(
        "UPDATE users SET coins = ?1 WHERE guildId = ?2 AND userId = ?3",
        params![new, guild, user],
    )?;
    log_transaction(
        conn,
        guild,
        user,
        reason,
        -amount,
        "coins",
        Some(new),
        None,
        related,
    )?;
    Ok(new)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct XpChange {
    pub new_xp: f64,
    pub old_level: i64,
    pub new_level: i64,
}

impl XpChange {
    pub fn level_up(&self) -> bool {
        self.new_level > self.old_level
    }
    pub fn level_down(&self) -> bool {
        self.new_level < self.old_level
    }
}

fn change_xp(
    conn: &Connection,
    guild: &str,
    user: &str,
    delta: f64,
    reason: &str,
    related: Option<(i64, &str)>,
) -> Result<XpChange> {
    let b = ensure_user(conn, guild, user)?;
    if delta < 0.0 && b.xp < -delta {
        user_err!(
            "Insufficient XP: have {}, need {}",
            fmt_num(b.xp),
            fmt_num(-delta)
        );
    }
    let new_xp = b.xp + delta;
    let new_level = level_from_xp(new_xp);
    conn.execute(
        "UPDATE users SET xp = ?1, level = ?2 WHERE guildId = ?3 AND userId = ?4",
        params![new_xp, new_level, guild, user],
    )?;
    log_transaction(
        conn,
        guild,
        user,
        reason,
        delta,
        "xp",
        Some(new_xp),
        None,
        related,
    )?;
    Ok(XpChange {
        new_xp,
        old_level: b.level,
        new_level,
    })
}

pub fn add_xp(
    conn: &Connection,
    guild: &str,
    user: &str,
    amount: f64,
    reason: &str,
    related: Option<(i64, &str)>,
) -> Result<XpChange> {
    change_xp(conn, guild, user, amount, reason, related)
}

pub fn spend_xp(
    conn: &Connection,
    guild: &str,
    user: &str,
    amount: f64,
    reason: &str,
    related: Option<(i64, &str)>,
) -> Result<XpChange> {
    change_xp(conn, guild, user, -amount, reason, related)
}

/// Affiche un nombre sans décimales inutiles.
pub fn fmt_num(n: f64) -> String {
    crate::xp::fmt_xp(n)
}

#[derive(Debug, Clone)]
pub struct TransactionRow {
    pub kind: String,
    pub amount: f64,
    pub currency: String,
    pub created_at: String,
}

/// Dernières transactions ; `kind` sert de préfixe (« capture » → capture_win, capture_loss...).
pub fn user_transactions(
    conn: &Connection,
    guild: &str,
    user: &str,
    limit: i64,
    kind: Option<&str>,
) -> Result<Vec<TransactionRow>> {
    let map = |r: &rusqlite::Row<'_>| {
        Ok(TransactionRow {
            kind: r.get(0)?,
            amount: r.get(1)?,
            currency: r
                .get::<_, Option<String>>(2)?
                .unwrap_or_else(|| "coins".into()),
            created_at: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
        })
    };
    let sql = "SELECT kind, amount, currency, created_at FROM transactions
               WHERE guildId = ?1 AND userId = ?2 AND kind LIKE ?3 || '%'
               ORDER BY created_at DESC LIMIT ?4";
    Ok(conn
        .prepare(sql)?
        .query_map(params![guild, user, kind.unwrap_or(""), limit], map)?
        .collect::<rusqlite::Result<_>>()?)
}

// ---------------------------------------------------------------------------
// Réglages par serveur
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct GuildSettings {
    pub minigame_enabled: bool,
    pub minigame_channel_id: Option<String>,
    pub xp_trading_enabled: bool,
    pub trade_tax_percent: f64,
    pub duel_tax_percent: f64,
    pub daily_xp_transfer_cap_percent: f64,
    pub daily_xp_transfer_cap_max: i64,
    pub capture_cooldown_seconds: i64,
    pub duel_cooldown_seconds: i64,
}

pub fn guild_settings(conn: &Connection, guild: &str) -> Result<GuildSettings> {
    conn.execute(
        "INSERT OR IGNORE INTO guild_settings (guildId) VALUES (?1)",
        [guild],
    )?;
    Ok(conn.query_row(
        "SELECT COALESCE(minigame_enabled, 1), minigame_channel_id, COALESCE(xp_trading_enabled, 1),
                COALESCE(trade_tax_percent, 10.0), COALESCE(duel_tax_percent, 10.0),
                COALESCE(daily_xp_transfer_cap_percent, 10.0), COALESCE(daily_xp_transfer_cap_max, 500),
                COALESCE(capture_cooldown_seconds, 60), COALESCE(duel_cooldown_seconds, 300)
         FROM guild_settings WHERE guildId = ?1",
        [guild],
        |r| {
            Ok(GuildSettings {
                minigame_enabled: r.get::<_, i64>(0)? != 0,
                minigame_channel_id: r.get::<_, Option<String>>(1)?.filter(|s| !s.is_empty()),
                xp_trading_enabled: r.get::<_, i64>(2)? != 0,
                trade_tax_percent: r.get(3)?,
                duel_tax_percent: r.get(4)?,
                daily_xp_transfer_cap_percent: r.get(5)?,
                daily_xp_transfer_cap_max: r.get(6)?,
                capture_cooldown_seconds: r.get(7)?,
                duel_cooldown_seconds: r.get(8)?,
            })
        },
    )?)
}

pub fn set_minigame_enabled(conn: &Connection, guild: &str, enabled: bool) -> Result<()> {
    guild_settings(conn, guild)?;
    conn.execute(
        "UPDATE guild_settings SET minigame_enabled = ?1, updated_at = ?2 WHERE guildId = ?3",
        params![enabled as i64, utcnow_iso(), guild],
    )?;
    Ok(())
}

pub fn set_minigame_channel(conn: &Connection, guild: &str, channel: Option<&str>) -> Result<()> {
    guild_settings(conn, guild)?;
    conn.execute(
        "UPDATE guild_settings SET minigame_channel_id = ?1, updated_at = ?2 WHERE guildId = ?3",
        params![channel, utcnow_iso(), guild],
    )?;
    Ok(())
}

pub fn add_exception_channel(conn: &Connection, guild: &str, channel: &str) -> Result<bool> {
    Ok(conn.execute(
        "INSERT OR IGNORE INTO quest_exception_channels (guildId, channelId) VALUES (?1, ?2)",
        params![guild, channel],
    )? > 0)
}

pub fn remove_exception_channel(conn: &Connection, guild: &str, channel: &str) -> Result<bool> {
    Ok(conn.execute(
        "DELETE FROM quest_exception_channels WHERE guildId = ?1 AND channelId = ?2",
        params![guild, channel],
    )? > 0)
}

pub fn exception_channel_count(conn: &Connection, guild: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM quest_exception_channels WHERE guildId = ?1",
        [guild],
        |r| r.get(0),
    )?)
}

// ---------------------------------------------------------------------------
// Délais et suivi quotidien
// ---------------------------------------------------------------------------

/// Secondes restantes avant de pouvoir refaire l'action (0 = disponible).
pub fn cooldown_remaining(
    conn: &Connection,
    guild: &str,
    user: &str,
    action: &str,
    cooldown: i64,
) -> Result<i64> {
    let last: Option<String> = conn
        .query_row(
            "SELECT last_action_at FROM user_cooldowns WHERE guildId = ?1 AND userId = ?2 AND action_type = ?3",
            params![guild, user, action],
            |r| r.get(0),
        )
        .optional()?;
    let Some(last) = last.as_deref().and_then(parse_naive) else {
        return Ok(0);
    };
    let elapsed = (now_naive() - last).num_milliseconds() as f64 / 1000.0;
    Ok(if elapsed >= cooldown as f64 {
        0
    } else {
        (cooldown as f64 - elapsed) as i64
    })
}

pub fn set_cooldown(conn: &Connection, guild: &str, user: &str, action: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO user_cooldowns (guildId, userId, action_type, last_action_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(guildId, userId, action_type) DO UPDATE SET last_action_at = excluded.last_action_at",
        params![guild, user, action, utcnow_iso()],
    )?;
    Ok(())
}

#[derive(Debug, Clone, Default)]
pub struct DailyTracking {
    pub last_daily_claim: Option<String>,
    pub streak: i64,
    pub daily_xp_transferred: i64,
    pub last_xp_transfer_reset: Option<String>,
}

pub fn daily_tracking(conn: &Connection, guild: &str, user: &str) -> Result<DailyTracking> {
    conn.execute(
        "INSERT OR IGNORE INTO user_daily_tracking (guildId, userId) VALUES (?1, ?2)",
        params![guild, user],
    )?;
    Ok(conn.query_row(
        "SELECT last_daily_claim, COALESCE(streak, 0), COALESCE(daily_xp_transferred, 0), last_xp_transfer_reset
         FROM user_daily_tracking WHERE guildId = ?1 AND userId = ?2",
        params![guild, user],
        |r| {
            Ok(DailyTracking {
                last_daily_claim: r.get(0)?,
                streak: r.get(1)?,
                daily_xp_transferred: r.get(2)?,
                last_xp_transfer_reset: r.get(3)?,
            })
        },
    )?)
}

/// (peut transférer, déjà transféré aujourd'hui, plafond du jour).
pub fn check_daily_xp_limit(
    conn: &Connection,
    guild: &str,
    user: &str,
    amount: i64,
) -> Result<(bool, i64, i64)> {
    let settings = guild_settings(conn, guild)?;
    let b = ensure_user(conn, guild, user)?;
    let mut tracking = daily_tracking(conn, guild, user)?;
    let limit = (b.xp * settings.daily_xp_transfer_cap_percent / 100.0)
        .min(settings.daily_xp_transfer_cap_max as f64) as i64;
    if let Some(last) = tracking
        .last_xp_transfer_reset
        .as_deref()
        .and_then(parse_naive)
        && (now_naive() - last).num_days() >= 1
    {
        conn.execute(
            "UPDATE user_daily_tracking SET daily_xp_transferred = 0, last_xp_transfer_reset = ?1
             WHERE guildId = ?2 AND userId = ?3",
            params![utcnow_iso(), guild, user],
        )?;
        tracking.daily_xp_transferred = 0;
    }
    let current = tracking.daily_xp_transferred;
    Ok((limit - current >= amount, current, limit))
}

pub fn record_xp_transfer(conn: &Connection, guild: &str, user: &str, amount: i64) -> Result<()> {
    let tracking = daily_tracking(conn, guild, user)?;
    conn.execute(
        "UPDATE user_daily_tracking SET daily_xp_transferred = ?1,
            last_xp_transfer_reset = COALESCE(last_xp_transfer_reset, ?2)
         WHERE guildId = ?3 AND userId = ?4",
        params![
            tracking.daily_xp_transferred + amount,
            utcnow_iso(),
            guild,
            user
        ],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Boutique
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ShopItem {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub price_coins: i64,
    pub price_xp: i64,
    pub consumable: bool,
    pub stock: i64,
    pub active: bool,
}

fn map_item(r: &rusqlite::Row<'_>) -> rusqlite::Result<ShopItem> {
    Ok(ShopItem {
        id: r.get(0)?,
        name: r.get(1)?,
        description: r.get(2)?,
        price_coins: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
        price_xp: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
        consumable: r.get::<_, Option<i64>>(5)?.unwrap_or(1) != 0,
        stock: r.get::<_, Option<i64>>(6)?.unwrap_or(-1),
        active: r.get::<_, Option<i64>>(7)?.unwrap_or(1) != 0,
    })
}

const ITEM_COLUMNS: &str =
    "id, name, description, price_coins, price_xp, consumable, stock, active";

pub fn shop_items(conn: &Connection) -> Result<Vec<ShopItem>> {
    Ok(conn
        .prepare(&format!(
            "SELECT {ITEM_COLUMNS} FROM shop_items WHERE active = 1 AND (stock = -1 OR stock > 0)
             ORDER BY price_coins, price_xp"
        ))?
        .query_map([], map_item)?
        .collect::<rusqlite::Result<_>>()?)
}

#[derive(Debug, Clone)]
pub struct Purchase {
    pub item_name: String,
    pub quantity: i64,
    pub coins_spent: i64,
    pub xp_spent: i64,
    pub consumable: bool,
    pub level_down: Option<i64>,
}

pub fn buy_item(
    conn: &Connection,
    guild: &str,
    user: &str,
    item_id: i64,
    quantity: i64,
) -> Result<Purchase> {
    if quantity < 1 {
        user_err!("Quantity must be at least 1");
    }
    let item = conn
        .query_row(
            &format!("SELECT {ITEM_COLUMNS} FROM shop_items WHERE id = ?1"),
            [item_id],
            map_item,
        )
        .optional()?;
    let Some(item) = item else {
        user_err!("Item not found")
    };
    if !item.active {
        user_err!("Item is not available for purchase");
    }
    let total_coins = item.price_coins * quantity;
    let total_xp = item.price_xp * quantity;
    let b = ensure_user(conn, guild, user)?;
    if b.coins < total_coins as f64 {
        user_err!(
            "Not enough coins: have {}, need {total_coins}",
            fmt_num(b.coins)
        );
    }
    if total_xp > 0 && b.xp < total_xp as f64 {
        user_err!("Not enough XP: have {}, need {total_xp}", fmt_num(b.xp));
    }
    if item.stock != -1 {
        let n = conn.execute(
            "UPDATE shop_items SET stock = stock - ?1 WHERE id = ?2 AND stock >= ?1",
            params![quantity, item_id],
        )?;
        if n == 0 {
            user_err!("Not enough stock: only {} available", item.stock.max(0));
        }
    }
    let related = Some((item_id, "shop_item"));
    if total_coins > 0 {
        spend_coins(
            conn,
            guild,
            user,
            total_coins as f64,
            "shop_purchase",
            related,
        )?;
    }
    let mut level_down = None;
    if total_xp > 0 {
        let c = spend_xp(conn, guild, user, total_xp as f64, "shop_purchase", related)?;
        if c.level_down() {
            level_down = Some(c.new_level);
        }
    }
    if item.consumable {
        conn.execute(
            "INSERT INTO user_inventory (guildId, userId, itemId, quantity) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(guildId, userId, itemId) DO UPDATE SET quantity = quantity + excluded.quantity",
            params![guild, user, item_id, quantity],
        )?;
    }
    Ok(Purchase {
        item_name: item.name,
        quantity,
        coins_spent: total_coins,
        xp_spent: total_xp,
        consumable: item.consumable,
        level_down,
    })
}

#[derive(Debug, Clone)]
pub struct InventoryItem {
    pub item_id: i64,
    pub name: String,
    pub description: String,
    pub quantity: i64,
}

pub fn inventory(conn: &Connection, guild: &str, user: &str) -> Result<Vec<InventoryItem>> {
    Ok(conn
        .prepare(
            "SELECT ui.itemId, si.name, si.description, ui.quantity
             FROM user_inventory ui JOIN shop_items si ON ui.itemId = si.id
             WHERE ui.guildId = ?1 AND ui.userId = ?2 AND ui.quantity > 0
             ORDER BY ui.acquired_at DESC",
        )?
        .query_map(params![guild, user], |r| {
            Ok(InventoryItem {
                item_id: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
                quantity: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

#[derive(Debug, Clone)]
pub struct UsedItem {
    pub item_name: String,
    pub effect: Option<String>,
    pub duration_minutes: i64,
}

pub fn use_item(conn: &Connection, guild: &str, user: &str, item_id: i64) -> Result<UsedItem> {
    let row: Option<(i64, String, Option<String>)> = conn
        .query_row(
            "SELECT ui.quantity, si.name, si.metadata FROM user_inventory ui
             JOIN shop_items si ON ui.itemId = si.id
             WHERE ui.guildId = ?1 AND ui.userId = ?2 AND ui.itemId = ?3",
            params![guild, user, item_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((qty, name, meta)) = row.filter(|r| r.0 >= 1) else {
        user_err!("You don't have this item in your inventory");
    };
    let _ = qty;
    let metadata: Value = meta
        .as_deref()
        .and_then(|m| serde_json::from_str(m).ok())
        .unwrap_or(Value::Null);
    let n = conn.execute(
        "UPDATE user_inventory SET quantity = quantity - 1
         WHERE guildId = ?1 AND userId = ?2 AND itemId = ?3 AND quantity > 0",
        params![guild, user, item_id],
    )?;
    if n == 0 {
        user_err!("You don't have this item in your inventory");
    }
    let effect = metadata["effect"].as_str().map(str::to_string);
    let duration = metadata["duration_minutes"].as_i64().unwrap_or(60);
    if let Some(effect) = &effect {
        let expires = (now_naive() + Duration::minutes(duration))
            .format("%Y-%m-%dT%H:%M:%S%.6f")
            .to_string();
        conn.execute(
            "INSERT INTO user_active_effects (guildId, userId, effect_type, effect_data, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(guildId, userId, effect_type)
             DO UPDATE SET effect_data = excluded.effect_data, expires_at = excluded.expires_at",
            params![guild, user, effect, metadata.to_string(), expires],
        )?;
    }
    Ok(UsedItem {
        item_name: name,
        effect,
        duration_minutes: duration,
    })
}

/// Données d'un effet actif (non expiré), s'il existe.
pub fn active_effect(
    conn: &Connection,
    guild: &str,
    user: &str,
    effect: &str,
) -> Result<Option<Value>> {
    let row: Option<(Option<String>, String)> = conn
        .query_row(
            "SELECT effect_data, expires_at FROM user_active_effects
             WHERE guildId = ?1 AND userId = ?2 AND effect_type = ?3",
            params![guild, user, effect],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(row.and_then(|(data, exp)| {
        let expired = parse_naive(&exp).is_none_or(|e| e <= now_naive());
        (!expired).then(|| {
            data.and_then(|d| serde_json::from_str(&d).ok())
                .unwrap_or(Value::Null)
        })
    }))
}

// ---------------------------------------------------------------------------
// Échanges
// ---------------------------------------------------------------------------

pub const ESCROW_MINUTES: i64 = 5;

#[derive(Debug, Clone)]
pub struct Trade {
    pub id: i64,
    pub guild: String,
    pub from: String,
    pub to: String,
    pub coins: i64,
    pub xp: i64,
    pub status: String,
    pub tax_coins: i64,
    pub tax_xp: i64,
    pub escrow_release_at: Option<String>,
}

fn get_trade(conn: &Connection, id: i64) -> Result<Option<Trade>> {
    Ok(conn
        .query_row(
            "SELECT id, guildId, fromUserId, toUserId, COALESCE(coins, 0), COALESCE(xp, 0), status,
                    COALESCE(tax_coins, 0), COALESCE(tax_xp, 0), escrow_release_at
             FROM trades WHERE id = ?1",
            [id],
            |r| {
                Ok(Trade {
                    id: r.get(0)?,
                    guild: r.get(1)?,
                    from: r.get(2)?,
                    to: r.get(3)?,
                    coins: r.get(4)?,
                    xp: r.get(5)?,
                    status: r.get(6)?,
                    tax_coins: r.get(7)?,
                    tax_xp: r.get(8)?,
                    escrow_release_at: r.get(9)?,
                })
            },
        )
        .optional()?)
}

/// Avertissement de perte de niveau lors d'un transfert d'XP.
#[derive(Debug, Clone, PartialEq)]
pub struct XpTransferWarning {
    pub current_xp: f64,
    pub remaining_xp: f64,
    pub current_level: i64,
    pub new_level: i64,
}

impl XpTransferWarning {
    pub fn levels_lost(&self) -> i64 {
        (self.current_level - self.new_level).max(0)
    }
}

pub fn xp_transfer_warning(
    conn: &Connection,
    guild: &str,
    user: &str,
    xp: i64,
) -> Result<XpTransferWarning> {
    let b = ensure_user(conn, guild, user)?;
    let remaining = b.xp - xp as f64;
    Ok(XpTransferWarning {
        current_xp: b.xp,
        remaining_xp: remaining,
        current_level: level_from_xp(b.xp),
        new_level: level_from_xp(remaining.max(0.0)),
    })
}

pub fn create_trade(
    conn: &Connection,
    guild: &str,
    from: &str,
    to: &str,
    coins: i64,
    xp: i64,
) -> Result<Trade> {
    if from == to {
        user_err!("Cannot trade with yourself");
    }
    if coins < 0 || xp < 0 {
        user_err!("Cannot offer negative amounts");
    }
    if coins == 0 && xp == 0 {
        user_err!("Must offer at least some coins or XP");
    }
    let settings = guild_settings(conn, guild)?;
    if xp > 0 && !settings.xp_trading_enabled {
        user_err!("XP trading is disabled on this server");
    }
    let sender = ensure_user(conn, guild, from)?;
    if sender.coins < coins as f64 {
        user_err!(
            "Not enough coins: have {}, offering {coins}",
            fmt_num(sender.coins)
        );
    }
    if sender.xp < xp as f64 {
        user_err!("Not enough XP: have {}, offering {xp}", fmt_num(sender.xp));
    }
    if xp > 0 {
        let (ok, current, limit) = check_daily_xp_limit(conn, guild, from, xp)?;
        if !ok {
            user_err!(
                "Daily XP transfer limit reached. You can transfer {} more XP today (limit: {limit})",
                limit - current
            );
        }
    }
    let pending: i64 = conn.query_row(
        "SELECT COUNT(*) FROM trades WHERE guildId = ?1 AND fromUserId = ?2 AND toUserId = ?3 AND status = 'pending'",
        params![guild, from, to],
        |r| r.get(0),
    )?;
    if pending > 0 {
        user_err!("You already have a pending trade with this user");
    }
    let tax_coins = (coins as f64 * settings.trade_tax_percent / 100.0) as i64;
    let tax_xp = (xp as f64 * settings.trade_tax_percent / 100.0) as i64;
    conn.execute(
        "INSERT INTO trades (guildId, fromUserId, toUserId, coins, xp, status, tax_coins, tax_xp, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7, ?8)",
        params![guild, from, to, coins, xp, tax_coins, tax_xp, utcnow_iso()],
    )?;
    Ok(Trade {
        id: conn.last_insert_rowid(),
        guild: guild.into(),
        from: from.into(),
        to: to.into(),
        coins,
        xp,
        status: "pending".into(),
        tax_coins,
        tax_xp,
        escrow_release_at: None,
    })
}

pub fn pending_trades(
    conn: &Connection,
    guild: &str,
    user: &str,
) -> Result<(Vec<Trade>, Vec<Trade>)> {
    let ids = |col: &str| -> Result<Vec<i64>> {
        Ok(conn
            .prepare(&format!(
                "SELECT id FROM trades WHERE guildId = ?1 AND {col} = ?2 AND status = 'pending' ORDER BY created_at DESC"
            ))?
            .query_map(params![guild, user], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    };
    let load = |ids: Vec<i64>| -> Result<Vec<Trade>> {
        Ok(ids
            .into_iter()
            .filter_map(|i| get_trade(conn, i).ok().flatten())
            .collect())
    };
    Ok((load(ids("fromUserId")?)?, load(ids("toUserId")?)?))
}

/// Accepte un échange : les fonds partent en séquestre pour `ESCROW_MINUTES`.
/// Renvoie l'échange ; `Err(UserError)` en cas de refus. Une annulation pour
/// fonds insuffisants est enregistrée même si une erreur est renvoyée.
pub fn accept_trade(
    conn: &Connection,
    guild: &str,
    user: &str,
    id: i64,
) -> Result<std::result::Result<Trade, String>> {
    let Some(t) = get_trade(conn, id)? else {
        return Ok(Err("Trade not found".into()));
    };
    if t.guild != guild {
        return Ok(Err("Trade not found in this guild".into()));
    }
    if t.to != user {
        return Ok(Err("You are not the recipient of this trade".into()));
    }
    if t.status != "pending" {
        return Ok(Err(format!("Trade is not pending (status: {})", t.status)));
    }
    let sender = ensure_user(conn, guild, &t.from)?;
    let cancel = |msg: &str| -> Result<std::result::Result<Trade, String>> {
        conn.execute(
            "UPDATE trades SET status = 'canceled' WHERE id = ?1 AND status = 'pending'",
            [id],
        )?;
        Ok(Err(msg.to_string()))
    };
    if sender.coins < t.coins as f64 {
        return cancel("Trade canceled: sender no longer has enough coins");
    }
    if sender.xp < t.xp as f64 {
        return cancel("Trade canceled: sender no longer has enough XP");
    }
    if t.xp > 0 {
        if !guild_settings(conn, guild)?.xp_trading_enabled {
            return Ok(Err("XP trading is disabled on this server".into()));
        }
        let (ok, current, limit) = check_daily_xp_limit(conn, guild, &t.from, t.xp)?;
        if !ok {
            return Ok(Err(format!(
                "Trade canceled: sender has reached the daily XP transfer limit ({} XP remaining)",
                (limit - current).max(0)
            )));
        }
    }
    let now = now_naive();
    let release = (now + Duration::minutes(ESCROW_MINUTES))
        .format("%Y-%m-%dT%H:%M:%S%.6f")
        .to_string();
    let n = conn.execute(
        "UPDATE trades SET status = 'accepted', accepted_at = ?1, escrow_release_at = ?2
         WHERE id = ?3 AND status = 'pending'",
        params![utcnow_iso(), release, id],
    )?;
    if n == 0 {
        return Ok(Err("Trade is no longer pending".into()));
    }
    let related = Some((id, "trade"));
    if t.coins > 0 {
        spend_coins(
            conn,
            guild,
            &t.from,
            t.coins as f64,
            "trade_escrow",
            related,
        )?;
    }
    if t.xp > 0 {
        spend_xp(conn, guild, &t.from, t.xp as f64, "trade_escrow", related)?;
    }
    Ok(Ok(Trade {
        status: "accepted".into(),
        escrow_release_at: Some(release),
        ..t
    }))
}

fn complete_trade(conn: &Connection, id: i64) -> Result<()> {
    let Some(t) = get_trade(conn, id)? else {
        user_err!("Trade not found")
    };
    if t.status != "accepted" {
        user_err!("Trade cannot be completed (status: {})", t.status);
    }
    let Some(release) = t.escrow_release_at.as_deref().and_then(parse_naive) else {
        user_err!("Trade has no escrow release time");
    };
    if now_naive() < release {
        user_err!("Escrow period not over.");
    }
    let n = conn.execute(
        "UPDATE trades SET status = 'completed', completed_at = ?1 WHERE id = ?2 AND status = 'accepted'",
        params![utcnow_iso(), id],
    )?;
    if n == 0 {
        user_err!("Trade cannot be completed");
    }
    let related = Some((id, "trade"));
    let (net_coins, net_xp) = (t.coins - t.tax_coins, t.xp - t.tax_xp);
    if net_coins > 0 {
        add_coins(
            conn,
            &t.guild,
            &t.to,
            net_coins as f64,
            "trade_received",
            related,
            None,
        )?;
    }
    if net_xp > 0 {
        add_xp(
            conn,
            &t.guild,
            &t.to,
            net_xp as f64,
            "trade_received",
            related,
        )?;
        record_xp_transfer(conn, &t.guild, &t.from, t.xp)?;
    }
    Ok(())
}

/// Annule un échange ; renvoie `true` si l'expéditeur a été remboursé.
pub fn cancel_trade(conn: &Connection, guild: &str, user: &str, id: i64) -> Result<bool> {
    let Some(t) = get_trade(conn, id)? else {
        user_err!("Trade not found")
    };
    if t.guild != guild {
        user_err!("Trade not found in this guild");
    }
    let (is_sender, is_recipient) = (t.from == user, t.to == user);
    if !is_sender && !is_recipient {
        user_err!("You are not involved in this trade");
    }
    match t.status.as_str() {
        "completed" => user_err!("Cannot cancel a completed trade"),
        "canceled" => user_err!("Trade is already canceled"),
        "accepted" if !is_sender => user_err!("Only the sender can cancel during escrow period"),
        _ => {}
    }
    let n = conn.execute(
        "UPDATE trades SET status = 'canceled' WHERE id = ?1 AND status = ?2",
        params![id, t.status],
    )?;
    if n == 0 {
        user_err!("Trade could not be canceled (status changed)");
    }
    let refunded = t.status == "accepted";
    if refunded {
        let related = Some((id, "trade"));
        if t.coins > 0 {
            add_coins(
                conn,
                guild,
                &t.from,
                t.coins as f64,
                "trade_refund",
                related,
                None,
            )?;
        }
        if t.xp > 0 {
            add_xp(conn, guild, &t.from, t.xp as f64, "trade_refund", related)?;
        }
    }
    Ok(refunded)
}

/// Termine les échanges dont le séquestre est écoulé ; renvoie leurs IDs.
pub fn complete_ready_trades(conn: &mut Connection) -> Result<Vec<i64>> {
    let ids: Vec<(i64, String)> = conn
        .prepare("SELECT id, escrow_release_at FROM trades WHERE status = 'accepted'")?
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let now = now_naive();
    let mut done = Vec::new();
    for (id, release) in ids {
        if parse_naive(&release).is_none_or(|r| r > now) {
            continue;
        }
        let tx = conn.transaction()?;
        match complete_trade(&tx, id) {
            Ok(()) => {
                tx.commit()?;
                done.push(id);
            }
            Err(e) => tracing::error!("Échec de la finalisation de l'échange {id}: {e:#}"),
        }
    }
    Ok(done)
}

// ---------------------------------------------------------------------------
// Jeux : capture et duel
// ---------------------------------------------------------------------------

const CAPTURE_HOUSE_EDGE: f64 = 0.95;

pub fn capture_odds(xp: f64, stake: f64) -> f64 {
    let level = level_from_xp(xp) as f64;
    let xp_bonus = (level * 0.01).min(0.20);
    let stake_bonus = (stake * 0.0003).min(0.15);
    (0.30 + xp_bonus + stake_bonus).min(0.65)
}

#[derive(Debug, Clone)]
pub struct CaptureResult {
    pub success: bool,
    pub roll: f64,
    pub odds: f64,
    pub stake: i64,
    pub winnings: i64,
    pub net_gain: i64,
    pub xp_gained: i64,
    pub level_up: Option<i64>,
}

pub fn capture(
    conn: &Connection,
    guild: &str,
    user: &str,
    stake: i64,
    luck_bonus: f64,
) -> Result<CaptureResult> {
    if stake < 10 {
        user_err!("Minimum stake is 10 coins");
    }
    if stake > 1000 {
        user_err!("Maximum stake is 1000 coins");
    }
    let settings = guild_settings(conn, guild)?;
    let remaining = cooldown_remaining(
        conn,
        guild,
        user,
        "capture",
        settings.capture_cooldown_seconds,
    )?;
    if remaining > 0 {
        user_err!("Capture on cooldown. Wait {remaining} seconds.");
    }
    let b = ensure_user(conn, guild, user)?;
    if b.coins < stake as f64 {
        user_err!(
            "Insufficient coins: have {}, need {stake}",
            fmt_num(b.coins)
        );
    }
    let odds = (capture_odds(b.xp, stake as f64) + luck_bonus).min(0.75);
    let roll: f64 = rand::thread_rng().r#gen();
    let success = roll < odds;
    set_cooldown(conn, guild, user, "capture")?;

    let (winnings, xp_gain, change) = if success {
        let multiplier = (2.0 + (1.0 - odds)).min((1.0 / odds) * CAPTURE_HOUSE_EDGE);
        let winnings = (stake as f64 * multiplier) as i64;
        let xp_gain = (stake as f64 * 0.1) as i64;
        add_coins(
            conn,
            guild,
            user,
            (winnings - stake) as f64,
            "capture_win",
            None,
            None,
        )?;
        (
            winnings,
            xp_gain,
            add_xp(conn, guild, user, xp_gain as f64, "capture_win", None)?,
        )
    } else {
        let xp_gain = ((stake as f64 * 0.02) as i64).max(1);
        spend_coins(conn, guild, user, stake as f64, "capture_loss", None)?;
        (
            0,
            xp_gain,
            add_xp(
                conn,
                guild,
                user,
                xp_gain as f64,
                "capture_consolation",
                None,
            )?,
        )
    };
    Ok(CaptureResult {
        success,
        roll,
        odds,
        stake,
        winnings,
        net_gain: winnings - stake,
        xp_gained: xp_gain,
        level_up: change.level_up().then_some(change.new_level),
    })
}

pub fn duel_odds(xp1: f64, xp2: f64) -> (f64, f64) {
    let diff = level_from_xp(xp1) - level_from_xp(xp2);
    let shift = (diff.abs() as f64 * 0.02).min(0.20);
    match diff.signum() {
        1 => (0.5 + shift, 0.5 - shift),
        -1 => (0.5 - shift, 0.5 + shift),
        _ => (0.5, 0.5),
    }
}

#[derive(Debug, Clone)]
pub struct DuelResult {
    pub winner: String,
    pub loser: String,
    pub odds: (f64, f64),
    pub bet: i64,
    pub pot: i64,
    pub tax: i64,
    pub net_gain: i64,
    pub winner_xp: i64,
    pub loser_xp: i64,
}

pub fn duel(
    conn: &Connection,
    guild: &str,
    challenger: &str,
    opponent: &str,
    bet: i64,
) -> Result<DuelResult> {
    if challenger == opponent {
        user_err!("Cannot duel yourself");
    }
    if bet < 10 {
        user_err!("Minimum bet is 10 coins");
    }
    if bet > 500 {
        user_err!("Maximum bet is 500 coins");
    }
    let settings = guild_settings(conn, guild)?;
    let remaining = cooldown_remaining(
        conn,
        guild,
        challenger,
        "duel",
        settings.duel_cooldown_seconds,
    )?;
    if remaining > 0 {
        user_err!("Duel on cooldown for challenger. Wait {remaining} seconds.");
    }
    let (u1, u2) = (
        ensure_user(conn, guild, challenger)?,
        ensure_user(conn, guild, opponent)?,
    );
    if u1.coins < bet as f64 {
        user_err!(
            "Challenger has insufficient coins: have {}, need {bet}",
            fmt_num(u1.coins)
        );
    }
    if u2.coins < bet as f64 {
        user_err!(
            "Opponent has insufficient coins: have {}, need {bet}",
            fmt_num(u2.coins)
        );
    }
    let odds = duel_odds(u1.xp, u2.xp);
    let roll: f64 = rand::thread_rng().r#gen();
    let (winner, loser) = if roll < odds.0 {
        (challenger, opponent)
    } else {
        (opponent, challenger)
    };
    let pot = bet * 2;
    let tax = (pot as f64 * settings.duel_tax_percent / 100.0) as i64;
    let net_gain = pot - tax - bet;
    set_cooldown(conn, guild, challenger, "duel")?;
    spend_coins(conn, guild, loser, bet as f64, "duel_loss", None)?;
    let meta = serde_json::json!({ "tax": tax, "opponent": loser });
    add_coins(
        conn,
        guild,
        winner,
        net_gain as f64,
        "duel_win",
        None,
        Some(&meta),
    )?;
    let winner_xp = (bet as f64 * 0.1) as i64;
    let loser_xp = ((bet as f64 * 0.02) as i64).max(1);
    add_xp(conn, guild, winner, winner_xp as f64, "duel_win", None)?;
    add_xp(
        conn,
        guild,
        loser,
        loser_xp as f64,
        "duel_consolation",
        None,
    )?;
    Ok(DuelResult {
        winner: winner.into(),
        loser: loser.into(),
        odds,
        bet,
        pot,
        tax,
        net_gain,
        winner_xp,
        loser_xp,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct GameStats {
    pub total: i64,
    pub wins: i64,
    pub win_rate: f64,
    pub net_profit: f64,
}

pub fn game_stats(
    conn: &Connection,
    guild: &str,
    user: &str,
    win: &str,
    loss: &str,
) -> Result<GameStats> {
    let (wins, losses, won, lost): (i64, i64, f64, f64) = conn.query_row(
        "SELECT COALESCE(SUM(CASE WHEN kind = ?1 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN kind = ?2 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN kind = ?1 THEN amount ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN kind = ?2 THEN ABS(amount) ELSE 0 END), 0)
         FROM transactions WHERE guildId = ?3 AND userId = ?4 AND kind IN (?1, ?2) AND currency = 'coins'",
        params![win, loss, guild, user],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let total = wins + losses;
    let win_rate = if total > 0 {
        (wins as f64 / total as f64 * 1000.0).round() / 10.0
    } else {
        0.0
    };
    Ok(GameStats {
        total,
        wins,
        win_rate,
        net_profit: won - lost,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(include_str!("db/schema.sql")).unwrap();
        c
    }

    #[test]
    fn balances_and_ledger() {
        let c = conn();
        add_coins(&c, "g", "u", 100.0, "test", None, None).unwrap();
        assert!(
            spend_coins(&c, "g", "u", 150.0, "x", None)
                .unwrap_err()
                .is::<UserError>()
        );
        assert_eq!(spend_coins(&c, "g", "u", 40.0, "x", None).unwrap(), 60.0);
        let ch = add_xp(&c, "g", "u", 130.0, "x", None).unwrap();
        assert!(ch.level_up());
        let ch = spend_xp(&c, "g", "u", 100.0, "x", None).unwrap();
        assert!(ch.level_down());
        assert_eq!(user_transactions(&c, "g", "u", 10, None).unwrap().len(), 4);
        assert_eq!(
            user_transactions(&c, "g", "u", 10, Some("te"))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn shop_purchase_and_use() {
        let mut c = conn();
        crate::db::seed_for_tests(&mut c);
        add_coins(&c, "g", "u", 1000.0, "test", None, None).unwrap();
        let p = buy_item(&c, "g", "u", 3, 2).unwrap(); // Capture Luck Charm
        assert_eq!((p.coins_spent, p.quantity), (300, 2));
        assert_eq!(inventory(&c, "g", "u").unwrap()[0].quantity, 2);
        let used = use_item(&c, "g", "u", 3).unwrap();
        assert_eq!(used.effect.as_deref(), Some("capture_luck"));
        let luck = active_effect(&c, "g", "u", "capture_luck")
            .unwrap()
            .unwrap();
        assert_eq!(luck["bonus"].as_f64(), Some(0.10));
        assert!(buy_item(&c, "g", "u", 999, 1).is_err());
    }

    #[test]
    fn trade_lifecycle() {
        let mut c = conn();
        add_coins(&c, "g", "a", 100.0, "test", None, None).unwrap();
        let t = create_trade(&c, "g", "a", "b", 50, 0).unwrap();
        assert_eq!(t.tax_coins, 5);
        assert!(create_trade(&c, "g", "a", "b", 10, 0).is_err()); // déjà en attente
        assert!(accept_trade(&c, "g", "a", t.id).unwrap().is_err()); // pas le destinataire
        let accepted = accept_trade(&c, "g", "b", t.id).unwrap().unwrap();
        assert_eq!(accepted.status, "accepted");
        assert_eq!(ensure_user(&c, "g", "a").unwrap().coins, 50.0);
        // Séquestre forcé à expiration.
        c.execute(
            "UPDATE trades SET escrow_release_at = '2000-01-01T00:00:00'",
            [],
        )
        .unwrap();
        assert_eq!(complete_ready_trades(&mut c).unwrap(), vec![t.id]);
        assert_eq!(ensure_user(&c, "g", "b").unwrap().coins, 45.0);
    }

    #[test]
    fn trade_cancel_refunds_during_escrow() {
        let c = conn();
        add_coins(&c, "g", "a", 100.0, "test", None, None).unwrap();
        let t = create_trade(&c, "g", "a", "b", 30, 0).unwrap();
        accept_trade(&c, "g", "b", t.id).unwrap().unwrap();
        assert!(cancel_trade(&c, "g", "b", t.id).is_err()); // seul l'expéditeur
        assert!(cancel_trade(&c, "g", "a", t.id).unwrap());
        assert_eq!(ensure_user(&c, "g", "a").unwrap().coins, 100.0);
    }

    #[test]
    fn games() {
        let c = conn();
        assert!((capture_odds(0.0, 0.0) - 0.31).abs() < 1e-9);
        assert_eq!(capture_odds(1e9, 10_000.0), 0.65);
        assert_eq!(duel_odds(0.0, 0.0), (0.5, 0.5));
        let (a, b) = duel_odds(500.0, 0.0); // niveaux 3 et 1
        assert!((a - 0.54).abs() < 1e-9 && (b - 0.46).abs() < 1e-9);

        add_coins(&c, "g", "u", 100.0, "test", None, None).unwrap();
        assert!(capture(&c, "g", "u", 5, 0.0).is_err());
        let r = capture(&c, "g", "u", 50, 0.0).unwrap();
        assert!(r.xp_gained >= 1);
        // Délai de 60 s actif.
        assert!(
            capture(&c, "g", "u", 10, 0.0)
                .unwrap_err()
                .to_string()
                .contains("cooldown")
        );

        add_coins(&c, "g", "v", 100.0, "test", None, None).unwrap();
        add_coins(&c, "g", "u", 100.0, "test", None, None).unwrap();
        let d = duel(&c, "g", "u", "v", 20).unwrap();
        assert_eq!((d.pot, d.tax, d.net_gain), (40, 4, 16));
        let s = game_stats(&c, "g", &d.winner, "duel_win", "duel_loss").unwrap();
        assert_eq!((s.total, s.wins), (1, 1));
    }
}

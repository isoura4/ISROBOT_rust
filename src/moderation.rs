//! Logique partagée du système de modération (portage de `utils/moderation_utils.py`).
//!
//! Les dates sont stockées au format ISO 8601 avec fuseau (`...+00:00`),
//! exactement comme la version Python.

use chrono::{DateTime, Duration, NaiveDateTime, SecondsFormat, Utc};
use poise::serenity_prelude as serenity;
use rusqlite::{Connection, OptionalExtension, params};

use crate::bot::{Data, colors, escape_user_content};
use crate::tr;

// ---------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------

/// Équivalent de `datetime.now(timezone.utc).isoformat()`.
pub fn iso(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Micros, false)
}

pub fn now_iso() -> String {
    iso(Utc::now())
}

/// Lit une date ISO avec ou sans fuseau (sans fuseau = UTC).
pub fn parse_iso(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some(d.with_timezone(&Utc));
    }
    [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
    ]
    .iter()
    .find_map(|f| NaiveDateTime::parse_from_str(s, f).ok())
    .map(|n| n.and_utc())
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ModConfig {
    pub log_channel_id: Option<String>,
    pub appeal_channel_id: Option<String>,
    pub ai_enabled: bool,
    pub ai_confidence_threshold: i64,
    pub ai_flag_channel_id: Option<String>,
    pub ai_model: String,
    pub ollama_host: String,
    pub warn_1_decay_days: i64,
    pub warn_2_decay_days: i64,
    pub warn_3_decay_days: i64,
    pub mute_duration_warn_2: i64,
    pub mute_duration_warn_3: i64,
    pub rules_message_id: Option<String>,
}

fn non_empty(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.trim().is_empty())
}

pub fn get_config(conn: &Connection, guild: &str) -> rusqlite::Result<Option<ModConfig>> {
    conn.query_row(
        "SELECT log_channel_id, appeal_channel_id, ai_enabled, ai_confidence_threshold,
                ai_flag_channel_id, ai_model, ollama_host, warn_1_decay_days, warn_2_decay_days,
                warn_3_decay_days, mute_duration_warn_2, mute_duration_warn_3, rules_message_id
         FROM moderation_config WHERE guild_id = ?1",
        [guild],
        |r| {
            Ok(ModConfig {
                log_channel_id: non_empty(r.get(0)?),
                appeal_channel_id: non_empty(r.get(1)?),
                ai_enabled: r.get::<_, Option<i64>>(2)?.unwrap_or(1) == 1,
                ai_confidence_threshold: r.get::<_, Option<i64>>(3)?.unwrap_or(60),
                ai_flag_channel_id: non_empty(r.get(4)?),
                ai_model: r
                    .get::<_, Option<String>>(5)?
                    .unwrap_or_else(|| "llama2".into()),
                ollama_host: r
                    .get::<_, Option<String>>(6)?
                    .unwrap_or_else(|| "http://localhost:11434".into()),
                warn_1_decay_days: r.get::<_, Option<i64>>(7)?.unwrap_or(7),
                warn_2_decay_days: r.get::<_, Option<i64>>(8)?.unwrap_or(14),
                warn_3_decay_days: r.get::<_, Option<i64>>(9)?.unwrap_or(21),
                mute_duration_warn_2: r.get::<_, Option<i64>>(10)?.unwrap_or(3600),
                mute_duration_warn_3: r.get::<_, Option<i64>>(11)?.unwrap_or(86400),
                rules_message_id: non_empty(r.get(12)?),
            })
        },
    )
    .optional()
}

/// Colonnes modifiables via `/modconfig set`.
pub const CONFIG_COLUMNS: &[&str] = &[
    "log_channel_id",
    "appeal_channel_id",
    "ai_enabled",
    "ai_confidence_threshold",
    "ai_flag_channel_id",
    "ai_model",
    "ollama_host",
    "decay_multiplier",
    "warn_1_decay_days",
    "warn_2_decay_days",
    "warn_3_decay_days",
    "mute_duration_warn_2",
    "mute_duration_warn_3",
    "rules_message_id",
];

pub fn set_config(conn: &Connection, guild: &str, column: &str, value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        CONFIG_COLUMNS.contains(&column),
        "Invalid parameter: {column}"
    );
    conn.execute(
        "INSERT OR IGNORE INTO moderation_config (guild_id, created_at) VALUES (?1, ?2)",
        params![guild, now_iso()],
    )?;
    // `column` provient d'une liste blanche.
    conn.execute(
        &format!("UPDATE moderation_config SET {column} = ?1 WHERE guild_id = ?2"),
        params![value, guild],
    )?;
    Ok(())
}

pub fn decay_days(warn_count: i64, config: Option<&ModConfig>) -> i64 {
    match (warn_count, config) {
        (1, Some(c)) => c.warn_1_decay_days,
        (2, Some(c)) => c.warn_2_decay_days,
        (3, Some(c)) => c.warn_3_decay_days,
        (1, None) => 7,
        (2, None) => 14,
        (3, None) => 21,
        _ => 28,
    }
}

// ---------------------------------------------------------------------------
// Avertissements
// ---------------------------------------------------------------------------

fn log_history(
    conn: &Connection,
    guild: &str,
    user: &str,
    action: &str,
    before: i64,
    after: i64,
    moderator: Option<&str>,
    reason: Option<&str>,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO warning_history
         (guild_id, user_id, action, warn_count_before, warn_count_after, moderator_id, reason, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![guild, user, action, before, after, moderator, reason, now_iso()],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn warning_count(conn: &Connection, guild: &str, user: &str) -> rusqlite::Result<i64> {
    Ok(conn
        .query_row(
            "SELECT warn_count FROM warnings WHERE guild_id = ?1 AND user_id = ?2",
            params![guild, user],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0))
}

/// Ajoute un avertissement et renvoie le nouveau total.
///
/// `updated_at` suit le plus ancien avertissement actif : chaque avertissement
/// expire selon sa propre date d'émission.
pub fn increment_warning(
    conn: &mut Connection,
    guild: &str,
    user: &str,
    moderator: &str,
    reason: &str,
) -> rusqlite::Result<i64> {
    let tx = conn.transaction()?;
    let now = now_iso();
    let current: Option<(i64, String)> = tx
        .query_row(
            "SELECT warn_count, updated_at FROM warnings WHERE guild_id = ?1 AND user_id = ?2",
            params![guild, user],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let old = current.as_ref().map(|c| c.0).unwrap_or(0);
    let new = old + 1;
    let oldest = match current {
        Some((n, at)) if n > 0 => at,
        _ => now.clone(),
    };
    tx.execute(
        "INSERT INTO warnings (guild_id, user_id, warn_count, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(guild_id, user_id) DO UPDATE SET warn_count = ?3, updated_at = ?5",
        params![guild, user, new, now, oldest],
    )?;
    log_history(
        &tx,
        guild,
        user,
        "warn_issued",
        old,
        new,
        Some(moderator),
        Some(reason),
    )?;
    tx.commit()?;
    Ok(new)
}

/// Date d'émission du plus ancien avertissement encore actif.
fn oldest_active_warning(
    conn: &Connection,
    guild: &str,
    user: &str,
    count: i64,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT created_at FROM warning_history
         WHERE guild_id = ?1 AND user_id = ?2 AND action = 'warn_issued'
         ORDER BY created_at DESC LIMIT 1 OFFSET ?3",
        params![guild, user, (count - 1).max(0)],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()
    .map(Option::flatten)
}

/// Retire un avertissement et renvoie le nouveau total.
pub fn decrement_warning(
    conn: &mut Connection,
    guild: &str,
    user: &str,
    moderator: Option<&str>,
    reason: Option<&str>,
) -> rusqlite::Result<i64> {
    let tx = conn.transaction()?;
    let old = warning_count(&tx, guild, user)?;
    if old <= 0 {
        return Ok(0);
    }
    let new = old - 1;
    let now = now_iso();
    let oldest = if new == 0 {
        now
    } else {
        oldest_active_warning(&tx, guild, user, new)?
            .filter(|s| parse_iso(s).is_some())
            .unwrap_or(now)
    };
    tx.execute(
        "UPDATE warnings SET warn_count = ?1, updated_at = ?2 WHERE guild_id = ?3 AND user_id = ?4",
        params![new, oldest, guild, user],
    )?;
    log_history(
        &tx,
        guild,
        user,
        "warn_decreased",
        old,
        new,
        moderator,
        reason,
    )?;
    tx.commit()?;
    Ok(new)
}

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub user_id: String,
    pub action: String,
    pub reason: Option<String>,
    pub created_at: String,
}

pub fn history(
    conn: &Connection,
    guild: &str,
    user: Option<&str>,
    limit: i64,
) -> rusqlite::Result<Vec<HistoryEntry>> {
    let map = |r: &rusqlite::Row<'_>| {
        Ok(HistoryEntry {
            user_id: r.get(0)?,
            action: r.get(1)?,
            reason: r.get(2)?,
            created_at: r.get(3)?,
        })
    };
    match user {
        Some(u) => conn
            .prepare(
                "SELECT user_id, action, reason, created_at FROM warning_history
                 WHERE guild_id = ?1 AND user_id = ?2 ORDER BY created_at DESC LIMIT ?3",
            )?
            .query_map(params![guild, u, limit], map)?
            .collect(),
        None => conn
            .prepare(
                "SELECT user_id, action, reason, created_at FROM warning_history
                 WHERE guild_id = ?1 ORDER BY created_at DESC LIMIT ?2",
            )?
            .query_map(params![guild, limit], map)?
            .collect(),
    }
}

/// Utilisateurs dont le plus ancien avertissement a dépassé son délai d'expiration.
pub fn users_for_decay(conn: &Connection) -> rusqlite::Result<Vec<(String, String, i64)>> {
    let rows: Vec<(String, String, i64, String)> = conn
        .prepare(
            "SELECT guild_id, user_id, warn_count, updated_at FROM warnings WHERE warn_count > 0",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let now = Utc::now();
    let mut out = Vec::new();
    for (g, u, count, updated) in rows {
        let Some(updated) = parse_iso(&updated) else {
            continue;
        };
        let config = get_config(conn, &g)?;
        if now >= updated + Duration::days(decay_days(count, config.as_ref())) {
            out.push((g, u, count));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Mutes
// ---------------------------------------------------------------------------

pub fn add_mute(
    conn: &Connection,
    guild: &str,
    user: &str,
    moderator: Option<&str>,
    reason: &str,
    seconds: i64,
    is_auto: bool,
) -> rusqlite::Result<()> {
    let now = Utc::now();
    conn.execute(
        "INSERT INTO active_mutes (guild_id, user_id, moderator_id, reason, expires_at, created_at, is_auto)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(guild_id, user_id) DO UPDATE SET
            moderator_id = ?3, reason = ?4, expires_at = ?5, created_at = ?6, is_auto = ?7",
        params![guild, user, moderator, reason, iso(now + Duration::seconds(seconds)), iso(now), is_auto as i64],
    )?;
    log_history(
        conn,
        guild,
        user,
        "mute_applied",
        0,
        0,
        moderator,
        Some(reason),
    )?;
    Ok(())
}

pub fn remove_mute(
    conn: &Connection,
    guild: &str,
    user: &str,
    moderator: Option<&str>,
    reason: &str,
) -> rusqlite::Result<bool> {
    let n = conn.execute(
        "DELETE FROM active_mutes WHERE guild_id = ?1 AND user_id = ?2",
        params![guild, user],
    )?;
    if n == 0 {
        return Ok(false);
    }
    log_history(
        conn,
        guild,
        user,
        "mute_removed",
        0,
        0,
        moderator,
        Some(reason),
    )?;
    Ok(true)
}

/// `Some(is_auto)` si l'utilisateur a un mute actif.
pub fn active_mute(conn: &Connection, guild: &str, user: &str) -> rusqlite::Result<Option<bool>> {
    conn.query_row(
        "SELECT is_auto FROM active_mutes WHERE guild_id = ?1 AND user_id = ?2",
        params![guild, user],
        |r| Ok(r.get::<_, Option<i64>>(0)?.unwrap_or(0) == 1),
    )
    .optional()
}

pub fn expired_mutes(conn: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
    let now = Utc::now();
    let rows: Vec<(String, String, String)> = conn
        .prepare("SELECT guild_id, user_id, expires_at FROM active_mutes")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows
        .into_iter()
        .filter(|(_, _, exp)| parse_iso(exp).is_none_or(|e| e <= now))
        .map(|(g, u, _)| (g, u))
        .collect())
}

// ---------------------------------------------------------------------------
// Appels
// ---------------------------------------------------------------------------

/// Crée un appel ; `None` si aucun avertissement ou un appel déjà en attente.
pub fn create_appeal(
    conn: &Connection,
    guild: &str,
    user: &str,
    reason: &str,
) -> rusqlite::Result<Option<i64>> {
    if warning_count(conn, guild, user)? <= 0 {
        return Ok(None);
    }
    let pending = conn
        .prepare("SELECT 1 FROM moderation_appeals WHERE guild_id = ?1 AND user_id = ?2 AND status = 'pending'")?
        .exists(params![guild, user])?;
    if pending {
        return Ok(None);
    }
    conn.execute(
        "INSERT INTO moderation_appeals (guild_id, user_id, appeal_reason, status, created_at)
         VALUES (?1, ?2, ?3, 'pending', ?4)",
        params![guild, user, reason, now_iso()],
    )?;
    let id = conn.last_insert_rowid();
    log_history(
        conn,
        guild,
        user,
        "appeal_created",
        0,
        0,
        None,
        Some(reason),
    )?;
    Ok(Some(id))
}

/// Temps restant avant de pouvoir refaire appel (délai de 48 h).
pub fn appeal_cooldown(
    conn: &Connection,
    guild: &str,
    user: &str,
) -> rusqlite::Result<Option<Duration>> {
    let last: Option<String> = conn
        .query_row(
            "SELECT created_at FROM moderation_appeals WHERE guild_id = ?1 AND user_id = ?2
             ORDER BY created_at DESC LIMIT 1",
            params![guild, user],
            |r| r.get(0),
        )
        .optional()?;
    let Some(last) = last.as_deref().and_then(parse_iso) else {
        return Ok(None);
    };
    let elapsed = Utc::now() - last;
    let cooldown = Duration::hours(48);
    Ok((elapsed < cooldown).then(|| cooldown - elapsed))
}

/// Enregistre la décision ; renvoie `(guild_id, user_id)` de l'appel.
pub fn review_appeal(
    conn: &Connection,
    appeal_id: i64,
    moderator: &str,
    decision: &str,
    moderator_decision: &str,
) -> rusqlite::Result<Option<(String, String)>> {
    let found: Option<(String, String)> = conn
        .query_row(
            "SELECT guild_id, user_id FROM moderation_appeals WHERE id = ?1",
            [appeal_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((g, u)) = found else { return Ok(None) };
    conn.execute(
        "UPDATE moderation_appeals SET status = ?1, moderator_id = ?2, moderator_decision = ?3, reviewed_at = ?4
         WHERE id = ?5",
        params![decision, moderator, moderator_decision, now_iso(), appeal_id],
    )?;
    log_history(
        conn,
        &g,
        &u,
        "appeal_reviewed",
        0,
        0,
        Some(moderator),
        Some(moderator_decision),
    )?;
    Ok(Some((g, u)))
}

// ---------------------------------------------------------------------------
// Durées
// ---------------------------------------------------------------------------

pub fn format_duration(seconds: i64, loc: Option<&str>) -> String {
    let (n, one, many) = if seconds < 60 {
        (seconds, "mod.duration.sec", "mod.duration.secs")
    } else if seconds < 3600 {
        (seconds / 60, "mod.duration.min", "mod.duration.mins")
    } else if seconds < 86400 {
        (seconds / 3600, "mod.duration.hour", "mod.duration.hours")
    } else {
        (seconds / 86400, "mod.duration.day", "mod.duration.days")
    };
    tr!(if n == 1 { one } else { many }, loc, n = n)
}

/// « 1h », « 30m », « 1d », « 2h30m »... → secondes.
pub fn parse_duration(s: &str) -> Option<i64> {
    let re = regex::Regex::new(r"(\d+)([dhms])").unwrap();
    let s = s.trim().to_lowercase();
    let mut total = 0i64;
    let mut seen = [false; 4];
    for cap in re.captures_iter(&s) {
        let n: i64 = cap[1].parse().ok()?;
        let (idx, mult) = match &cap[2] {
            "d" => (0, 86400),
            "h" => (1, 3600),
            "m" => (2, 60),
            _ => (3, 1),
        };
        // Comme en Python : seule la première occurrence de chaque unité compte.
        if !seen[idx] {
            seen[idx] = true;
            total = total.checked_add(n.checked_mul(mult)?)?;
        }
    }
    (total > 0).then_some(total)
}

// ---------------------------------------------------------------------------
// Embeds
// ---------------------------------------------------------------------------

fn base_embed(title: String, color: u32) -> serenity::CreateEmbed {
    serenity::CreateEmbed::new()
        .title(title)
        .colour(color)
        .timestamp(serenity::Timestamp::now())
}

fn footer(loc: Option<&str>) -> serenity::CreateEmbedFooter {
    serenity::CreateEmbedFooter::new(tr!("mod.footer", loc))
}

pub fn warning_embed(
    reason: &str,
    count: i64,
    guild_name: &str,
    rules_link: Option<&str>,
    loc: Option<&str>,
) -> serenity::CreateEmbed {
    let mut e = base_embed(tr!("mod.warn.title", loc), colors::ORANGE)
        .description(tr!("mod.warn.desc", loc, guild = guild_name))
        .field(
            tr!("mod.field.reason", loc),
            escape_user_content(reason),
            false,
        )
        .field(tr!("mod.field.warn_count", loc), count.to_string(), true);
    if let Some(link) = rules_link {
        e = e.field(
            tr!("mod.field.rules", loc),
            tr!("mod.field.rules_link", loc, link = link),
            false,
        );
    }
    e.field(
        tr!("mod.field.appeal", loc),
        tr!("mod.field.appeal_value", loc),
        false,
    )
    .footer(footer(loc))
}

pub fn mute_embed(
    reason: &str,
    seconds: i64,
    guild_name: &str,
    loc: Option<&str>,
) -> serenity::CreateEmbed {
    let expires = (Utc::now() + Duration::seconds(seconds)).timestamp();
    base_embed(tr!("mod.mute.title", loc), colors::RED)
        .description(tr!("mod.mute.desc", loc, guild = guild_name))
        .field(
            tr!("mod.field.reason", loc),
            escape_user_content(reason),
            false,
        )
        .field(
            tr!("mod.field.duration", loc),
            format_duration(seconds, loc),
            true,
        )
        .field(
            tr!("mod.field.expires", loc),
            format!("<t:{expires}:R>"),
            true,
        )
        .footer(footer(loc))
}

pub fn decay_embed(new_count: i64, guild_name: &str, loc: Option<&str>) -> serenity::CreateEmbed {
    base_embed(tr!("mod.decay.title", loc), colors::GREEN)
        .description(tr!("mod.decay.desc", loc, guild = guild_name))
        .field(tr!("mod.decay.field_new", loc), new_count.to_string(), true)
        .field(
            tr!("mod.decay.field_keep", loc),
            tr!("mod.decay.keep_value", loc),
            false,
        )
        .footer(footer(loc))
}

pub fn mute_expired_embed(guild_name: &str) -> serenity::CreateEmbed {
    serenity::CreateEmbed::new()
        .title(tr!("mod.dm.expired_title", None))
        .description(tr!("mod.dm.expired_desc", None, guild = guild_name))
        .colour(colors::GREEN)
        .field(
            tr!("mod.dm.expired_field", None),
            tr!("mod.dm.expired_value", None),
            false,
        )
        .footer(footer(None))
}

/// Embed du salon de logs de modération.
pub fn modlog_embed(
    action: &str,
    user: serenity::UserId,
    moderator: Option<serenity::UserId>,
    fields: &[(&str, String)],
    loc: Option<&str>,
) -> serenity::CreateEmbed {
    let color = match action {
        "warn" => colors::ORANGE,
        "mute" | "appeal_denied" => colors::RED,
        "unmute" | "appeal_approved" => colors::GREEN,
        "decay" => colors::LIGHT_GRAY,
        "appeal_created" => colors::PURPLE,
        _ => colors::BLUE,
    };
    let title_key = format!("mod.log.{action}");
    let mut title = tr!(&title_key, loc);
    if title == title_key {
        title = tr!("mod.log.default", loc);
    }
    let mut e =
        base_embed(title, color).field(tr!("mod.log.field_user", loc), format!("<@{user}>"), true);
    e = match moderator {
        Some(m) => e.field(tr!("mod.log.field_moderator", loc), format!("<@{m}>"), true),
        None => e.field(
            tr!("mod.log.field_action", loc),
            tr!("mod.log.auto", loc),
            true,
        ),
    };
    for (key, value) in fields {
        let tkey = format!("mod.log.field_{key}");
        let mut name = tr!(&tkey, loc);
        if name == tkey {
            name = title_case(&key.replace('_', " "));
        }
        e = e.field(name, value.clone(), false);
    }
    e.footer(footer(loc))
}

/// « warn_issued » → « Warn Issued » (équivalent de `str.title()`).
pub fn title_case(s: &str) -> String {
    s.replace('_', " ")
        .split(' ')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn ts(created_at: &str) -> i64 {
    parse_iso(created_at).map(|d| d.timestamp()).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Actions Discord
// ---------------------------------------------------------------------------

/// Applique un timeout Discord et l'enregistre en base.
pub async fn apply_mute(
    http: &serenity::Http,
    data: &Data,
    guild: serenity::GuildId,
    user: serenity::UserId,
    moderator: Option<String>,
    reason: String,
    seconds: i64,
    is_auto: bool,
) -> anyhow::Result<()> {
    let until = serenity::Timestamp::from_unix_timestamp(Utc::now().timestamp() + seconds)?;
    guild
        .edit_member(
            http,
            user,
            serenity::EditMember::new()
                .disable_communication_until(until.to_string())
                .audit_log_reason(&reason),
        )
        .await?;
    let (g, u) = (guild.to_string(), user.to_string());
    data.db
        .run(move |c| {
            Ok(add_mute(
                c,
                &g,
                &u,
                moderator.as_deref(),
                &reason,
                seconds,
                is_auto,
            )?)
        })
        .await
}

/// Retire le timeout Discord et le mute en base.
pub async fn lift_mute(
    http: &serenity::Http,
    data: &Data,
    guild: serenity::GuildId,
    user: serenity::UserId,
    moderator: Option<String>,
    reason: String,
) -> anyhow::Result<()> {
    guild
        .edit_member(
            http,
            user,
            serenity::EditMember::new()
                .enable_communication()
                .audit_log_reason(&reason),
        )
        .await?;
    let (g, u) = (guild.to_string(), user.to_string());
    data.db
        .run(move |c| Ok(remove_mute(c, &g, &u, moderator.as_deref(), &reason)?))
        .await?;
    Ok(())
}

/// Envoie un embed dans le salon de logs configuré (si présent).
pub async fn post_modlog(
    http: &serenity::Http,
    config: Option<&ModConfig>,
    embed: serenity::CreateEmbed,
) {
    let Some(id) = config
        .and_then(|c| c.log_channel_id.as_deref())
        .and_then(|s| s.parse::<u64>().ok())
    else {
        return;
    };
    if let Err(e) = serenity::ChannelId::new(id)
        .send_message(http, serenity::CreateMessage::new().embed(embed))
        .await
    {
        tracing::error!("Erreur lors de l'envoi au salon de logs: {e}");
    }
}

pub async fn load_config(data: &Data, guild: serenity::GuildId) -> Option<ModConfig> {
    let g = guild.to_string();
    data.db
        .run(move |c| Ok(get_config(c, &g)?))
        .await
        .unwrap_or_else(|e| {
            tracing::error!("Lecture de la configuration de modération: {e:#}");
            None
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
    fn durations() {
        assert_eq!(parse_duration("1h"), Some(3600));
        assert_eq!(parse_duration("2h30m"), Some(9000));
        assert_eq!(parse_duration(" 1D "), Some(86400));
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration("0m"), None);
        assert_eq!(format_duration(1, Some("fr")), "1 seconde");
        assert_eq!(format_duration(7200, Some("fr")), "2 heures");
        assert_eq!(format_duration(86400, Some("en")), "1 day");
    }

    #[test]
    fn warnings_cycle() {
        let mut c = conn();
        assert_eq!(increment_warning(&mut c, "g", "u", "m", "spam").unwrap(), 1);
        assert_eq!(increment_warning(&mut c, "g", "u", "m", "spam").unwrap(), 2);
        assert_eq!(warning_count(&c, "g", "u").unwrap(), 2);
        assert_eq!(
            decrement_warning(&mut c, "g", "u", None, Some("x")).unwrap(),
            1
        );
        assert_eq!(decrement_warning(&mut c, "g", "u", None, None).unwrap(), 0);
        assert_eq!(decrement_warning(&mut c, "g", "u", None, None).unwrap(), 0);
        assert_eq!(history(&c, "g", Some("u"), 100).unwrap().len(), 4);
        // Rien à faire expirer juste après l'émission.
        increment_warning(&mut c, "g", "u", "m", "spam").unwrap();
        assert!(users_for_decay(&c).unwrap().is_empty());
        c.execute(
            "UPDATE warnings SET updated_at = '2000-01-01T00:00:00+00:00'",
            [],
        )
        .unwrap();
        assert_eq!(
            users_for_decay(&c).unwrap(),
            vec![("g".into(), "u".into(), 1)]
        );
    }

    #[test]
    fn mutes_and_appeals() {
        let c = conn();
        add_mute(&c, "g", "u", Some("m"), "r", -10, true).unwrap();
        assert_eq!(active_mute(&c, "g", "u").unwrap(), Some(true));
        assert_eq!(expired_mutes(&c).unwrap().len(), 1);
        assert!(remove_mute(&c, "g", "u", None, "x").unwrap());
        assert!(!remove_mute(&c, "g", "u", None, "x").unwrap());

        assert_eq!(create_appeal(&c, "g", "u", "pardon").unwrap(), None);
        c.execute(
            "INSERT INTO warnings (guild_id, user_id, warn_count, created_at, updated_at) VALUES ('g','u',1,'','')",
            [],
        )
        .unwrap();
        let id = create_appeal(&c, "g", "u", "pardon").unwrap().unwrap();
        assert_eq!(create_appeal(&c, "g", "u", "encore").unwrap(), None);
        assert!(appeal_cooldown(&c, "g", "u").unwrap().is_some());
        assert_eq!(
            review_appeal(&c, id, "m", "approved", "ok").unwrap(),
            Some(("g".into(), "u".into()))
        );
    }

    #[test]
    fn iso_roundtrip_and_python_formats() {
        let now = Utc::now();
        assert_eq!(
            parse_iso(&iso(now)).unwrap().timestamp_micros(),
            now.timestamp_micros()
        );
        assert!(parse_iso("2024-05-01T10:00:00.123456").is_some());
        assert!(parse_iso("2024-05-01 10:00:00").is_some());
        assert_eq!(title_case("warn_issued"), "Warn Issued");
    }
}

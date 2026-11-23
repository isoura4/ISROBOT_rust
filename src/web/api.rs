//! API JSON de l'interface web (toutes les routes exigent un administrateur).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use poise::serenity_prelude as serenity;
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{Admin, WebState};
use crate::{commands, config, economy, features, moderation as m};

pub struct ApiError(StatusCode, String);

impl ApiError {
    pub fn unauthorized() -> Self {
        Self(StatusCode::UNAUTHORIZED, "Connexion requise".into())
    }
    pub fn forbidden(msg: &str) -> Self {
        Self(StatusCode::FORBIDDEN, msg.into())
    }
    fn bad(msg: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, msg.into())
    }
    fn not_found() -> Self {
        Self(StatusCode::NOT_FOUND, "Introuvable".into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!("[Web] Erreur interne: {e:#}");
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Erreur interne du bot".into(),
        )
    }
}

type ApiResult = Result<Json<Value>, ApiError>;
type St = State<Arc<WebState>>;

pub fn routes() -> Router<Arc<WebState>> {
    Router::new()
        .route("/me", get(me))
        .route("/guild", get(guild_info))
        .route("/features", get(features_list))
        .route("/features/{key}", post(feature_toggle))
        .route("/moderation/config", get(mod_config).put(mod_config_update))
        .route("/moderation/overview", get(mod_overview))
        .route("/moderation/unwarn", post(mod_unwarn))
        .route("/moderation/unmute", post(mod_unmute))
        .route("/moderation/flags/{id}", post(mod_flag_review))
        .route("/minigame", get(minigame_settings).put(minigame_update))
        .route("/counter", get(counter_list).post(counter_add))
        .route("/counter/{channel}", delete(counter_remove))
        .route("/notifications", get(notifications))
        .route("/notifications/{kind}", post(notification_add))
        .route("/notifications/{kind}/{id}", delete(notification_remove))
        .route("/leaderboard", get(leaderboard))
}

fn guild_str(st: &WebState) -> String {
    st.data.config.server_id.to_string()
}

fn audit(admin: &Admin, action: &str) {
    tracing::info!(
        "[Web] {} ({}) : {action}",
        admin.session.username,
        admin.session.user_id
    );
}

// ---------------------------------------------------------------------------
// Session et serveur
// ---------------------------------------------------------------------------

async fn me(State(st): St, admin: Admin) -> ApiResult {
    let bot = st.ctx.cache.current_user().clone();
    let s = &admin.session;
    Ok(Json(json!({
        "user": {
            "id": s.user_id.to_string(),
            "name": s.username,
            "avatar": s.avatar.as_ref().map(|a| format!("https://cdn.discordapp.com/avatars/{}/{a}.png?size=64", s.user_id)),
        },
        "csrf": s.csrf,
        "bot": {
            "name": bot.name,
            "avatar": bot.avatar_url(),
            "uptime_seconds": st.data.started.elapsed().as_secs(),
            "platform": format!("{:?}", st.data.config.platform),
            "version": env!("CARGO_PKG_VERSION"),
        },
    })))
}

/// Salons textuels et rôles du serveur (pour les listes déroulantes).
async fn guild_info(State(st): St, _a: Admin) -> ApiResult {
    let Some(g) = st.ctx.cache.guild(st.guild()) else {
        return Ok(Json(json!({ "name": null, "channels": [], "roles": [] })));
    };
    let mut channels: Vec<_> = g
        .channels
        .values()
        .filter(|c| {
            matches!(
                c.kind,
                serenity::ChannelType::Text | serenity::ChannelType::News
            )
        })
        .map(|c| (c.position, c.id.to_string(), c.name.clone()))
        .collect();
    channels.sort();
    let mut roles: Vec<_> = g
        .roles
        .values()
        .filter(|r| r.id.get() != g.id.get() && !r.managed)
        .map(|r| {
            (
                std::cmp::Reverse(r.position),
                r.id.to_string(),
                r.name.clone(),
            )
        })
        .collect();
    roles.sort();
    Ok(Json(json!({
        "name": g.name,
        "icon": g.icon_url(),
        "member_count": g.member_count,
        "channels": channels.into_iter().map(|(_, id, name)| json!({"id": id, "name": name})).collect::<Vec<_>>(),
        "roles": roles.into_iter().map(|(_, id, name)| json!({"id": id, "name": name})).collect::<Vec<_>>(),
    })))
}

/// Salons textuels valides du serveur.
fn text_channels(st: &WebState) -> Vec<u64> {
    st.ctx
        .cache
        .guild(st.guild())
        .map(|g| {
            g.channels
                .values()
                .filter(|c| {
                    matches!(
                        c.kind,
                        serenity::ChannelType::Text | serenity::ChannelType::News
                    )
                })
                .map(|c| c.id.get())
                .collect()
        })
        .unwrap_or_default()
}

fn role_exists(st: &WebState, id: u64) -> bool {
    st.ctx
        .cache
        .guild(st.guild())
        .is_some_and(|g| g.roles.contains_key(&serenity::RoleId::new(id)))
}

fn parse_id(v: &Value) -> Option<u64> {
    match v {
        Value::String(s) => s.trim().parse().ok(),
        Value::Number(n) => n.as_u64(),
        _ => None,
    }
    .filter(|n| *n > 0)
}

fn channel_value(st: &WebState, v: &Value, field: &str) -> Result<Option<String>, ApiError> {
    if v.is_null() || v.as_str() == Some("") {
        return Ok(None);
    }
    let id = parse_id(v).ok_or_else(|| ApiError::bad(format!("{field} : salon invalide")))?;
    if !text_channels(st).contains(&id) {
        return Err(ApiError::bad(format!(
            "{field} : salon textuel introuvable"
        )));
    }
    Ok(Some(id.to_string()))
}

// ---------------------------------------------------------------------------
// Fonctionnalités
// ---------------------------------------------------------------------------

async fn features_list(_st: St, _a: Admin) -> ApiResult {
    let list: Vec<Value> = features::FEATURES
        .iter()
        .map(|(key, _)| {
            json!({
                "key": key,
                "label": features::feature_label(key, None),
                "enabled": features::is_enabled(key),
                "env": features::feature_env_key(key),
            })
        })
        .collect();
    Ok(Json(json!(list)))
}

#[derive(Deserialize)]
struct Toggle {
    enabled: bool,
}

async fn feature_toggle(
    State(st): St,
    admin: Admin,
    Path(key): Path<String>,
    Json(body): Json<Toggle>,
) -> ApiResult {
    if !features::FEATURES.iter().any(|(k, _)| *k == key) {
        return Err(ApiError::not_found());
    }
    config::set_env_key(
        &st.data.config.root,
        &features::feature_env_key(&key),
        if body.enabled { "true" } else { "false" },
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    audit(&admin, &format!("fonctionnalité {key} → {}", body.enabled));
    let synced = commands::sync(&st.ctx.http, &commands::all(), st.guild())
        .await
        .is_ok();
    Ok(Json(
        json!({ "enabled": features::is_enabled(&key), "synced": synced }),
    ))
}

// ---------------------------------------------------------------------------
// Modération
// ---------------------------------------------------------------------------

async fn mod_config(State(st): St, _a: Admin) -> ApiResult {
    let g = guild_str(&st);
    let c = st.data.db.run(move |c| Ok(m::get_config(c, &g)?)).await?;
    let c = c.unwrap_or(m::ModConfig {
        log_channel_id: None,
        appeal_channel_id: None,
        ai_enabled: true,
        ai_confidence_threshold: 60,
        ai_flag_channel_id: None,
        ai_model: "llama2".into(),
        ollama_host: "http://localhost:11434".into(),
        warn_1_decay_days: 7,
        warn_2_decay_days: 14,
        warn_3_decay_days: 21,
        mute_duration_warn_2: 3600,
        mute_duration_warn_3: 86400,
        rules_message_id: None,
    });
    Ok(Json(json!({
        "log_channel_id": c.log_channel_id,
        "appeal_channel_id": c.appeal_channel_id,
        "ai_flag_channel_id": c.ai_flag_channel_id,
        "ai_enabled": c.ai_enabled,
        "ai_confidence_threshold": c.ai_confidence_threshold,
        "ai_model": c.ai_model,
        "ollama_host": c.ollama_host,
        "rules_message_id": c.rules_message_id,
        "warn_1_decay_days": c.warn_1_decay_days,
        "warn_2_decay_days": c.warn_2_decay_days,
        "warn_3_decay_days": c.warn_3_decay_days,
        "mute_duration_warn_2": c.mute_duration_warn_2,
        "mute_duration_warn_3": c.mute_duration_warn_3,
    })))
}

fn int_in(v: &Value, field: &str, min: i64, max: i64) -> Result<i64, ApiError> {
    let n = match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    };
    n.filter(|n| (min..=max).contains(n))
        .ok_or_else(|| ApiError::bad(format!("{field} : nombre entre {min} et {max} attendu")))
}

/// Valeur SQL validée pour un champ de configuration.
enum Sql {
    Null,
    Int(i64),
    Text(String),
}

async fn mod_config_update(
    State(st): St,
    admin: Admin,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let mut updates: Vec<(&'static str, Sql)> = Vec::new();
    for (k, v) in &body {
        let (col, val) = match k.as_str() {
            "log_channel_id" => ("log_channel_id", channel_value(&st, v, k)?),
            "appeal_channel_id" => ("appeal_channel_id", channel_value(&st, v, k)?),
            "ai_flag_channel_id" => ("ai_flag_channel_id", channel_value(&st, v, k)?),
            "ai_enabled" => {
                let b = v
                    .as_bool()
                    .ok_or_else(|| ApiError::bad("ai_enabled : booléen attendu"))?;
                updates.push(("ai_enabled", Sql::Int(b as i64)));
                continue;
            }
            "ai_confidence_threshold" => {
                updates.push(("ai_confidence_threshold", Sql::Int(int_in(v, k, 0, 100)?)));
                continue;
            }
            "warn_1_decay_days" | "warn_2_decay_days" | "warn_3_decay_days" => {
                let col = m::CONFIG_COLUMNS
                    .iter()
                    .find(|c| **c == k.as_str())
                    .unwrap();
                updates.push((col, Sql::Int(int_in(v, k, 1, 3650)?)));
                continue;
            }
            "mute_duration_warn_2" | "mute_duration_warn_3" => {
                let col = m::CONFIG_COLUMNS
                    .iter()
                    .find(|c| **c == k.as_str())
                    .unwrap();
                // Discord limite les timeouts à 28 jours.
                updates.push((col, Sql::Int(int_in(v, k, 60, 28 * 86400)?)));
                continue;
            }
            "ai_model" => {
                let s = v
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty() && s.len() <= 100);
                (
                    "ai_model",
                    Some(
                        s.ok_or_else(|| ApiError::bad("ai_model : texte attendu"))?
                            .to_string(),
                    ),
                )
            }
            "ollama_host" => {
                let s = v.as_str().map(str::trim).filter(|s| {
                    (s.starts_with("http://") || s.starts_with("https://")) && s.len() <= 200
                });
                (
                    "ollama_host",
                    Some(
                        s.ok_or_else(|| ApiError::bad("ollama_host : URL http(s) attendue"))?
                            .to_string(),
                    ),
                )
            }
            "rules_message_id" => (
                "rules_message_id",
                if v.is_null() || v.as_str() == Some("") {
                    None
                } else {
                    Some(
                        parse_id(v)
                            .ok_or_else(|| ApiError::bad("rules_message_id : ID invalide"))?
                            .to_string(),
                    )
                },
            ),
            other => return Err(ApiError::bad(format!("Paramètre inconnu : {other}"))),
        };
        updates.push((col, val.map(Sql::Text).unwrap_or(Sql::Null)));
    }
    let g = guild_str(&st);
    let summary = updates
        .iter()
        .map(|(c, _)| *c)
        .collect::<Vec<_>>()
        .join(", ");
    st.data
        .db
        .tx(move |c| {
            c.execute(
                "INSERT OR IGNORE INTO moderation_config (guild_id, created_at) VALUES (?1, ?2)",
                params![g, m::now_iso()],
            )?;
            for (col, val) in updates {
                // `col` provient d'une liste blanche.
                let sql = format!("UPDATE moderation_config SET {col} = ?1 WHERE guild_id = ?2");
                match val {
                    Sql::Null => c.execute(&sql, params![Option::<String>::None, g])?,
                    Sql::Int(n) => c.execute(&sql, params![n, g])?,
                    Sql::Text(t) => c.execute(&sql, params![t, g])?,
                };
            }
            Ok(())
        })
        .await?;
    audit(
        &admin,
        &format!("configuration de modération modifiée ({summary})"),
    );
    mod_config(State(st), admin).await
}

/// Nom d'affichage d'un membre (cache, puis API), ou son ID.
async fn member_name(st: &WebState, id: &str) -> String {
    let Ok(uid) = id.parse::<u64>() else {
        return id.to_string();
    };
    match st.guild().member(&st.ctx, serenity::UserId::new(uid)).await {
        Ok(mb) => mb.display_name().to_string(),
        Err(_) => format!("Utilisateur {id}"),
    }
}

async fn mod_overview(State(st): St, _a: Admin) -> ApiResult {
    let g = guild_str(&st);
    #[allow(clippy::type_complexity)]
    let (warnings, mutes, appeals, history, flags): (
        Vec<(String, i64, String)>,
        Vec<(String, String, String, bool)>,
        Vec<(i64, String, String, String)>,
        Vec<m::HistoryEntry>,
        Vec<(i64, String, String, String, i64, String, String)>,
    ) = st
        .data
        .db
        .run(move |c| {
            let warnings = c
                .prepare("SELECT user_id, warn_count, updated_at FROM warnings WHERE guild_id = ?1 AND warn_count > 0 ORDER BY warn_count DESC LIMIT 100")?
                .query_map([&g], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let mutes = c
                .prepare("SELECT user_id, reason, expires_at, COALESCE(is_auto, 0) FROM active_mutes WHERE guild_id = ?1 ORDER BY expires_at")?
                .query_map([&g], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, i64>(3)? != 0)))?
                .collect::<rusqlite::Result<_>>()?;
            let appeals = c
                .prepare("SELECT id, user_id, appeal_reason, created_at FROM moderation_appeals WHERE guild_id = ?1 AND status = 'pending' ORDER BY created_at")?
                .query_map([&g], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let flags = c
                .prepare("SELECT id, user_id, channel_id, message_content, ai_score, ai_category, ai_reason FROM ai_flags WHERE guild_id = ?1 AND moderator_action = 'pending' ORDER BY ai_score DESC, created_at LIMIT 25")?
                .query_map([&g], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?
                .collect::<rusqlite::Result<_>>()?;
            Ok((warnings, mutes, appeals, m::history(c, &g, None, 30)?, flags))
        })
        .await?;

    let mut w = Vec::new();
    for (uid, count, updated) in warnings {
        w.push(json!({ "user_id": uid, "name": member_name(&st, &uid).await, "count": count, "since": m::ts(&updated) }));
    }
    let mut mu = Vec::new();
    for (uid, reason, expires, auto) in mutes {
        mu.push(json!({ "user_id": uid, "name": member_name(&st, &uid).await, "reason": reason, "expires": m::ts(&expires), "auto": auto }));
    }
    let mut ap = Vec::new();
    for (id, uid, reason, created) in appeals {
        ap.push(json!({ "id": id, "user_id": uid, "name": member_name(&st, &uid).await, "reason": reason, "created": m::ts(&created) }));
    }
    let mut hi = Vec::new();
    for h in history {
        hi.push(json!({
            "user_id": h.user_id,
            "name": member_name(&st, &h.user_id).await,
            "action": m::title_case(&h.action),
            "reason": h.reason,
            "at": m::ts(&h.created_at),
        }));
    }
    let mut fl = Vec::new();
    for (id, uid, ch, content, score, cat, reason) in flags {
        fl.push(json!({
            "id": id, "user_id": uid, "name": member_name(&st, &uid).await, "channel_id": ch,
            "content": content, "score": score, "category": cat, "reason": reason,
        }));
    }
    Ok(Json(
        json!({ "warnings": w, "mutes": mu, "appeals": ap, "history": hi, "flags": fl }),
    ))
}

#[derive(Deserialize)]
struct UserBody {
    user_id: String,
    reason: Option<String>,
}

fn reason_or(body: &UserBody, default: &str, admin: &Admin) -> String {
    let r = body
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .unwrap_or(default);
    format!("{r} (interface web, {})", admin.session.username)
}

async fn mod_unwarn(State(st): St, admin: Admin, Json(body): Json<UserBody>) -> ApiResult {
    let uid = body
        .user_id
        .trim()
        .parse::<u64>()
        .map_err(|_| ApiError::bad("ID utilisateur invalide"))?;
    let reason = reason_or(&body, "Retiré par un modérateur", &admin);
    let (g, u, mo, r) = (
        guild_str(&st),
        uid.to_string(),
        admin.session.user_id.to_string(),
        reason.clone(),
    );
    let (new, config) = st
        .data
        .db
        .run(move |c| {
            if m::warning_count(c, &g, &u)? <= 0 {
                return Ok(None);
            }
            Ok(Some((
                m::decrement_warning(c, &g, &u, Some(&mo), Some(&r))?,
                m::get_config(c, &g)?,
            )))
        })
        .await?
        .ok_or_else(|| ApiError::bad("Cet utilisateur n'a aucun avertissement"))?;
    let user = serenity::UserId::new(uid);
    let moderator = serenity::UserId::new(admin.session.user_id);
    if new == 0 {
        let (g, u) = (guild_str(&st), uid.to_string());
        if st
            .data
            .db
            .run(move |c| Ok(m::active_mute(c, &g, &u)?))
            .await?
            == Some(true)
            && let Err(e) = m::lift_mute(
                &st.ctx.http,
                &st.data,
                st.guild(),
                user,
                Some(moderator.to_string()),
                reason.clone(),
            )
            .await
        {
            tracing::error!("[Web] Retrait du mute automatique impossible: {e:#}");
        }
    }
    m::post_modlog(
        &st.ctx.http,
        config.as_ref(),
        m::modlog_embed(
            "unwarn",
            user,
            Some(moderator),
            &[("reason", reason), ("warn_count", new.to_string())],
            None,
        ),
    )
    .await;
    audit(
        &admin,
        &format!("avertissement retiré à {uid} (reste {new})"),
    );
    Ok(Json(json!({ "count": new })))
}

async fn mod_unmute(State(st): St, admin: Admin, Json(body): Json<UserBody>) -> ApiResult {
    let uid = body
        .user_id
        .trim()
        .parse::<u64>()
        .map_err(|_| ApiError::bad("ID utilisateur invalide"))?;
    let reason = reason_or(&body, "Démuté par un modérateur", &admin);
    let user = serenity::UserId::new(uid);
    let moderator = serenity::UserId::new(admin.session.user_id);
    if let Err(e) = m::lift_mute(
        &st.ctx.http,
        &st.data,
        st.guild(),
        user,
        Some(moderator.to_string()),
        reason.clone(),
    )
    .await
    {
        // Membre parti ou permissions manquantes : on nettoie au moins la base.
        tracing::warn!("[Web] Timeout Discord non retiré pour {uid}: {e:#}");
        let (g, u, r) = (guild_str(&st), uid.to_string(), reason.clone());
        st.data
            .db
            .run(move |c| Ok(m::remove_mute(c, &g, &u, None, &r)?))
            .await?;
    }
    let config = m::load_config(&st.data, st.guild()).await;
    m::post_modlog(
        &st.ctx.http,
        config.as_ref(),
        m::modlog_embed("unmute", user, Some(moderator), &[("reason", reason)], None),
    )
    .await;
    audit(&admin, &format!("mute levé pour {uid}"));
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct FlagReview {
    action: String,
}

async fn mod_flag_review(
    State(st): St,
    admin: Admin,
    Path(id): Path<i64>,
    Json(body): Json<FlagReview>,
) -> ApiResult {
    if !["reviewed", "ignored", "false_positive"].contains(&body.action.as_str()) {
        return Err(ApiError::bad("Action invalide"));
    }
    let (g, mo, action) = (
        guild_str(&st),
        admin.session.user_id.to_string(),
        body.action.clone(),
    );
    let n = st
        .data
        .db
        .run(move |c| {
            Ok(c.execute(
                "UPDATE ai_flags SET moderator_action = ?1, moderator_id = ?2, reviewed_at = ?3 WHERE id = ?4 AND guild_id = ?5",
                params![action, mo, m::now_iso(), id, g],
            )?)
        })
        .await?;
    if n == 0 {
        return Err(ApiError::not_found());
    }
    audit(&admin, &format!("signalement IA #{id} → {}", body.action));
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Minijeu et compteur
// ---------------------------------------------------------------------------

async fn minigame_settings(State(st): St, _a: Admin) -> ApiResult {
    let g = guild_str(&st);
    let s = st
        .data
        .db
        .tx(move |c| economy::guild_settings(c, &g))
        .await?;
    Ok(Json(json!({
        "global_enabled": features::is_enabled("minigame"),
        "minigame_enabled": s.minigame_enabled,
        "minigame_channel_id": s.minigame_channel_id,
        "xp_trading_enabled": s.xp_trading_enabled,
        "trade_tax_percent": s.trade_tax_percent,
        "duel_tax_percent": s.duel_tax_percent,
        "daily_xp_transfer_cap_percent": s.daily_xp_transfer_cap_percent,
        "daily_xp_transfer_cap_max": s.daily_xp_transfer_cap_max,
        "capture_cooldown_seconds": s.capture_cooldown_seconds,
        "duel_cooldown_seconds": s.duel_cooldown_seconds,
    })))
}

fn percent(v: &Value, field: &str) -> Result<f64, ApiError> {
    let n = match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().replace(',', ".").parse().ok(),
        _ => None,
    };
    n.filter(|n| (0.0..=100.0).contains(n))
        .ok_or_else(|| ApiError::bad(format!("{field} : pourcentage entre 0 et 100 attendu")))
}

async fn minigame_update(
    State(st): St,
    admin: Admin,
    Json(body): Json<Map<String, Value>>,
) -> ApiResult {
    let mut updates: Vec<(&'static str, Sql)> = Vec::new();
    for (k, v) in &body {
        let entry = match k.as_str() {
            "minigame_enabled" | "xp_trading_enabled" => {
                let b = v
                    .as_bool()
                    .ok_or_else(|| ApiError::bad(format!("{k} : booléen attendu")))?;
                let col = if k == "minigame_enabled" {
                    "minigame_enabled"
                } else {
                    "xp_trading_enabled"
                };
                (col, Sql::Int(b as i64))
            }
            "minigame_channel_id" => (
                "minigame_channel_id",
                channel_value(&st, v, k)?
                    .map(Sql::Text)
                    .unwrap_or(Sql::Null),
            ),
            "trade_tax_percent" | "duel_tax_percent" | "daily_xp_transfer_cap_percent" => {
                let col = match k.as_str() {
                    "trade_tax_percent" => "trade_tax_percent",
                    "duel_tax_percent" => "duel_tax_percent",
                    _ => "daily_xp_transfer_cap_percent",
                };
                (col, Sql::Text(percent(v, k)?.to_string()))
            }
            "daily_xp_transfer_cap_max" => (
                "daily_xp_transfer_cap_max",
                Sql::Int(int_in(v, k, 0, 1_000_000)?),
            ),
            "capture_cooldown_seconds" => (
                "capture_cooldown_seconds",
                Sql::Int(int_in(v, k, 0, 86400)?),
            ),
            "duel_cooldown_seconds" => ("duel_cooldown_seconds", Sql::Int(int_in(v, k, 0, 86400)?)),
            other => return Err(ApiError::bad(format!("Paramètre inconnu : {other}"))),
        };
        updates.push(entry);
    }
    let g = guild_str(&st);
    st.data
        .db
        .tx(move |c| {
            economy::guild_settings(c, &g)?;
            for (col, val) in updates {
                let sql = format!(
                    "UPDATE guild_settings SET {col} = ?1, updated_at = ?2 WHERE guildId = ?3"
                );
                let now = crate::db::utcnow_iso();
                match val {
                    Sql::Null => c.execute(&sql, params![Option::<String>::None, now, g])?,
                    Sql::Int(n) => c.execute(&sql, params![n, now, g])?,
                    // Les pourcentages sont des REAL : on les réécrit en nombre.
                    Sql::Text(t) => match t.parse::<f64>() {
                        Ok(f) => c.execute(&sql, params![f, now, g])?,
                        Err(_) => c.execute(&sql, params![t, now, g])?,
                    },
                };
            }
            Ok(())
        })
        .await?;
    audit(&admin, "réglages du minijeu modifiés");
    minigame_settings(State(st), admin).await
}

async fn counter_list(State(st): St, _a: Admin) -> ApiResult {
    let g = guild_str(&st);
    let rows: Vec<(String, i64)> = st
        .data
        .db
        .run(move |c| {
            Ok(c.prepare(
                "SELECT channelId, COALESCE(count, 0) FROM counter_game WHERE guildId = ?1",
            )?
            .query_map([g], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?)
        })
        .await?;
    Ok(Json(json!(
        rows.into_iter()
            .map(|(ch, n)| json!({"channel_id": ch, "count": n}))
            .collect::<Vec<_>>()
    )))
}

#[derive(Deserialize)]
struct ChannelBody {
    channel_id: Value,
}

async fn counter_add(State(st): St, admin: Admin, Json(body): Json<ChannelBody>) -> ApiResult {
    let ch = channel_value(&st, &body.channel_id, "channel_id")?
        .ok_or_else(|| ApiError::bad("Salon requis"))?;
    let (g, c2, u) = (
        guild_str(&st),
        ch.clone(),
        admin.session.user_id.to_string(),
    );
    let added = st
        .data
        .db
        .run(move |c| {
            Ok(c.execute(
                "INSERT OR IGNORE INTO counter_game (guildId, channelId, messageId, userId, lastUserId, count)
                 VALUES (?1, ?2, '', ?3, '0', 0)",
                params![g, c2, u],
            )? > 0)
        })
        .await?;
    if !added {
        return Err(ApiError::bad("Ce salon a déjà un compteur"));
    }
    audit(&admin, &format!("compteur ajouté dans {ch}"));
    counter_list(State(st), admin).await
}

async fn counter_remove(State(st): St, admin: Admin, Path(channel): Path<String>) -> ApiResult {
    let (g, ch) = (guild_str(&st), channel.clone());
    let n = st
        .data
        .db
        .run(move |c| {
            Ok(c.execute(
                "DELETE FROM counter_game WHERE guildId = ?1 AND channelId = ?2",
                params![g, ch],
            )?)
        })
        .await?;
    if n == 0 {
        return Err(ApiError::not_found());
    }
    audit(&admin, &format!("compteur retiré de {channel}"));
    counter_list(State(st), admin).await
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

async fn notifications(State(st): St, _a: Admin) -> ApiResult {
    let v = st
        .data
        .db
        .run(|c| {
            let twitch: Vec<Value> = c
                .prepare("SELECT id, streamerName, streamChannelId, roleId, COALESCE(announced, 0) FROM streamers ORDER BY streamerName")?
                .query_map([], |r| {
                    Ok(json!({
                        "id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?,
                        "channel_id": r.get::<_, Option<String>>(2)?, "role_id": r.get::<_, Option<String>>(3)?,
                        "live": r.get::<_, i64>(4)? != 0,
                    }))
                })?
                .collect::<rusqlite::Result<_>>()?;
            let youtube: Vec<Value> = c
                .prepare("SELECT id, channelName, channelId, discordChannelId, roleId, COALESCE(notifyVideos, 1), COALESCE(notifyShorts, 1) FROM youtube_channels ORDER BY channelName")?
                .query_map([], |r| {
                    Ok(json!({
                        "id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "youtube_id": r.get::<_, String>(2)?,
                        "channel_id": r.get::<_, String>(3)?, "role_id": r.get::<_, Option<String>>(4)?,
                        "videos": r.get::<_, i64>(5)? != 0, "shorts": r.get::<_, i64>(6)? != 0,
                    }))
                })?
                .collect::<rusqlite::Result<_>>()?;
            let bluesky: Vec<Value> = c
                .prepare("SELECT id, handle, displayName, discordChannelId, roleId FROM bluesky_accounts ORDER BY handle")?
                .query_map([], |r| {
                    Ok(json!({
                        "id": r.get::<_, i64>(0)?, "handle": r.get::<_, String>(1)?, "name": r.get::<_, Option<String>>(2)?,
                        "channel_id": r.get::<_, String>(3)?, "role_id": r.get::<_, Option<String>>(4)?,
                    }))
                })?
                .collect::<rusqlite::Result<_>>()?;
            Ok(json!({ "twitch": twitch, "youtube": youtube, "bluesky": bluesky }))
        })
        .await?;
    Ok(Json(json!({
        "data": v,
        "enabled": {
            "twitch": features::is_enabled("stream"),
            "youtube": features::is_enabled("youtube"),
            "bluesky": features::is_enabled("bluesky"),
        },
    })))
}

#[derive(Deserialize)]
struct NotificationBody {
    target: String,
    channel_id: Value,
    role_id: Option<Value>,
    notify_videos: Option<bool>,
    notify_shorts: Option<bool>,
}

/// Retire le markdown Discord des messages de confirmation (affichés en texte brut).
fn plain(msg: String) -> String {
    msg.replace("**", "")
}

async fn notification_add(
    State(st): St,
    admin: Admin,
    Path(kind): Path<String>,
    Json(b): Json<NotificationBody>,
) -> ApiResult {
    let channel = channel_value(&st, &b.channel_id, "channel_id")?
        .and_then(|c| c.parse::<u64>().ok())
        .ok_or_else(|| ApiError::bad("Salon requis"))?;
    let role = match b
        .role_id
        .as_ref()
        .filter(|v| !v.is_null() && v.as_str() != Some(""))
    {
        Some(v) => {
            let id = parse_id(v)
                .filter(|id| role_exists(&st, *id))
                .ok_or_else(|| ApiError::bad("Rôle introuvable"))?;
            Some(id)
        }
        None => None,
    };
    let loc = Some("fr");
    let result = match kind.as_str() {
        "twitch" => commands::stream::add_streamer(&st.data, &b.target, channel, role, loc).await?,
        "youtube" => {
            commands::youtube::add_channel(
                &st.data,
                &b.target,
                channel,
                role,
                b.notify_videos.unwrap_or(true),
                b.notify_shorts.unwrap_or(true),
                loc,
            )
            .await?
        }
        "bluesky" => {
            commands::bluesky::add_account(&st.data, &b.target, channel, role, loc).await?
        }
        _ => return Err(ApiError::not_found()),
    };
    match result {
        Ok(msg) => {
            audit(
                &admin,
                &format!("notification {kind} ajoutée : {}", b.target.trim()),
            );
            Ok(Json(json!({ "message": plain(msg) })))
        }
        Err(msg) => Err(ApiError::bad(plain(msg))),
    }
}

async fn notification_remove(
    State(st): St,
    admin: Admin,
    Path((kind, id)): Path<(String, i64)>,
) -> ApiResult {
    let table = match kind.as_str() {
        "twitch" => "streamers",
        "youtube" => "youtube_channels",
        "bluesky" => "bluesky_accounts",
        _ => return Err(ApiError::not_found()),
    };
    let n = st
        .data
        .db
        .run(move |c| Ok(c.execute(&format!("DELETE FROM {table} WHERE id = ?1"), [id])?))
        .await?;
    if n == 0 {
        return Err(ApiError::not_found());
    }
    audit(&admin, &format!("notification {kind} #{id} supprimée"));
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Classement
// ---------------------------------------------------------------------------

async fn leaderboard(State(st): St, _a: Admin) -> ApiResult {
    let g = guild_str(&st);
    let rows: Vec<(String, f64, i64, i64, f64)> = st
        .data
        .db
        .run(move |c| {
            Ok(c.prepare(
                "SELECT userId, COALESCE(xp, 0), COALESCE(level, 1), COALESCE(messages, 0), COALESCE(coins, 0)
                 FROM users WHERE guildId = ?1 ORDER BY xp DESC LIMIT 25",
            )?
            .query_map([g], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
            .collect::<rusqlite::Result<_>>()?)
        })
        .await?;
    let mut out = Vec::new();
    for (uid, xp, level, messages, coins) in rows {
        out.push(json!({
            "user_id": uid, "name": member_name(&st, &uid).await,
            "xp": xp.round(), "level": level, "messages": messages, "coins": coins.round(),
        }));
    }
    Ok(Json(json!(out)))
}

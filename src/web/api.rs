//! API JSON de l'interface web (toutes les routes exigent un administrateur).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use poise::serenity_prelude as serenity;
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Admin, WebState};
use crate::{commands, config, features};

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
        .route("/counter", get(counter_get).put(counter_set))
        .route("/notifications", get(notifications))
        .route("/notifications/{kind}", post(notification_add))
        .route("/notifications/{kind}/{id}", delete(notification_remove))
        .route("/leaderboard", get(leaderboard))
        .route(
            "/members/{id}",
            get(member_get).put(member_update).delete(member_reset),
        )
        .route("/audit", get(audit_log))
        .route("/backups", get(backups_list).post(backup_create))
        .route("/backups/{name}", get(backup_download))
}

fn guild_str(st: &WebState) -> String {
    st.data.config.server_id.to_string()
}

/// Trace une action dans les logs et dans le journal persistant (`web_audit_log`).
fn audit(st: &WebState, admin: &Admin, action: &str) {
    let (uid, name, action) = (
        admin.session.user_id.to_string(),
        admin.session.username.clone(),
        action.to_string(),
    );
    tracing::info!("[Web] {name} ({uid}) : {action}");
    let db = st.data.db.clone();
    tokio::spawn(async move {
        let r = db
            .run(move |c| {
                c.execute(
                    "INSERT INTO web_audit_log (user_id, username, action, created_at) VALUES (?1, ?2, ?3, ?4)",
                    params![uid, name, action, chrono::Utc::now().to_rfc3339()],
                )?;
                Ok(())
            })
            .await;
        if let Err(e) = r {
            tracing::error!("[Web] Journal d'audit non enregistré: {e:#}");
        }
    });
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
    audit(
        &st,
        &admin,
        &format!("fonctionnalité {key} → {}", body.enabled),
    );
    let synced = commands::sync(&st.ctx.http, &commands::all(), st.guild())
        .await
        .is_ok();
    Ok(Json(
        json!({ "enabled": features::is_enabled(&key), "synced": synced }),
    ))
}

// ---------------------------------------------------------------------------
// Membres et compteur
// ---------------------------------------------------------------------------

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

fn int_in(v: &Value, field: &str, min: i64, max: i64) -> Result<i64, ApiError> {
    let n = match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    };
    n.filter(|n| (min..=max).contains(n))
        .ok_or_else(|| ApiError::bad(format!("{field} : nombre entre {min} et {max} attendu")))
}

/// Salon du jeu du compteur (un seul par serveur).
async fn counter_get(State(st): St, _a: Admin) -> ApiResult {
    let g = guild_str(&st);
    let row: Option<(String, i64)> = st
        .data
        .db
        .run(move |c| {
            Ok(c.query_row(
                "SELECT channelId, COALESCE(count, 0) FROM counter_game WHERE guildId = ?1 ORDER BY rowid LIMIT 1",
                [g],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
        })
        .await?;
    Ok(Json(match row {
        Some((ch, n)) => json!({ "channel_id": ch, "count": n }),
        None => json!({ "channel_id": null, "count": 0 }),
    }))
}

#[derive(Deserialize)]
struct ChannelBody {
    channel_id: Value,
}

async fn counter_set(State(st): St, admin: Admin, Json(body): Json<ChannelBody>) -> ApiResult {
    let ch = channel_value(&st, &body.channel_id, "channel_id")?;
    let (g, c2, u) = (
        guild_str(&st),
        ch.clone(),
        admin.session.user_id.to_string(),
    );
    st.data
        .db
        .tx(move |c| {
            match c2 {
                Some(ch) => crate::commands::count::set_channel(c, &g, &ch, &u)?,
                None => {
                    c.execute("DELETE FROM counter_game WHERE guildId = ?1", [&g])?;
                }
            }
            Ok(())
        })
        .await?;
    audit(
        &st,
        &admin,
        &match &ch {
            Some(c) => format!("salon du compteur → {c}"),
            None => "compteur désactivé".into(),
        },
    );
    counter_get(State(st), admin).await
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
                &st,
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
    audit(&st, &admin, &format!("notification {kind} #{id} supprimée"));
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

// ---------------------------------------------------------------------------
// Membres : consultation et modification de l'XP
// ---------------------------------------------------------------------------

fn member_id(id: &str) -> Result<u64, ApiError> {
    id.trim()
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| ApiError::bad("ID de membre invalide"))
}

async fn member_get(State(st): St, _a: Admin, Path(id): Path<String>) -> ApiResult {
    let uid = member_id(&id)?;
    let (g, u) = (guild_str(&st), uid.to_string());
    let row: Option<(f64, i64, i64)> = st
        .data
        .db
        .run(move |c| {
            Ok(c.query_row(
                "SELECT COALESCE(xp, 0), COALESCE(level, 1), COALESCE(messages, 0) FROM users WHERE guildId = ?1 AND userId = ?2",
                params![g, u],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?)
        })
        .await?;
    let (xp, level, messages) = row.unwrap_or((0.0, 1, 0));
    Ok(Json(json!({
        "user_id": uid.to_string(),
        "name": member_name(&st, &uid.to_string()).await,
        "xp": xp.round(), "level": level, "messages": messages,
        "exists": row.is_some(),
    })))
}

#[derive(Deserialize)]
struct XpBody {
    xp: Value,
}

async fn member_update(
    State(st): St,
    admin: Admin,
    Path(id): Path<String>,
    Json(b): Json<XpBody>,
) -> ApiResult {
    let uid = member_id(&id)?;
    let xp = int_in(&b.xp, "xp", 0, 100_000_000)?;
    let level = crate::xp::level_from_xp(xp as f64);
    let (g, u) = (guild_str(&st), uid.to_string());
    st.data
        .db
        .run(move |c| {
            c.execute(
                "INSERT INTO users (guildId, userId, xp, level, messages, coins) VALUES (?1, ?2, ?3, ?4, 0, 0)
                 ON CONFLICT(guildId, userId) DO UPDATE SET xp = ?3, level = ?4",
                params![g, u, xp as f64, level],
            )?;
            Ok(())
        })
        .await?;
    audit(
        &st,
        &admin,
        &format!("XP de {uid} fixée à {xp} (niveau {level})"),
    );
    member_get(State(st), admin, Path(id)).await
}

async fn member_reset(State(st): St, admin: Admin, Path(id): Path<String>) -> ApiResult {
    let uid = member_id(&id)?;
    let (g, u) = (guild_str(&st), uid.to_string());
    let n = st
        .data
        .db
        .run(move |c| {
            Ok(c.execute(
                "DELETE FROM users WHERE guildId = ?1 AND userId = ?2",
                params![g, u],
            )?)
        })
        .await?;
    if n == 0 {
        return Err(ApiError::not_found());
    }
    audit(&st, &admin, &format!("progression de {uid} réinitialisée"));
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Journal des actions web
// ---------------------------------------------------------------------------

async fn audit_log(State(st): St, _a: Admin) -> ApiResult {
    let rows: Vec<Value> = st
        .data
        .db
        .run(|c| {
            Ok(c.prepare("SELECT username, user_id, action, created_at FROM web_audit_log ORDER BY id DESC LIMIT 200")?
                .query_map([], |r| {
                    let at: String = r.get(3)?;
                    Ok(json!({
                        "name": r.get::<_, String>(0)?,
                        "user_id": r.get::<_, String>(1)?,
                        "action": r.get::<_, String>(2)?,
                        "at": chrono::DateTime::parse_from_rfc3339(&at).map(|d| d.timestamp()).unwrap_or(0),
                    }))
                })?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await?;
    Ok(Json(json!(rows)))
}

// ---------------------------------------------------------------------------
// Sauvegardes
// ---------------------------------------------------------------------------

/// Nom de fichier de sauvegarde valide (empêche toute sortie du dossier).
fn valid_backup_name(name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix("db_backup_")
        .and_then(|r| r.strip_suffix(".sqlite3"))
    else {
        return false;
    };
    rest.len() == 15
        && rest
            .chars()
            .enumerate()
            .all(|(i, c)| if i == 8 { c == '_' } else { c.is_ascii_digit() })
}

async fn backups_list(State(st): St, _a: Admin) -> ApiResult {
    let dir = crate::backup::backup_dir(&st.data.config.root);
    let list = tokio::task::spawn_blocking(move || {
        crate::backup::backups(&dir, crate::backup::PREFIX)
            .into_iter()
            .filter_map(|p| {
                let meta = p.metadata().ok()?;
                let name = p.file_name()?.to_string_lossy().to_string();
                let at = meta
                    .modified()
                    .ok()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?
                    .as_secs();
                Some(json!({ "name": name, "size": meta.len(), "at": at }))
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| anyhow::anyhow!(e))?;
    Ok(Json(json!(list)))
}

async fn backup_create(State(st): St, admin: Admin) -> ApiResult {
    let db = st.data.db.path().to_path_buf();
    let dir = crate::backup::backup_dir(&st.data.config.root);
    let path = tokio::task::spawn_blocking(move || {
        crate::backup::backup_database(&db, &dir, crate::backup::PREFIX)
    })
    .await
    .map_err(|e| anyhow::anyhow!(e))??;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    audit(&st, &admin, &format!("sauvegarde créée : {name}"));
    Ok(Json(json!({ "name": name })))
}

async fn backup_download(
    State(st): St,
    admin: Admin,
    Path(name): Path<String>,
) -> Result<Response, ApiError> {
    if !valid_backup_name(&name) {
        return Err(ApiError::bad("Nom de sauvegarde invalide"));
    }
    let path = crate::backup::backup_dir(&st.data.config.root).join(&name);
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| ApiError::not_found())?;
    audit(&st, &admin, &format!("sauvegarde téléchargée : {name}"));
    Ok((
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/vnd.sqlite3".to_string(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
            (axum::http::header::CACHE_CONTROL, "no-store".to_string()),
        ],
        bytes,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    #[test]
    fn backup_names_cannot_escape_the_folder() {
        assert!(super::valid_backup_name(
            "db_backup_20261123_081500.sqlite3"
        ));
        assert!(!super::valid_backup_name(
            "../db_backup_20261123_081500.sqlite3"
        ));
        assert!(!super::valid_backup_name(
            "db_backup_2026112X_081500.sqlite3"
        ));
        assert!(!super::valid_backup_name("database.sqlite3"));
        assert!(!super::valid_backup_name(
            "db_backup_20261123_081500.sqlite3/../x"
        ));
    }
}

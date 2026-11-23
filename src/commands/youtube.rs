//! Notifications YouTube (vidéos et shorts) : /youtube_add, /youtube_remove et vérification périodique.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use poise::serenity_prelude as serenity;
use rusqlite::params;
use serde_json::Value;

use crate::bot::{Context, Data, Error, colors, reply_ephemeral};
use crate::tr;

const API: &str = "https://www.googleapis.com/youtube/v3";

fn api_key() -> Option<String> {
    std::env::var("youtube_api_key")
        .ok()
        .filter(|v| !v.trim().is_empty())
}

/// Vérifie le format d'un identifiant (`@handle` ou `UC...` de 24 caractères).
pub fn validate_identifier(id: &str, loc: Option<&str>) -> Result<(), String> {
    if let Some(handle) = id.strip_prefix('@') {
        if handle.is_empty() {
            return Err(tr!("youtube.handle_short", loc));
        }
        if !handle
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return Err(tr!("youtube.handle_invalid", loc));
        }
        Ok(())
    } else if id.starts_with("UC") && id.len() == 24 {
        Ok(())
    } else {
        Err(tr!("youtube.id_invalid", loc))
    }
}

/// Durée ISO 8601 (`PT#H#M#S`) ≤ 60 s → short.
pub fn is_short(duration: &str) -> bool {
    let re = regex::Regex::new(r"^PT(?:(\d+)H)?(?:(\d+)M)?(?:(\d+)S)?").unwrap();
    let Some(c) = re.captures(duration) else {
        return false;
    };
    let n = |i: usize| {
        c.get(i)
            .and_then(|m| m.as_str().parse::<i64>().ok())
            .unwrap_or(0)
    };
    n(1) * 3600 + n(2) * 60 + n(3) <= 60
}

fn is_recent(published_at: &str, hours: i64) -> bool {
    DateTime::parse_from_rfc3339(published_at)
        .map(|d| Utc::now() - d.with_timezone(&Utc) <= chrono::Duration::hours(hours))
        .unwrap_or(false)
}

async fn get(
    http: &reqwest::Client,
    path: &str,
    query: &[(&str, &str)],
) -> anyhow::Result<(u16, Value)> {
    let key =
        api_key().ok_or_else(|| anyhow::anyhow!("La clé API YouTube n'est pas configurée."))?;
    let resp = http
        .get(format!("{API}/{path}"))
        .query(query)
        .query(&[("key", key.as_str())])
        .send()
        .await?;
    let status = resp.status().as_u16();
    let body = resp.json::<Value>().await.unwrap_or(Value::Null);
    Ok((status, body))
}

fn api_error(body: &Value, status: u16) -> String {
    body["error"]["message"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| format!("Status {status}"))
}

fn first_item(body: &Value) -> Option<&Value> {
    body["items"].as_array().and_then(|a| a.first())
}

/// Résout un handle en (id, titre) : forHandle, puis forUsername, puis recherche.
async fn channel_by_handle(
    http: &reqwest::Client,
    handle: &str,
) -> anyhow::Result<Option<(String, String)>> {
    let bare = handle.trim_start_matches('@');
    let (status, body) = get(
        http,
        "channels",
        &[("part", "id,snippet"), ("forHandle", bare)],
    )
    .await?;
    if status == 200 {
        if let Some(i) = first_item(&body) {
            return Ok(Some((
                i["id"].as_str().unwrap_or_default().into(),
                i["snippet"]["title"].as_str().unwrap_or(handle).into(),
            )));
        }
    } else if status != 400 {
        anyhow::bail!("Erreur API YouTube: {}", api_error(&body, status));
    }
    let (status, body) = get(
        http,
        "channels",
        &[("part", "id,snippet"), ("forUsername", bare)],
    )
    .await?;
    if status == 200 {
        if let Some(i) = first_item(&body) {
            return Ok(Some((
                i["id"].as_str().unwrap_or_default().into(),
                i["snippet"]["title"].as_str().unwrap_or(handle).into(),
            )));
        }
    }
    let (status, body) = get(
        http,
        "search",
        &[
            ("part", "snippet"),
            ("q", handle),
            ("type", "channel"),
            ("maxResults", "1"),
        ],
    )
    .await?;
    if status == 200 {
        if let Some(id) = first_item(&body).and_then(|i| i["snippet"]["channelId"].as_str()) {
            return channel_by_id(http, id).await;
        }
    }
    tracing::warn!("Aucune chaîne YouTube trouvée pour le handle '{handle}'");
    Ok(None)
}

async fn channel_by_id(
    http: &reqwest::Client,
    id: &str,
) -> anyhow::Result<Option<(String, String)>> {
    let (status, body) = get(http, "channels", &[("part", "id,snippet"), ("id", id)]).await?;
    if status != 200 {
        anyhow::bail!("Erreur lors de la vérification de la chaîne: {status}");
    }
    Ok(first_item(&body).map(|i| {
        (
            i["id"].as_str().unwrap_or(id).to_string(),
            i["snippet"]["title"].as_str().unwrap_or(id).to_string(),
        )
    }))
}

/// Dernières publications de la chaîne (de la plus récente à la plus ancienne).
async fn latest_uploads(
    http: &reqwest::Client,
    channel_id: &str,
    max: u8,
) -> anyhow::Result<Vec<Value>> {
    let (status, body) = get(
        http,
        "channels",
        &[("part", "contentDetails"), ("id", channel_id)],
    )
    .await?;
    if status == 404 {
        return Ok(vec![]);
    }
    if status != 200 {
        anyhow::bail!(
            "Erreur lors de la récupération de l'ID de playlist: {}",
            api_error(&body, status)
        );
    }
    let Some(playlist) =
        first_item(&body).and_then(|i| i["contentDetails"]["relatedPlaylists"]["uploads"].as_str())
    else {
        return Ok(vec![]);
    };
    let max = max.to_string();
    let (status, body) = get(
        http,
        "playlistItems",
        &[
            ("part", "snippet"),
            ("playlistId", playlist),
            ("maxResults", &max),
        ],
    )
    .await?;
    if status == 404 {
        return Ok(vec![]);
    }
    if status != 200 {
        anyhow::bail!(
            "Erreur lors de la récupération des vidéos: {}",
            api_error(&body, status)
        );
    }
    Ok(body["items"].as_array().cloned().unwrap_or_default())
}

/// Détails de plusieurs vidéos en un seul appel (1 unité de quota).
async fn video_details(http: &reqwest::Client, ids: &[String]) -> HashMap<String, Value> {
    if ids.is_empty() {
        return HashMap::new();
    }
    let joined = ids.join(",");
    match get(
        http,
        "videos",
        &[("part", "snippet,contentDetails"), ("id", &joined)],
    )
    .await
    {
        Ok((200, body)) => body["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|i| Some((i["id"].as_str()?.to_string(), i.clone())))
            .collect(),
        Ok((status, body)) => {
            tracing::error!(
                "Erreur lors de la récupération des détails vidéos: {}",
                api_error(&body, status)
            );
            HashMap::new()
        }
        Err(e) => {
            tracing::error!("Erreur lors de la récupération des détails vidéos: {e}");
            HashMap::new()
        }
    }
}

fn video_id(upload: &Value) -> Option<String> {
    upload["snippet"]["resourceId"]["videoId"]
        .as_str()
        .map(str::to_string)
}

/// Ajouter une chaîne YouTube à surveiller (ID de chaîne ou @handle).
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn youtube_add(
    ctx: Context<'_>,
    channel_id: String,
    #[channel_types("Text")] channel: serenity::GuildChannel,
    notify_videos: Option<bool>,
    notify_shorts: Option<bool>,
    ping_role: Option<serenity::Role>,
) -> Result<(), Error> {
    let loc = ctx.locale();
    let (notify_videos, notify_shorts) =
        (notify_videos.unwrap_or(true), notify_shorts.unwrap_or(true));
    if api_key().is_none() {
        return reply_ephemeral(ctx, tr!("youtube.no_api", loc)).await;
    }
    let ident = channel_id.trim().to_string();
    if ident.is_empty() {
        return reply_ephemeral(ctx, tr!("youtube.empty_input", loc)).await;
    }
    if let Err(msg) = validate_identifier(&ident, loc) {
        return reply_ephemeral(ctx, msg).await;
    }
    ctx.defer().await?;
    let http = &ctx.data().http;

    let resolved = if ident.starts_with('@') {
        channel_by_handle(http, &ident).await
    } else {
        channel_by_id(http, &ident).await
    };
    let (yt_id, yt_name) = match resolved {
        Ok(Some(v)) => v,
        Ok(None) => {
            let msg = if ident.starts_with('@') {
                tr!("youtube.handle_not_found", loc, handle = ident)
            } else {
                tr!("youtube.id_not_found", loc, id = ident)
            };
            ctx.say(msg).await?;
            return Ok(());
        }
        Err(e) => {
            tracing::warn!("Impossible de vérifier la chaîne {ident}: {e:#}");
            ctx.say(tr!("youtube.check_error", loc, error = e.to_string()))
                .await?;
            return Ok(());
        }
    };

    let (y, ch) = (yt_id.clone(), channel.id.to_string());
    let exists = ctx
        .data()
        .db
        .run(move |c| {
            Ok(c.prepare(
                "SELECT 1 FROM youtube_channels WHERE channelId = ?1 AND discordChannelId = ?2",
            )?
            .exists(params![y, ch])?)
        })
        .await?;
    if exists {
        ctx.say(tr!("youtube.already", loc, name = yt_name)).await?;
        return Ok(());
    }

    // Initialise le suivi pour ne pas annoncer l'ancien contenu.
    let uploads = match latest_uploads(http, &yt_id, 5).await {
        Ok(u) => u,
        Err(e) => {
            ctx.say(tr!("youtube.check_error", loc, error = e.to_string()))
                .await?;
            return Ok(());
        }
    };
    let ids: Vec<String> = uploads.iter().filter_map(video_id).collect();
    let details = video_details(http, &ids).await;
    let (mut last_video, mut last_short) = (None::<String>, None::<String>);
    for id in &ids {
        if let Some(d) = details.get(id) {
            let short = is_short(d["contentDetails"]["duration"].as_str().unwrap_or_default());
            if short && last_short.is_none() {
                last_short = Some(id.clone());
            }
            if !short && last_video.is_none() {
                last_video = Some(id.clone());
            }
            if last_short.is_some() && last_video.is_some() {
                break;
            }
        }
    }

    let (y, n, ch, role, lv, ls) = (
        yt_id.clone(),
        yt_name.clone(),
        channel.id.to_string(),
        ping_role.as_ref().map(|r| r.id.to_string()),
        last_video.clone(),
        last_short.clone(),
    );
    ctx.data()
        .db
        .run(move |c| {
            c.execute(
                "INSERT INTO youtube_channels (channelId, channelName, discordChannelId, roleId,
                    notifyVideos, notifyShorts, notifyLive, lastVideoId, lastShortId, lastLiveId)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8, NULL)",
                params![
                    y,
                    n,
                    ch,
                    role,
                    notify_videos as i64,
                    notify_shorts as i64,
                    lv,
                    ls
                ],
            )?;
            Ok(())
        })
        .await?;

    let mut notifs = Vec::new();
    if notify_videos {
        notifs.push(tr!("youtube.notif_videos", loc));
    }
    if notify_shorts {
        notifs.push(tr!("youtube.notif_shorts", loc));
    }
    let mention = format!("<#{}>", channel.id);
    if notifs.is_empty() {
        ctx.say(tr!(
            "youtube.added_warning",
            loc,
            name = yt_name,
            channel = mention,
            notifications = tr!("youtube.notif_none", loc)
        ))
        .await?;
        return Ok(());
    }
    let mut tracking = Vec::new();
    if let Some(v) = &last_video {
        tracking.push(tr!(
            "youtube.tracking_last_video",
            loc,
            id = v.chars().take(8).collect::<String>()
        ));
    }
    if let Some(s) = &last_short {
        tracking.push(tr!(
            "youtube.tracking_last_short",
            loc,
            id = s.chars().take(8).collect::<String>()
        ));
    }
    let tracking = if tracking.is_empty() {
        tr!("youtube.tracking_none", loc)
    } else {
        tr!("youtube.tracking_init", loc, items = tracking.join(", "))
    };
    ctx.say(tr!(
        "youtube.added",
        loc,
        name = yt_name,
        channel = mention,
        notifications = notifs.join(", "),
        tracking = tracking
    ))
    .await?;
    if let Some(r) = ping_role {
        ctx.say(tr!(
            "stream.role_announce",
            loc,
            role = format!("<@&{}>", r.id)
        ))
        .await?;
    }
    Ok(())
}

/// Retirer une chaîne YouTube de la liste de surveillance.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn youtube_remove(ctx: Context<'_>, channel_name: String) -> Result<(), Error> {
    let loc = ctx.locale();
    let name = channel_name.trim().to_string();
    if name.is_empty() {
        let names: Vec<String> = ctx
            .data()
            .db
            .run(|c| {
                Ok(c.prepare("SELECT channelName FROM youtube_channels")?
                    .query_map([], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await?;
        let msg = if names.is_empty() {
            tr!("youtube.remove_none", loc)
        } else {
            tr!("youtube.remove_specify", loc, list = names.join("\n"))
        };
        ctx.say(msg).await?;
        return Ok(());
    }
    let n = name.clone();
    let rows = ctx
        .data()
        .db
        .run(move |c| Ok(c.execute("DELETE FROM youtube_channels WHERE channelName = ?1", [n])?))
        .await?;
    let key = if rows > 0 {
        "youtube.removed"
    } else {
        "youtube.remove_not_found"
    };
    ctx.say(tr!(key, loc, name = name)).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Vérification périodique
// ---------------------------------------------------------------------------

struct Watched {
    id: i64,
    channel_id: String,
    name: String,
    discord_channel: u64,
    role: Option<String>,
    last_video: Option<String>,
    last_short: Option<String>,
    notify_videos: bool,
    notify_shorts: bool,
}

struct Found {
    id: String,
    title: String,
    thumbnail: Option<String>,
}

/// Toutes les 10 minutes (quota API) : nouvelles vidéos et nouveaux shorts.
pub async fn check_loop(ctx: serenity::Context, data: Data) {
    tracing::info!("Démarrage de la boucle de vérification YouTube");
    let mut interval = tokio::time::interval(Duration::from_secs(600));
    loop {
        interval.tick().await;
        if api_key().is_none() {
            continue;
        }
        if let Err(e) = check_once(&ctx, &data).await {
            tracing::error!("[YouTube] Erreur lors de la vérification: {e:#}");
        }
    }
}

async fn check_once(ctx: &serenity::Context, data: &Data) -> Result<(), Error> {
    let channels: Vec<Watched> = data
        .db
        .run(|c| {
            Ok(c.prepare(
                "SELECT id, channelId, channelName, discordChannelId, roleId, lastVideoId, lastShortId,
                        COALESCE(notifyVideos, 1), COALESCE(notifyShorts, 1)
                 FROM youtube_channels",
            )?
            .query_map([], |r| {
                Ok(Watched {
                    id: r.get(0)?,
                    channel_id: r.get(1)?,
                    name: r.get(2)?,
                    discord_channel: r.get::<_, String>(3)?.parse().unwrap_or(0),
                    role: r.get(4)?,
                    last_video: r.get(5)?,
                    last_short: r.get(6)?,
                    notify_videos: r.get::<_, i64>(7)? != 0,
                    notify_shorts: r.get::<_, i64>(8)? != 0,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
        })
        .await?;
    tracing::debug!("[YouTube] Vérification de {} chaîne(s)", channels.len());

    for ch in channels {
        if ch.discord_channel == 0 || (!ch.notify_videos && !ch.notify_shorts) {
            continue;
        }
        if let Err(e) = check_channel(ctx, data, &ch).await {
            let msg = e.to_string();
            if msg.to_lowercase().contains("quota") || msg.contains("403") {
                tracing::error!(
                    "[YouTube] QUOTA API DÉPASSÉ ! Le quota se réinitialise à minuit PST. Erreur: {msg}"
                );
            } else {
                tracing::error!(
                    "Erreur lors de la vérification des uploads pour {}: {e:#}",
                    ch.name
                );
            }
        }
    }
    Ok(())
}

async fn check_channel(ctx: &serenity::Context, data: &Data, ch: &Watched) -> Result<(), Error> {
    let uploads = latest_uploads(&data.http, &ch.channel_id, 3).await?;
    let ids: Vec<String> = uploads.iter().filter_map(video_id).collect();
    let details = video_details(&data.http, &ids).await;

    let (mut new_video, mut new_short) = (None::<Found>, None::<Found>);
    let (mut recent_video, mut recent_short) = (ch.last_video.clone(), ch.last_short.clone());
    let (mut seen_video, mut seen_short) = (false, false);

    // Les éléments arrivent du plus récent au plus ancien.
    for upload in &uploads {
        let Some(id) = video_id(upload) else { continue };
        if !is_recent(
            upload["snippet"]["publishedAt"]
                .as_str()
                .unwrap_or_default(),
            24,
        ) {
            break;
        }
        let Some(d) = details.get(&id) else { continue };
        let found = Found {
            id: id.clone(),
            title: d["snippet"]["title"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            thumbnail: d["snippet"]["thumbnails"]["high"]["url"]
                .as_str()
                .map(str::to_string),
        };
        let short = is_short(d["contentDetails"]["duration"].as_str().unwrap_or_default());
        let (last, seen, recent, newest, notify) = if short {
            (
                &ch.last_short,
                &mut seen_short,
                &mut recent_short,
                &mut new_short,
                ch.notify_shorts,
            )
        } else {
            (
                &ch.last_video,
                &mut seen_video,
                &mut recent_video,
                &mut new_video,
                ch.notify_videos,
            )
        };
        if last.as_deref() == Some(id.as_str()) {
            *seen = true;
            continue;
        }
        if *seen || !notify {
            continue;
        }
        if recent == last {
            *recent = Some(id.clone());
        }
        if newest.is_none() {
            *newest = Some(found);
        }
    }

    if recent_video != ch.last_video || recent_short != ch.last_short {
        let (id, rv, rs) = (ch.id, recent_video.clone(), recent_short.clone());
        data.db
            .run(move |c| {
                c.execute(
                    "UPDATE youtube_channels SET lastVideoId = ?1, lastShortId = ?2 WHERE id = ?3",
                    params![rv, rs, id],
                )?;
                Ok(())
            })
            .await?;
        tracing::info!(
            "IDs mis à jour pour {}: lastVideoId={recent_video:?}, lastShortId={recent_short:?}",
            ch.name
        );
    }

    for (found, short) in [(new_short, true), (new_video, false)] {
        let Some(f) = found else { continue };
        let (title_key, desc_key, url, color) = if short {
            (
                "youtube.new_short_title",
                "youtube.new_short_desc",
                format!("https://www.youtube.com/shorts/{}", f.id),
                colors::ORANGE,
            )
        } else {
            (
                "youtube.new_video_title",
                "youtube.new_video_desc",
                format!("https://www.youtube.com/watch?v={}", f.id),
                colors::RED,
            )
        };
        let mut e = serenity::CreateEmbed::new()
            .title(tr!(title_key, None, title = f.title))
            .description(tr!(desc_key, None, name = &ch.name, url = url))
            .colour(color);
        if let Some(t) = f.thumbnail {
            e = e.image(t);
        }
        let mut msg = serenity::CreateMessage::new().embed(e);
        if let Some(r) = ch.role.as_deref().filter(|r| !r.is_empty()) {
            msg = msg.content(format!("<@&{r}>"));
        }
        match serenity::ChannelId::new(ch.discord_channel)
            .send_message(ctx, msg)
            .await
        {
            Ok(_) => tracing::info!(
                "Annonce {} envoyée pour {}",
                if short { "short" } else { "vidéo" },
                ch.name
            ),
            Err(e) => tracing::error!("Erreur lors de l'annonce pour {}: {e}", ch.name),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shorts_detection() {
        assert!(is_short("PT45S"));
        assert!(is_short("PT1M"));
        assert!(!is_short("PT1M1S"));
        assert!(!is_short("PT1H"));
        assert!(!is_short("P1D"));
    }

    #[test]
    fn identifiers() {
        assert!(validate_identifier("@el-dorado-community", None).is_ok());
        assert!(validate_identifier("@", None).is_err());
        assert!(validate_identifier("@bad handle", None).is_err());
        assert!(validate_identifier("UCaaaaaaaaaaaaaaaaaaaaaa", None).is_ok());
        assert!(validate_identifier("UCshort", None).is_err());
    }

    #[test]
    fn recency() {
        let now = Utc::now().to_rfc3339();
        assert!(is_recent(&now, 24));
        assert!(!is_recent("2020-01-01T00:00:00Z", 24));
        assert!(!is_recent("garbage", 24));
    }
}

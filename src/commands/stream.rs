//! Notifications Twitch : /stream_add, /stream_remove, /stream_list et vérification périodique.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use poise::CreateReply;
use poise::serenity_prelude as serenity;
use rusqlite::params;
use serde_json::Value;

use crate::bot::{Context, Data, Error, colors, reply_ephemeral};
use crate::tr;

fn credentials() -> Option<(String, String)> {
    let id = std::env::var("twitch_client_id")
        .ok()
        .filter(|v| !v.trim().is_empty())?;
    let secret = std::env::var("twitch_client_secret")
        .ok()
        .filter(|v| !v.trim().is_empty())?;
    Some((id, secret))
}

pub fn is_valid_name(name: &str) -> bool {
    (4..=25).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Ajouter un streamer Twitch à annoncer.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn stream_add(
    ctx: Context<'_>,
    streamer_name: String,
    #[channel_types("Text")] channel: serenity::GuildChannel,
    ping_role: Option<serenity::Role>,
) -> Result<(), Error> {
    let loc = ctx.locale();
    let name = streamer_name.trim().to_lowercase();
    if name.is_empty() {
        return reply_ephemeral(ctx, tr!("stream.empty_name", loc)).await;
    }
    if !is_valid_name(&name) {
        return reply_ephemeral(ctx, tr!("stream.invalid_name", loc)).await;
    }
    if credentials().is_none() {
        return reply_ephemeral(ctx, tr!("stream.no_creds", loc)).await;
    }
    let (n, ch, role) = (
        name.clone(),
        channel.id.to_string(),
        ping_role.as_ref().map(|r| r.id.to_string()),
    );
    let added = ctx
        .data()
        .db
        .run(move |c| {
            let exists = c
                .prepare(
                    "SELECT 1 FROM streamers WHERE streamerName = ?1 AND streamChannelId = ?2",
                )?
                .exists(params![n, ch])?;
            if exists {
                return Ok(false);
            }
            c.execute(
                "INSERT INTO streamers (streamerName, streamChannelId, roleId) VALUES (?1, ?2, ?3)",
                params![n, ch, role],
            )?;
            Ok(true)
        })
        .await;
    match added {
        Ok(false) => reply_ephemeral(ctx, tr!("stream.already", loc, name = name)).await,
        Ok(true) => {
            ctx.say(tr!(
                "stream.added",
                loc,
                name = name,
                channel = format!("<#{}>", channel.id)
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
        Err(e) => {
            tracing::error!("Erreur lors de l'ajout du streamer {name}: {e:#}");
            reply_ephemeral(ctx, tr!("stream.add_error", loc, error = e.to_string())).await
        }
    }
}

/// Retirer un streamer de la liste des streamers.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn stream_remove(ctx: Context<'_>, streamer_name: String) -> Result<(), Error> {
    let loc = ctx.locale();
    let name = streamer_name.trim().to_lowercase();
    if name.is_empty() {
        return reply_ephemeral(ctx, tr!("stream.empty_name", loc)).await;
    }
    let n = name.clone();
    match ctx
        .data()
        .db
        .run(move |c| Ok(c.execute("DELETE FROM streamers WHERE streamerName = ?1", [n])?))
        .await
    {
        Ok(0) => reply_ephemeral(ctx, tr!("stream.not_found", loc, name = name)).await,
        Ok(_) => {
            ctx.say(tr!("stream.removed", loc, name = name)).await?;
            Ok(())
        }
        Err(e) => {
            reply_ephemeral(ctx, tr!("stream.remove_error", loc, error = e.to_string())).await
        }
    }
}

/// Lister les streamers suivis.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn stream_list(ctx: Context<'_>) -> Result<(), Error> {
    let loc = ctx.locale();
    let rows: Vec<(String, Option<String>, Option<String>, i64)> = match ctx
        .data()
        .db
        .run(|c| {
            Ok(c.prepare(
                "SELECT streamerName, streamChannelId, roleId, COALESCE(announced, 0)
                 FROM streamers ORDER BY streamerName ASC",
            )?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?)
        })
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return reply_ephemeral(ctx, tr!("stream.list_error", loc, error = e.to_string())).await;
        }
    };
    if rows.is_empty() {
        return reply_ephemeral(
            ctx,
            format!(
                "ℹ️ {}: {}",
                tr!("stream.list_title", loc),
                tr!("stream.chan_none", loc)
            ),
        )
        .await;
    }
    let mut e = serenity::CreateEmbed::new()
        .title(tr!("stream.list_title", loc))
        .colour(colors::PURPLE);
    for (name, channel, role, announced) in rows.into_iter().take(25) {
        let channel = channel
            .filter(|c| !c.is_empty())
            .map(|c| format!("<#{c}>"))
            .unwrap_or_else(|| tr!("stream.chan_none", loc));
        let role = role
            .filter(|r| !r.is_empty())
            .map(|r| format!("<@&{r}>"))
            .unwrap_or_else(|| tr!("stream.role_none", loc));
        let status = tr!(
            if announced != 0 {
                "stream.live"
            } else {
                "stream.offline"
            },
            loc
        );
        e = e.field(
            name,
            tr!(
                "stream.field_value",
                loc,
                channel = channel,
                role = role,
                status = status
            ),
            false,
        );
    }
    ctx.send(CreateReply::default().embed(e).ephemeral(true))
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// API Twitch
// ---------------------------------------------------------------------------

/// Jeton d'application Twitch mis en cache jusqu'à son expiration.
static TOKEN: Mutex<Option<(String, Instant)>> = Mutex::new(None);

async fn with_retry<T, F, Fut>(what: &str, mut f: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, (bool, anyhow::Error)>>,
{
    let mut last = None;
    for attempt in 0..3u32 {
        match f().await {
            Ok(v) => return Ok(v),
            // (retryable, erreur)
            Err((false, e)) => return Err(e),
            Err((true, e)) => {
                if attempt < 2 {
                    tracing::warn!("Tentative {}/3 échouée pour {what}: {e}", attempt + 1);
                    tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
                }
                last = Some(e);
            }
        }
    }
    Err(anyhow::anyhow!(
        "Échec pour {what} après 3 tentatives: {}",
        last.map(|e| e.to_string()).unwrap_or_default()
    ))
}

async fn auth_token(http: &reqwest::Client) -> anyhow::Result<String> {
    if let Some((t, exp)) = TOKEN.lock().unwrap().clone() {
        if Instant::now() < exp {
            return Ok(t);
        }
    }
    let (id, secret) = credentials()
        .ok_or_else(|| anyhow::anyhow!("Les identifiants Twitch ne sont pas configurés."))?;
    let data: Value = with_retry("l'authentification Twitch", || async {
        let resp = http
            .post("https://id.twitch.tv/oauth2/token")
            .query(&[
                ("client_id", &id),
                ("client_secret", &secret),
                ("grant_type", &"client_credentials".to_string()),
            ])
            .send()
            .await
            .map_err(|e| (true, e.into()))?;
        let status = resp.status();
        if status.is_server_error() {
            return Err((true, anyhow::anyhow!("Erreur serveur Twitch: {status}")));
        }
        if !status.is_success() {
            return Err((
                false,
                anyhow::anyhow!("Erreur lors de la récupération du token OAuth: {status}"),
            ));
        }
        resp.json::<Value>().await.map_err(|e| (true, e.into()))
    })
    .await?;
    let token = data["access_token"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Réponse OAuth Twitch invalide"))?
        .to_string();
    let expires_in = data["expires_in"].as_u64().unwrap_or(3600);
    *TOKEN.lock().unwrap() = Some((
        token.clone(),
        Instant::now() + Duration::from_secs(expires_in.saturating_sub(60).max(60)),
    ));
    Ok(token)
}

/// Renvoie les infos du stream en cours (`None` si hors ligne).
async fn live_stream(http: &reqwest::Client, login: &str) -> anyhow::Result<Option<Value>> {
    let (client_id, _) =
        credentials().ok_or_else(|| anyhow::anyhow!("Identifiants Twitch absents"))?;
    let token = auth_token(http).await?;
    let data: Value = with_retry(login, || async {
        let resp = http
            .get("https://api.twitch.tv/helix/streams")
            .query(&[("user_login", login)])
            .header("Client-ID", &client_id)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|e| (true, e.into()))?;
        let status = resp.status();
        if status.is_server_error() {
            return Err((true, anyhow::anyhow!("Erreur serveur Twitch: {status}")));
        }
        if status.as_u16() == 401 {
            // Jeton révoqué : on l'oublie pour le prochain cycle.
            *TOKEN.lock().unwrap() = None;
        }
        if !status.is_success() {
            return Err((
                false,
                anyhow::anyhow!("Erreur lors de la vérification du statut du streamer: {status}"),
            ));
        }
        resp.json::<Value>().await.map_err(|e| (true, e.into()))
    })
    .await?;
    Ok(data["data"].as_array().and_then(|a| a.first()).cloned())
}

fn announce_embed(name: &str, title: &str, category: &str) -> serenity::CreateEmbed {
    serenity::CreateEmbed::new()
        .title(title)
        .description(tr!(
            "stream.announce_desc",
            None,
            name = name,
            category = category,
            url = format!("https://www.twitch.tv/{name}")
        ))
        .colour(colors::PURPLE)
        .image(format!(
            "https://static-cdn.jtvnw.net/previews-ttv/live_user_{name}-1920x1080.jpg"
        ))
}

/// Toutes les 5 minutes : annonce des streamers passés en ligne.
pub async fn check_loop(ctx: serenity::Context, data: Data) {
    tracing::info!("Démarrage de la boucle de vérification Twitch");
    let mut interval = tokio::time::interval(Duration::from_secs(300));
    loop {
        interval.tick().await;
        if credentials().is_none() {
            continue;
        }
        if let Err(e) = check_once(&ctx, &data).await {
            tracing::error!("Erreur lors de la vérification des streams: {e:#}");
        }
    }
}

async fn check_once(ctx: &serenity::Context, data: &Data) -> Result<(), Error> {
    let streamers: Vec<(i64, String, Option<String>, Option<String>, i64)> = data
        .db
        .run(|c| {
            Ok(c.prepare("SELECT id, streamerName, streamChannelId, roleId, COALESCE(announced, 0) FROM streamers")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await?;
    tracing::debug!("[Twitch] Vérification de {} streamer(s)", streamers.len());

    for (id, name, channel, role, announced) in streamers {
        let live = match live_stream(&data.http, &name).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("Erreur lors de la vérification du streamer {name}: {e:#}");
                continue;
            }
        };
        match (live, announced) {
            (Some(info), 0) => {
                let Some(ch) = channel.and_then(|c| c.parse::<u64>().ok()) else {
                    continue;
                };
                let title = info["title"].as_str().unwrap_or("Stream en direct");
                let category = info["game_name"].as_str().unwrap_or("Inconnu");
                let mut msg =
                    serenity::CreateMessage::new().embed(announce_embed(&name, title, category));
                if let Some(r) = role.filter(|r| !r.is_empty()) {
                    msg = msg.content(format!("<@&{r}>"));
                }
                if let Err(e) = serenity::ChannelId::new(ch).send_message(ctx, msg).await {
                    tracing::error!("Erreur lors de l'annonce du stream de {name}: {e}");
                    continue;
                }
                data.db
                    .run(move |c| {
                        Ok(c.execute("UPDATE streamers SET announced = 1 WHERE id = ?1", [id])?)
                    })
                    .await?;
                tracing::info!("Annonce envoyée pour le streamer {name}");
            }
            (None, a) if a != 0 => {
                data.db
                    .run(move |c| {
                        Ok(c.execute("UPDATE streamers SET announced = 0 WHERE id = ?1", [id])?)
                    })
                    .await?;
                tracing::debug!("Statut réinitialisé pour le streamer {name}");
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn twitch_names() {
        assert!(super::is_valid_name("some_streamer"));
        assert!(!super::is_valid_name("abc"));
        assert!(!super::is_valid_name("bad name"));
    }
}

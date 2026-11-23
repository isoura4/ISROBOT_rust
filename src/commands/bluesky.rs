//! Notifications Bluesky : /bluesky_add, /bluesky_remove, /bluesky_list et vérification périodique.
//! Utilise l'API publique (AppView), sans authentification.

use std::sync::LazyLock;
use std::time::Duration;

use poise::CreateReply;
use poise::serenity_prelude as serenity;
use regex::Regex;
use rusqlite::params;
use serde_json::Value;

use crate::bot::{Context, Data, Error, colors, reply_ephemeral};
use crate::tr;

/// Nombre maximal de posts annoncés par cycle et par compte (anti-spam).
pub const MAX_ANNOUNCE_PER_CYCLE: usize = 5;

static HANDLE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?(?:\.[a-zA-Z0-9](?:[a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?)+$").unwrap()
});
static DID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^did:(?:plc|web):[a-zA-Z0-9._:%-]+$").unwrap());

fn api_base() -> String {
    std::env::var("bluesky_public_api")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "https://public.api.bsky.app".into())
        .trim_end_matches('/')
        .to_string()
}

pub fn normalize(identifier: &str) -> String {
    identifier.trim().trim_start_matches('@').trim().to_string()
}

pub fn validate(identifier: &str, loc: Option<&str>) -> Result<(), String> {
    let n = normalize(identifier);
    if n.is_empty() {
        Err(tr!("bluesky.empty_input", loc))
    } else if DID_RE.is_match(&n) || HANDLE_RE.is_match(&n) {
        Ok(())
    } else {
        Err(tr!("bluesky.invalid_handle", loc))
    }
}

pub fn post_web_url(handle: &str, uri: &str) -> String {
    let rkey = uri
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    format!("https://bsky.app/profile/{handle}/post/{rkey}")
}

pub fn post_text(post: &Value) -> String {
    post["record"]["text"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string()
}

pub fn post_image(post: &Value) -> Option<String> {
    let embed = &post["embed"];
    let from_images = |imgs: &Value| {
        imgs.as_array()
            .and_then(|a| a.first())
            .and_then(|i| i["fullsize"].as_str().or_else(|| i["thumb"].as_str()))
            .map(str::to_string)
    };
    from_images(&embed["images"])
        .or_else(|| embed["external"]["thumb"].as_str().map(str::to_string))
        .or_else(|| from_images(&embed["media"]["images"]))
}

pub fn post_created_at(post: &Value) -> Option<String> {
    post["record"]["createdAt"]
        .as_str()
        .or_else(|| post["indexedAt"].as_str())
        .map(str::to_string)
}

/// Nouveaux posts depuis `last_uri` (exclu), sans les reposts, dans l'ordre chronologique.
pub fn select_new_posts(feed: &[Value], last_uri: Option<&str>) -> Vec<Value> {
    let mut out = Vec::new();
    for item in feed {
        let post = &item["post"];
        if last_uri.is_some() && post["uri"].as_str() == last_uri {
            break;
        }
        if !item["reason"].is_null() {
            continue;
        }
        out.push(post.clone());
    }
    out.reverse();
    out
}

async fn get(
    http: &reqwest::Client,
    method: &str,
    query: &[(&str, &str)],
) -> anyhow::Result<Option<Value>> {
    let resp = http
        .get(format!("{}/xrpc/{method}", api_base()))
        .query(query)
        .send()
        .await?;
    match resp.status().as_u16() {
        200 => Ok(Some(resp.json().await?)),
        400 | 404 => Ok(None),
        s => anyhow::bail!("Erreur API Bluesky ({method}): {s}"),
    }
}

async fn profile(http: &reqwest::Client, actor: &str) -> anyhow::Result<Option<Value>> {
    get(http, "app.bsky.actor.getProfile", &[("actor", actor)]).await
}

async fn author_feed(http: &reqwest::Client, actor: &str, limit: u8) -> anyhow::Result<Vec<Value>> {
    let limit = limit.to_string();
    Ok(get(
        http,
        "app.bsky.feed.getAuthorFeed",
        &[
            ("actor", actor),
            ("limit", &limit),
            ("filter", "posts_no_replies"),
        ],
    )
    .await?
    .and_then(|v| v["feed"].as_array().cloned())
    .unwrap_or_default())
}

/// Suivre un compte Bluesky.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn bluesky_add(
    ctx: Context<'_>,
    account: String,
    #[channel_types("Text")] channel: serenity::GuildChannel,
    ping_role: Option<serenity::Role>,
) -> Result<(), Error> {
    let loc = ctx.locale();
    if let Err(msg) = validate(&account, loc) {
        return reply_ephemeral(ctx, msg).await;
    }
    let handle = normalize(&account);
    ctx.defer().await?;
    let http = &ctx.data().http;

    let init = async {
        let Some(p) = profile(http, &handle).await? else {
            return anyhow::Ok(None);
        };
        let actual = p["handle"].as_str().unwrap_or(&handle).to_string();
        let did = p["did"].as_str().map(str::to_string);
        let display = p["displayName"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(&actual)
            .to_string();
        let feed = author_feed(http, did.as_deref().unwrap_or(&actual), 1).await?;
        let last = feed.first().map(|i| {
            (
                i["post"]["uri"].as_str().map(str::to_string),
                i["post"]["cid"].as_str().map(str::to_string),
            )
        });
        Ok(Some((actual, did, display, last.unwrap_or((None, None)))))
    };
    let (actual, did, display, (last_uri, last_cid)) = match init.await {
        Ok(Some(v)) => v,
        Ok(None) => {
            ctx.say(tr!("bluesky.not_found", loc, account = handle))
                .await?;
            return Ok(());
        }
        Err(e) => {
            tracing::warn!("Impossible d'initialiser le compte Bluesky {handle}: {e:#}");
            ctx.say(tr!("bluesky.check_error", loc, error = e.to_string()))
                .await?;
            return Ok(());
        }
    };

    let tracking = tr!(
        if last_uri.is_some() {
            "bluesky.tracking_init"
        } else {
            "bluesky.tracking_none"
        },
        loc
    );
    let (h, ch, role, dn) = (
        actual.clone(),
        channel.id.to_string(),
        ping_role.as_ref().map(|r| r.id.to_string()),
        display.clone(),
    );
    let added = ctx
        .data()
        .db
        .run(move |c| {
            let exists = c
                .prepare(
                    "SELECT 1 FROM bluesky_accounts WHERE handle = ?1 AND discordChannelId = ?2",
                )?
                .exists(params![h, ch])?;
            if exists {
                return Ok(false);
            }
            c.execute(
                "INSERT INTO bluesky_accounts (handle, did, displayName, discordChannelId, roleId,
                    lastPostUri, lastPostCid, createdAt)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    h,
                    did,
                    dn,
                    ch,
                    role,
                    last_uri,
                    last_cid,
                    crate::db::utcnow_iso()
                ],
            )?;
            Ok(true)
        })
        .await?;
    if !added {
        ctx.say(tr!("bluesky.already", loc, name = display)).await?;
        return Ok(());
    }
    ctx.say(tr!(
        "bluesky.added",
        loc,
        name = display,
        channel = format!("<#{}>", channel.id),
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

struct Account {
    id: i64,
    handle: String,
    did: Option<String>,
    display_name: Option<String>,
    channel: String,
    role: Option<String>,
    last_uri: Option<String>,
}

fn list_accounts(c: &rusqlite::Connection) -> rusqlite::Result<Vec<Account>> {
    c.prepare(
        "SELECT id, handle, did, displayName, discordChannelId, roleId, lastPostUri
         FROM bluesky_accounts ORDER BY handle ASC",
    )?
    .query_map([], |r| {
        Ok(Account {
            id: r.get(0)?,
            handle: r.get(1)?,
            did: r.get(2)?,
            display_name: r.get(3)?,
            channel: r.get(4)?,
            role: r.get(5)?,
            last_uri: r.get(6)?,
        })
    })?
    .collect()
}

/// Ne plus suivre un compte Bluesky.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn bluesky_remove(ctx: Context<'_>, account: String) -> Result<(), Error> {
    let loc = ctx.locale();
    let handle = normalize(&account);
    if handle.is_empty() {
        let accounts = ctx.data().db.run(|c| Ok(list_accounts(c)?)).await?;
        let msg = if accounts.is_empty() {
            tr!("bluesky.remove_none", loc)
        } else {
            let list = accounts
                .iter()
                .map(|a| a.handle.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            tr!("bluesky.remove_specify", loc, list = list)
        };
        ctx.say(msg).await?;
        return Ok(());
    }
    let h = handle.clone();
    let rows = ctx
        .data()
        .db
        .run(move |c| Ok(c.execute("DELETE FROM bluesky_accounts WHERE handle = ?1", [h])?))
        .await?;
    let key = if rows > 0 {
        "bluesky.removed"
    } else {
        "bluesky.remove_not_found"
    };
    ctx.say(tr!(key, loc, name = handle)).await?;
    Ok(())
}

/// Afficher la liste des comptes Bluesky suivis.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn bluesky_list(ctx: Context<'_>) -> Result<(), Error> {
    let loc = ctx.locale();
    let accounts = ctx.data().db.run(|c| Ok(list_accounts(c)?)).await?;
    if accounts.is_empty() {
        return reply_ephemeral(
            ctx,
            format!(
                "ℹ️ {}: {}",
                tr!("bluesky.list_title", loc),
                tr!("bluesky.chan_none", loc)
            ),
        )
        .await;
    }
    let mut e = serenity::CreateEmbed::new()
        .title(tr!("bluesky.list_title", loc))
        .colour(colors::BLUESKY);
    for a in accounts.iter().take(25) {
        let role = a
            .role
            .as_deref()
            .filter(|r| !r.is_empty())
            .map(|r| format!("<@&{r}>"))
            .unwrap_or_else(|| tr!("bluesky.role_none", loc));
        let channel = if a.channel.is_empty() {
            tr!("bluesky.chan_none", loc)
        } else {
            format!("<#{}>", a.channel)
        };
        e = e.field(
            a.display_name
                .clone()
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| a.handle.clone()),
            tr!(
                "bluesky.field_value",
                loc,
                handle = &a.handle,
                channel = channel,
                role = role
            ),
            false,
        );
    }
    ctx.send(CreateReply::default().embed(e).ephemeral(true))
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Vérification périodique
// ---------------------------------------------------------------------------

fn post_embed(handle: &str, display: Option<&str>, post: &Value) -> serenity::CreateEmbed {
    let url = post_web_url(handle, post["uri"].as_str().unwrap_or_default());
    let text = post_text(post);
    let text = if text.is_empty() {
        tr!("bluesky.no_text", None)
    } else {
        text
    };
    let author = display.filter(|d| !d.is_empty()).unwrap_or(handle);
    let mut e = serenity::CreateEmbed::new()
        .title(crate::bot::truncate(
            &tr!("bluesky.new_post_title", None, author = author),
            256,
        ))
        .description(crate::bot::truncate(
            &tr!(
                "bluesky.new_post_desc",
                None,
                text = text,
                url = url.clone()
            ),
            4096,
        ))
        .url(url)
        .colour(colors::BLUESKY)
        .footer(serenity::CreateEmbedFooter::new(tr!(
            "bluesky.footer",
            None,
            handle = handle
        )));
    if let Some(img) = post_image(post) {
        e = e.image(img);
    }
    if let Some(ts) = post_created_at(post).and_then(|s| serenity::Timestamp::parse(&s).ok()) {
        e = e.timestamp(ts);
    }
    e
}

/// Toutes les 5 minutes : nouveaux posts des comptes suivis.
pub async fn check_loop(ctx: serenity::Context, data: Data) {
    tracing::info!("Démarrage de la boucle de vérification Bluesky");
    let mut interval = tokio::time::interval(Duration::from_secs(300));
    loop {
        interval.tick().await;
        if let Err(e) = check_once(&ctx, &data).await {
            tracing::error!("[Bluesky] Erreur lors de la vérification: {e:#}");
        }
    }
}

async fn update_last(
    data: &Data,
    id: i64,
    uri: Option<String>,
    cid: Option<String>,
) -> Result<(), Error> {
    data.db
        .run(move |c| {
            c.execute(
                "UPDATE bluesky_accounts SET lastPostUri = ?1, lastPostCid = ?2 WHERE id = ?3",
                params![uri, cid, id],
            )?;
            Ok(())
        })
        .await
}

async fn check_once(ctx: &serenity::Context, data: &Data) -> Result<(), Error> {
    let accounts = data.db.run(|c| Ok(list_accounts(c)?)).await?;
    tracing::debug!("[Bluesky] Vérification de {} compte(s)", accounts.len());
    for a in accounts {
        let Ok(channel) = a.channel.parse::<u64>() else {
            continue;
        };
        let actor = a
            .did
            .clone()
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| a.handle.clone());
        let feed = match author_feed(&data.http, &actor, 20).await {
            Ok(f) if !f.is_empty() => f,
            Ok(_) => continue,
            Err(e) => {
                tracing::error!(
                    "Erreur lors de la vérification Bluesky de {}: {e:#}",
                    a.handle
                );
                continue;
            }
        };
        let newest = &feed[0]["post"];
        // Premier cycle : on initialise sans annoncer l'ancien contenu.
        let Some(last) = a.last_uri.as_deref().filter(|s| !s.is_empty()) else {
            update_last(
                data,
                a.id,
                newest["uri"].as_str().map(str::to_string),
                newest["cid"].as_str().map(str::to_string),
            )
            .await?;
            continue;
        };
        let posts = select_new_posts(&feed, Some(last));
        let skip = posts.len().saturating_sub(MAX_ANNOUNCE_PER_CYCLE);
        // Le marqueur avance après chaque envoi réussi : pas de doublon, pas de perte.
        for post in &posts[skip..] {
            let mut msg = serenity::CreateMessage::new().embed(post_embed(
                &a.handle,
                a.display_name.as_deref(),
                post,
            ));
            if let Some(r) = a.role.as_deref().filter(|r| !r.is_empty()) {
                msg = msg.content(format!("<@&{r}>"));
            }
            if let Err(e) = serenity::ChannelId::new(channel)
                .send_message(ctx, msg)
                .await
            {
                tracing::error!(
                    "Échec de l'annonce Bluesky de {} (post {}): {e}",
                    a.handle,
                    post["uri"]
                );
                break;
            }
            update_last(
                data,
                a.id,
                post["uri"].as_str().map(str::to_string),
                post["cid"].as_str().map(str::to_string),
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn identifiers() {
        assert_eq!(normalize(" @alice.bsky.social "), "alice.bsky.social");
        assert!(validate("alice.bsky.social", None).is_ok());
        assert!(validate("did:plc:abc123", None).is_ok());
        assert!(validate("alice", None).is_err());
        assert!(validate("", None).is_err());
    }

    #[test]
    fn urls_and_extraction() {
        assert_eq!(
            post_web_url(
                "alice.bsky.social",
                "at://did:plc:x/app.bsky.feed.post/3kabc"
            ),
            "https://bsky.app/profile/alice.bsky.social/post/3kabc"
        );
        let post = json!({
            "record": {"text": "  hi  ", "createdAt": "2024-01-01T00:00:00Z"},
            "embed": {"external": {"thumb": "https://img"}}
        });
        assert_eq!(post_text(&post), "hi");
        assert_eq!(post_image(&post), Some("https://img".into()));
        assert_eq!(post_created_at(&post), Some("2024-01-01T00:00:00Z".into()));
    }

    #[test]
    fn new_posts_selection() {
        let feed = vec![
            json!({"post": {"uri": "c"}}),
            json!({"post": {"uri": "r"}, "reason": {"$type": "repost"}}),
            json!({"post": {"uri": "b"}}),
            json!({"post": {"uri": "a"}}),
        ];
        let uris: Vec<_> = select_new_posts(&feed, Some("a"))
            .iter()
            .map(|p| p["uri"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(uris, vec!["b", "c"]);
    }
}

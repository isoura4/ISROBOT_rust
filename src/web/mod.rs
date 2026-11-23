//! Interface web d'administration.
//!
//! - Connexion via OAuth2 Discord (scope `identify` uniquement).
//! - Accès réservé aux administrateurs (permission « Administrateur » ou
//!   propriétaire) du serveur `server_id` : la vérification est faite par le
//!   bot lui-même et répétée régulièrement, un administrateur rétrogradé perd
//!   donc l'accès sans avoir à se déconnecter.
//! - Sessions en mémoire (cookie `HttpOnly`, `SameSite=Lax`, `Secure` en HTTPS)
//!   et jeton anti-CSRF exigé pour toute modification.

mod api;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{FromRequestParts, Query, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use poise::serenity_prelude as serenity;
use serde::Deserialize;

use crate::bot::Data;
use crate::config::{self, Platform, env_bool};

const SESSION_COOKIE: &str = "isrobot_session";
const STATE_COOKIE: &str = "isrobot_oauth_state";
const SESSION_TTL: Duration = Duration::from_secs(12 * 3600);
/// Fréquence de re-vérification des droits administrateur.
const ADMIN_RECHECK: Duration = Duration::from_secs(60);

/// Configuration de l'interface web (variables `WEB_*` du `.env`).
#[derive(Clone, Debug)]
pub struct WebConfig {
    pub bind: SocketAddr,
    pub base_url: String,
    pub client_id: u64,
    pub client_secret: String,
    pub authorize_url: String,
    pub api_base: String,
}

impl WebConfig {
    /// `None` si l'interface est désactivée ou mal configurée (avec un message dans les logs).
    pub fn from_env(cfg: &crate::config::Config) -> Option<Self> {
        if !env_bool("WEB_ENABLED", false) {
            return None;
        }
        let bind = config::var("WEB_BIND").unwrap_or_else(|_| "127.0.0.1:8080".into());
        let Ok(bind) = bind.trim().parse::<SocketAddr>() else {
            tracing::error!("[Web] WEB_BIND invalide: {bind}");
            return None;
        };
        let secret = config::var("WEB_OAUTH_CLIENT_SECRET")
            .unwrap_or_default()
            .trim()
            .to_string();
        if secret.is_empty() || secret.starts_with("YOUR_") {
            tracing::error!("[Web] WEB_OAUTH_CLIENT_SECRET manquant : interface web désactivée");
            return None;
        }
        let base_url = config::var("WEB_BASE_URL")
            .unwrap_or_else(|_| format!("http://{bind}"))
            .trim()
            .trim_end_matches('/')
            .to_string();
        let (authorize_url, api_base) = match cfg.platform {
            Platform::Discord => (
                "https://discord.com/oauth2/authorize".to_string(),
                "https://discord.com/api/v10".to_string(),
            ),
            Platform::GameVox => (
                "https://gamevox.com/oauth2/authorize".to_string(),
                format!("{}/api/v10", crate::config::GAMEVOX_API_BASE),
            ),
        };
        Some(Self {
            bind,
            base_url,
            client_id: cfg.app_id,
            client_secret: secret,
            authorize_url: config::var("WEB_OAUTH_AUTHORIZE_URL").unwrap_or(authorize_url),
            api_base: config::var("WEB_OAUTH_API_BASE").unwrap_or(api_base),
        })
    }

    fn redirect_uri(&self) -> String {
        format!("{}/auth/callback", self.base_url)
    }

    fn secure_cookies(&self) -> bool {
        self.base_url.starts_with("https://")
    }
}

#[derive(Clone, Debug)]
pub struct Session {
    pub user_id: u64,
    pub username: String,
    pub avatar: Option<String>,
    pub csrf: String,
    expires: Instant,
    admin_checked: Option<(Instant, bool)>,
}

pub struct WebState {
    pub data: Data,
    pub ctx: serenity::Context,
    pub cfg: WebConfig,
    sessions: Mutex<HashMap<String, Session>>,
}

impl WebState {
    pub fn guild(&self) -> serenity::GuildId {
        serenity::GuildId::new(self.data.config.server_id)
    }
}

fn random_token() -> String {
    hex::encode(rand::random::<[u8; 32]>())
}

/// Démarre le serveur web s'il est activé dans le `.env`.
pub fn spawn(ctx: serenity::Context, data: Data) {
    let Some(cfg) = WebConfig::from_env(&data.config) else {
        return;
    };
    let bind = cfg.bind;
    let base = cfg.base_url.clone();
    let state = Arc::new(WebState {
        data,
        ctx,
        cfg,
        sessions: Mutex::default(),
    });
    tokio::spawn(async move {
        let app = router(state);
        match tokio::net::TcpListener::bind(bind).await {
            Ok(listener) => {
                tracing::info!(
                    "[Web] Interface d'administration disponible sur {base} (écoute {bind})"
                );
                if let Err(e) = axum::serve(listener, app).await {
                    tracing::error!("[Web] Arrêt du serveur web: {e}");
                }
            }
            Err(e) => tracing::error!("[Web] Impossible d'écouter sur {bind}: {e}"),
        }
    });
}

fn router(state: Arc<WebState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/auth/login", get(login))
        .route("/auth/callback", get(callback))
        .route("/auth/logout", post(logout))
        .nest("/api", api::routes())
        .with_state(state)
}

/// En-têtes de sécurité communs aux pages HTML.
fn security_headers() -> [(header::HeaderName, &'static str); 4] {
    [
        (
            header::CONTENT_SECURITY_POLICY,
            "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' https://cdn.discordapp.com data:; frame-ancestors 'none'",
        ),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::REFERRER_POLICY, "same-origin"),
    ]
}

async fn index() -> impl IntoResponse {
    (security_headers(), Html(include_str!("index.html")))
}

fn cookie(
    cfg: &WebConfig,
    name: &'static str,
    value: String,
    max_age: Duration,
) -> Cookie<'static> {
    Cookie::build((name, value))
        .path("/")
        .http_only(true)
        .secure(cfg.secure_cookies())
        .same_site(SameSite::Lax)
        .max_age(cookie::time::Duration::seconds(max_age.as_secs() as i64))
        .build()
}

async fn login(State(st): State<Arc<WebState>>, jar: CookieJar) -> impl IntoResponse {
    let state = random_token();
    let url = reqwest::Url::parse_with_params(
        &st.cfg.authorize_url,
        &[
            ("client_id", st.cfg.client_id.to_string()),
            ("redirect_uri", st.cfg.redirect_uri()),
            ("response_type", "code".into()),
            ("scope", "identify".into()),
            ("state", state.clone()),
            ("prompt", "none".into()),
        ],
    )
    .map(|u| u.to_string())
    .unwrap_or_else(|_| "/".into());
    let jar = jar.add(cookie(
        &st.cfg,
        STATE_COOKIE,
        state,
        Duration::from_secs(600),
    ));
    (jar, Redirect::to(&url))
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

fn error_page(status: StatusCode, message: &str) -> Response {
    let body = format!(
        "<!doctype html><html lang=\"fr\"><meta charset=\"utf-8\"><title>ISROBOT</title>\
         <body style=\"font-family:system-ui;max-width:32rem;margin:4rem auto;padding:0 1rem\">\
         <h1>Connexion impossible</h1><p>{}</p><p><a href=\"/\">Retour</a></p></body></html>",
        html_escape(message)
    );
    (status, security_headers(), Html(body)).into_response()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

#[derive(Deserialize)]
struct DiscordUser {
    id: String,
    username: String,
    global_name: Option<String>,
    avatar: Option<String>,
}

async fn callback(
    State(st): State<Arc<WebState>>,
    jar: CookieJar,
    Query(q): Query<CallbackQuery>,
) -> Response {
    if let Some(e) = q.error {
        return error_page(
            StatusCode::BAD_REQUEST,
            &format!("Autorisation refusée ({e})."),
        );
    }
    let expected = jar.get(STATE_COOKIE).map(|c| c.value().to_string());
    let jar = jar.remove(Cookie::build(STATE_COOKIE).path("/").build());
    let (Some(code), Some(state)) = (q.code, q.state) else {
        return error_page(StatusCode::BAD_REQUEST, "Requête OAuth incomplète.");
    };
    if expected.as_deref() != Some(state.as_str()) {
        return error_page(
            StatusCode::BAD_REQUEST,
            "Jeton de sécurité OAuth invalide ou expiré. Réessayez.",
        );
    }

    let user = match exchange_code(&st, &code).await {
        Ok(u) => u,
        Err(e) => {
            tracing::warn!("[Web] Échec de l'authentification OAuth: {e:#}");
            return error_page(
                StatusCode::BAD_GATEWAY,
                "Échec de l'authentification auprès de Discord.",
            );
        }
    };
    let Ok(user_id) = user.id.parse::<u64>() else {
        return error_page(StatusCode::BAD_GATEWAY, "Réponse Discord invalide.");
    };

    let admin = is_admin(&st, user_id).await;
    if !admin {
        tracing::warn!(
            "[Web] Accès refusé à {} ({user_id}) : pas administrateur",
            user.username
        );
        return error_page(
            StatusCode::FORBIDDEN,
            "Accès réservé aux administrateurs du serveur. Votre compte n'a pas la permission « Administrateur » sur le serveur du bot.",
        );
    }

    let sid = random_token();
    let session = Session {
        user_id,
        username: user.global_name.unwrap_or(user.username),
        avatar: user.avatar,
        csrf: random_token(),
        expires: Instant::now() + SESSION_TTL,
        admin_checked: Some((Instant::now(), true)),
    };
    tracing::info!("[Web] Connexion de {} ({user_id})", session.username);
    {
        let mut sessions = st.sessions.lock().unwrap();
        let now = Instant::now();
        sessions.retain(|_, s| s.expires > now);
        sessions.insert(sid.clone(), session);
    }
    let jar = jar.add(cookie(&st.cfg, SESSION_COOKIE, sid, SESSION_TTL));
    (jar, Redirect::to("/")).into_response()
}

async fn exchange_code(st: &WebState, code: &str) -> anyhow::Result<DiscordUser> {
    let token: TokenResponse = st
        .data
        .http
        .post(format!("{}/oauth2/token", st.cfg.api_base))
        .basic_auth(st.cfg.client_id, Some(&st.cfg.client_secret))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", &st.cfg.redirect_uri()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(st
        .data
        .http
        .get(format!("{}/users/@me", st.cfg.api_base))
        .bearer_auth(&token.access_token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

/// L'utilisateur est-il propriétaire ou administrateur du serveur du bot ?
pub async fn is_admin(st: &WebState, user_id: u64) -> bool {
    let guild = st.guild();
    let user = serenity::UserId::new(user_id);
    let Ok(member) = guild.member(&st.ctx, user).await else {
        return false;
    };
    let everyone = serenity::RoleId::new(guild.get());
    let check = |owner: serenity::UserId, roles: &HashMap<serenity::RoleId, serenity::Role>| {
        owner == user
            || member
                .roles
                .iter()
                .chain(std::iter::once(&everyone))
                .any(|r| {
                    roles
                        .get(r)
                        .is_some_and(|role| role.permissions.administrator())
                })
    };
    if let Some(g) = st.ctx.cache.guild(guild) {
        return check(g.owner_id, &g.roles);
    }
    match guild.to_partial_guild(&st.ctx.http).await {
        Ok(g) => check(g.owner_id, &g.roles),
        Err(e) => {
            tracing::warn!("[Web] Impossible de lire le serveur pour vérifier les droits: {e}");
            false
        }
    }
}

async fn logout(State(st): State<Arc<WebState>>, jar: CookieJar, headers: HeaderMap) -> Response {
    if let Some(sid) = jar.get(SESSION_COOKIE).map(|c| c.value().to_string()) {
        let mut sessions = st.sessions.lock().unwrap();
        // Déconnexion protégée par le jeton CSRF de la session.
        let token = headers.get("x-csrf-token").and_then(|v| v.to_str().ok());
        if sessions
            .get(&sid)
            .is_some_and(|s| Some(s.csrf.as_str()) == token)
        {
            sessions.remove(&sid);
        } else {
            return api::ApiError::forbidden("Jeton CSRF invalide").into_response();
        }
    }
    let jar = jar.remove(Cookie::build(SESSION_COOKIE).path("/").build());
    (jar, StatusCode::NO_CONTENT).into_response()
}

/// Administrateur authentifié (extracteur des routes `/api`).
pub struct Admin {
    pub session: Session,
}

impl FromRequestParts<Arc<WebState>> for Admin {
    type Rejection = api::ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        st: &Arc<WebState>,
    ) -> Result<Self, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);
        let sid = jar
            .get(SESSION_COOKIE)
            .map(|c| c.value().to_string())
            .ok_or_else(api::ApiError::unauthorized)?;
        let session = {
            let sessions = st.sessions.lock().unwrap();
            sessions
                .get(&sid)
                .filter(|s| s.expires > Instant::now())
                .cloned()
                .ok_or_else(api::ApiError::unauthorized)?
        };
        if parts.method != Method::GET && parts.method != Method::HEAD {
            let token = parts
                .headers
                .get("x-csrf-token")
                .and_then(|v| v.to_str().ok());
            if token != Some(session.csrf.as_str()) {
                return Err(api::ApiError::forbidden("Jeton CSRF invalide"));
            }
        }
        let fresh = session
            .admin_checked
            .is_some_and(|(at, ok)| ok && at.elapsed() < ADMIN_RECHECK);
        if !fresh {
            let ok = is_admin(st, session.user_id).await;
            let mut sessions = st.sessions.lock().unwrap();
            if ok {
                if let Some(s) = sessions.get_mut(&sid) {
                    s.admin_checked = Some((Instant::now(), true));
                }
            } else {
                sessions.remove(&sid);
                tracing::warn!(
                    "[Web] Droits administrateur perdus pour {}",
                    session.user_id
                );
                return Err(api::ApiError::forbidden(
                    "Vous n'êtes plus administrateur du serveur",
                ));
            }
        }
        Ok(Admin { session })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_and_long() {
        let (a, b) = (random_token(), random_token());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
    }

    #[test]
    fn escaping() {
        assert_eq!(
            html_escape("<b>\"x\" & y</b>"),
            "&lt;b&gt;&quot;x&quot; &amp; y&lt;/b&gt;"
        );
    }

    #[test]
    fn page_has_no_external_resources() {
        let page = include_str!("index.html");
        assert!(!page.contains("<script src"));
        assert!(!page.contains("<link"));
        // Le contenu dynamique passe par textContent, jamais innerHTML.
        assert!(!page.contains("innerHTML"));
    }
}

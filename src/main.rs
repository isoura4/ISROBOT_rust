//! ISROBOT — version Rust (poise + serenity).

mod ai;
mod backup;
mod bot;
mod commands;
mod config;
mod db;
mod errors;
mod features;
mod i18n;
mod tasks;
mod web;
mod xp;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use poise::serenity_prelude as serenity;

use bot::{Data, Error};
use config::{Config, GAMEVOX_API_BASE, Platform};

fn init_logging(root: &std::path::Path) -> tracing_appender::non_blocking::WorkerGuard {
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};

    let level = std::env::var("LOG_LEVEL")
        .unwrap_or_else(|_| "INFO".into())
        .to_lowercase();
    // Les logs des bibliothèques restent en "warn" pour ne pas noyer ceux du bot.
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("warn,isrobot={level}")));

    let file = std::env::var("LOG_FILE").unwrap_or_else(|_| "isrobot.log".into());
    let file = PathBuf::from(file);
    let file = if file.is_absolute() {
        file
    } else {
        root.join(file)
    };
    let dir = file
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| root.to_path_buf());
    let name = file
        .file_name()
        .map(|n| n.to_owned())
        .unwrap_or_else(|| "isrobot.log".into());
    // Rotation quotidienne, LOG_BACKUP_COUNT fichiers conservés.
    let keep = std::env::var("LOG_BACKUP_COUNT")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(5);
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(name.to_string_lossy())
        .max_log_files(keep.max(1))
        .build(dir)
        .expect("impossible de créer le fichier de logs");
    let (writer, guard) = tracing_appender::non_blocking(appender);

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .with(fmt::layer().with_ansi(false).with_writer(writer))
        .init();
    guard
}

#[tokio::main]
async fn main() -> Result<()> {
    let root = std::env::current_dir()?;
    if let Err(e) = config::ensure_env_variables(&root) {
        eprintln!("Impossible de synchroniser .env avec .env.example: {e}");
    }
    let _ = dotenvy::from_path(root.join(".env"));
    let _log_guard = init_logging(&root);

    let config = match Config::from_env(&root) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Erreur de validation des variables d'environnement: {e}");
            std::process::exit(1);
        }
    };

    tracing::info!("Initialisation de la base de données...");
    let database = db::Db::new(&config.db_path);
    if config.db_path.exists() && !backup::auto_recover(&config.db_path, &backup::backup_dir(&root))
    {
        tracing::error!("La base de données est corrompue et n'a pas pu être restaurée.");
    }
    {
        let database = database.clone();
        tokio::task::spawn_blocking(move || db::init(&database))
            .await?
            .context("initialisation de la base de données")?;
    }
    tracing::info!("Base de données prête");

    let commands = commands::all();
    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands,
            on_error: |e| Box::pin(errors::on_error(e)),
            // Une fonctionnalité désactivée à chaud ne répond plus.
            command_check: Some(|ctx| {
                Box::pin(async move { Ok(commands::command_enabled(ctx.command())) })
            }),
            event_handler: |fw, event| Box::pin(event_handler(fw, event)),
            ..Default::default()
        })
        .setup({
            let config = config.clone();
            let database = database.clone();
            move |ctx, ready, framework| {
                Box::pin(async move {
                    tracing::info!(
                        "Connecté en tant que {} (ID: {})",
                        ready.user.name,
                        ready.user.id
                    );
                    let guild = serenity::GuildId::new(config.server_id);
                    // Les commandes sont déclarées sur le serveur configuré (instantané),
                    // et les anciennes commandes globales sont retirées.
                    let n = commands::sync(ctx, &framework.options().commands, guild).await?;
                    serenity::Command::set_global_commands(ctx, vec![]).await?;
                    tracing::info!("{n} commande(s) synchronisée(s) avec le serveur");
                    let data = Data::new(config, database);
                    tasks::spawn_all(ctx.clone(), data.clone());
                    web::spawn(ctx.clone(), data.clone());
                    Ok(data)
                })
            }
        })
        .build();

    let mut intents =
        serenity::GatewayIntents::non_privileged() | serenity::GatewayIntents::MESSAGE_CONTENT;
    intents.remove(
        serenity::GatewayIntents::GUILD_PRESENCES | serenity::GatewayIntents::GUILD_MESSAGE_TYPING,
    );

    let mut http = serenity::HttpBuilder::new(&config.token)
        .application_id(serenity::ApplicationId::new(config.app_id));
    match config.platform {
        Platform::GameVox => {
            tracing::info!("🎮 Configuration du bot pour la plateforme GameVox");
            // L'API GameVox est compatible Discord : on redirige toutes les requêtes REST.
            // L'URL de la gateway est ensuite fournie par l'API (/gateway/bot).
            http = http.proxy(GAMEVOX_API_BASE);
        }
        Platform::Discord => tracing::info!("🤖 Configuration du bot pour la plateforme Discord"),
    }

    let mut client = serenity::ClientBuilder::new_with_http(http.build(), intents)
        .framework(framework)
        .activity(serenity::ActivityData::custom(tr!("bot.presence", None)))
        .await
        .context("création du client")?;

    let shard_manager = Arc::clone(&client.shard_manager);
    tracing::info!("Démarrage du bot...");
    tokio::select! {
        res = client.start() => res?,
        _ = shutdown_signal() => {
            tracing::warn!("Signal d'arrêt reçu, arrêt gracieux du bot...");
            // Borne le temps d'arrêt (ex. gateway injoignable pendant une reconnexion).
            let _ = tokio::time::timeout(Duration::from_secs(5), shard_manager.shutdown_all()).await;
        }
    }
    tracing::info!("Bot arrêté avec succès");
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("gestionnaire SIGTERM");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

async fn event_handler(
    fw: poise::FrameworkContext<'_, Data, Error>,
    event: &serenity::FullEvent,
) -> Result<(), Error> {
    let (ctx, data) = (fw.serenity_context, fw.user_data);
    match event {
        serenity::FullEvent::Ready { .. } => tracing::info!("Bot prêt !"),
        serenity::FullEvent::Message { new_message: msg } => {
            if msg.author.bot || msg.guild_id.is_none() {
                return Ok(());
            }
            if features::is_enabled("xp_system")
                && let Err(e) = commands::xp_system::on_message(ctx, data, msg).await
            {
                tracing::error!(
                    "Erreur lors de l'ajout d'XP pour {}: {e:#}",
                    msg.author.name
                );
            }
            if let Err(e) = commands::count::on_message(ctx, data, msg).await {
                tracing::error!("Erreur lors du traitement du jeu de comptage: {e:#}");
            }
        }
        serenity::FullEvent::VoiceStateUpdate { new, .. } if features::is_enabled("xp_voice") => {
            commands::xp_voice::on_voice_state_update(ctx, data, new);
        }
        _ => {}
    }
    Ok(())
}

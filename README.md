# ISROBOT (Rust)

Portage en Rust du bot [ISROBOT_python](https://github.com/isoura4/ISROBOT_python),
basé sur [serenity](https://github.com/serenity-rs/serenity) et
[poise](https://github.com/serenity-rs/poise). Compatible Discord et GameVox.

Le portage se fait **module par module**. La version Rust reprend la même base
SQLite, le même `.env` et les mêmes fichiers de traduction que la version Python :
vous pouvez passer de l'une à l'autre sans perdre de données.

## Avancement du portage

Tous les modules de la version Python sont portés :

| Module (`COG_*`) | Contenu |
|---|---|
| socle | `.env` (synchronisation + validation), SQLite (schéma, migrations, données par défaut), traductions fr/en, Discord **et** GameVox |
| `ping`, `ping_bot`, `coinflip` | commandes simples |
| `count` | minijeu du compteur |
| `xp_system`, `xp_voice` | XP texte (/level, /leaderboard), XP vocal horaire |
| `moderation`, `moderation_config`, `moderation_context`, `user_moderation` | /warn, /warns, /unwarn, /mute, /unmute, /modlog, /modconfig, menu « Warn User », /appeal + revue par boutons, expiration automatique |
| `ai` | /ai (Ollama ou API compatible OpenAI), filtre de contenu, modération IA des messages |
| `stream`, `youtube`, `bluesky` | annonces Twitch, YouTube (vidéos/shorts), Bluesky |
| `minigame` | /minigame, /daily, /quest, /shop, /inventory, /use, /trade, /wallet, /history, /capture, /duel, /stats |
| administration | /reload, /feature_list, /feature_enable, /feature_disable, sauvegardes automatiques |

### Différences avec la version Python

- **Pas de rechargement de code à chaud** (Rust est compilé) : `/reload` relit le `.env`
  et resynchronise les commandes. Activer/désactiver un module via `/feature_*`
  prend effet immédiatement, sans redémarrer.
- Toutes les commandes sont déclarées sur le serveur `server_id` (instantané) ;
  les anciennes commandes globales sont retirées au démarrage.
- Les boutons de revue des appels (`/appeal`) fonctionnent aussi après un redémarrage.
- Les logs tournent chaque jour (`LOG_BACKUP_COUNT` fichiers conservés).
- Corrections de bugs présents dans la version Python :
  - `/duel` échouait systématiquement (argument `metadata` inconnu) ;
  - `/trade offer` avec de l'XP plantait (`get_xp_transfer_warning` non importé) ;
  - `/stats` comptait deux fois chaque victoire (transactions pièces + XP) ;
  - la restauration automatique d'une base corrompue ne pouvait pas écraser le fichier ;
  - `AI_CONTENT_FILTER_ENABLED` est désormais réellement pris en compte par `/ai`.

## Prérequis

- Rust stable récent (`rustup` : <https://rustup.rs>)
- Aucune dépendance système : SQLite est embarqué (`rusqlite` en mode `bundled`).

## Installation

```bash
git clone <ce dépôt>
cd ISROBOT_rust
cp .env.example .env   # puis renseignez vos valeurs
cargo run --release
```

Au démarrage, les clés manquantes de `.env` sont ajoutées depuis `.env.example`
(comme dans la version Python).

Pour réutiliser la base de la version Python, pointez `db_path` vers le fichier
existant (par ex. `db_path=../ISROBOT_python/database.sqlite3`). Une copie de
sauvegarde `<base>.<date>.bak` est créée à chaque démarrage avant les migrations.

### GameVox

```env
PLATFORM=gamevox
gamevox_bot_token=votre_token
```

Les requêtes REST sont redirigées vers `https://bot-api.gamevox.com` ; l'URL de
la gateway est fournie par l'API GameVox.

## Interface web d'administration

Le bot embarque une interface web (désactivée par défaut) pour gérer sans commandes :
fonctionnalités, configuration de la modération, avertissements / mutes / signalements IA,
notifications Twitch / YouTube / Bluesky, réglages du minijeu, salons du compteur et classement.

**Accès** : connexion OAuth2 Discord (scope `identify` seulement), puis le bot vérifie que le
compte est **propriétaire ou a la permission Administrateur** sur le serveur `server_id`.
La vérification est refaite chaque minute : un admin rétrogradé perd l'accès immédiatement.

Mise en place :

1. Portail développeur Discord → votre application → **OAuth2** :
   copiez le **Client Secret** et ajoutez la redirection `https://votre-domaine/auth/callback`
   (ou `http://localhost:8080/auth/callback` en local).
2. Dans `.env` :
   ```env
   WEB_ENABLED=true
   WEB_BIND=127.0.0.1:8080
   WEB_BASE_URL=https://votre-domaine
   WEB_OAUTH_CLIENT_SECRET=le_client_secret
   ```
3. Redémarrez le bot puis ouvrez `WEB_BASE_URL`.

Sécurité : le serveur écoute en local par défaut ; pour un accès distant, placez-le derrière
un reverse proxy **HTTPS** (Caddy, Nginx...). Avec une `WEB_BASE_URL` en `https://`, les cookies
de session sont marqués `Secure`. Les sessions sont en mémoire (perdues au redémarrage),
toute modification exige un jeton anti-CSRF et chaque action est tracée dans les logs (`[Web]`).

## Configuration utile

| Variable           | Rôle |
|--------------------|------|
| `LOG_LEVEL`        | `ERROR`, `WARN`, `INFO`, `DEBUG`, `TRACE` (logs du bot) |
| `RUST_LOG`         | Filtre complet `tracing` (prioritaire sur `LOG_LEVEL`), ex. `info,serenity=debug` |
| `LOG_FILE`         | Fichier de logs (rotation quotidienne) |
| `LOG_BACKUP_COUNT` | Nombre de fichiers de logs conservés |
| `COG_<MODULE>_ENABLED` | Active/désactive un module (`true`/`false`) |

## Développement

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

### Structure

```
src/
  main.rs            démarrage, connexion, événements
  bot.rs             état partagé (Data), types communs
  config.rs          .env : synchronisation, validation, rechargement à chaud
  db/                SQLite : schéma, migrations, données par défaut
  i18n.rs            traductions (locales/*.json embarqués)
  features.rs        activation des modules (COG_*_ENABLED)
  errors.rs          gestion globale des erreurs de commandes
  tasks.rs           tâches de fond (vérifications, expirations, sauvegardes)
  backup.rs          sauvegardes SQLite vérifiées + restauration automatique
  moderation.rs      logique de modération (avertissements, mutes, appels)
  ai.rs              fournisseurs IA, filtre de contenu, analyse de modération
  economy.rs         économie du minijeu (soldes, boutique, échanges, jeux)
  quests.rs          quêtes (progression, quotidiennes, récompenses, séries)
  ui.rs              modales et réponses d'interaction
  web/               interface web : OAuth2, sessions, API JSON, page (index.html)
  xp.rs              formule XP ↔ niveau
  commands/          les commandes Discord, un fichier par module
locales/             fr.json, en.json (mêmes clés que la version Python)
```

### Ajouter un module

1. Créer `src/commands/<module>.rs` avec les commandes `#[poise::command]`.
2. L'ajouter dans `commands::modules()` avec sa clé `COG_*` (et dans `features::FEATURES`).
3. Ajouter `cmd.<nom>.desc` et `cmd.<nom>.<paramètre>` dans `locales/fr.json` et
   `locales/en.json` : descriptions et paramètres sont localisés automatiquement,
   et le test `all_commands_are_registrable` vérifie qu'aucune description ne manque.

# ISROBOT (Rust)

Portage en Rust du bot [ISROBOT_python](https://github.com/isoura4/ISROBOT_python),
basé sur [serenity](https://github.com/serenity-rs/serenity) et
[poise](https://github.com/serenity-rs/poise). Compatible Discord et GameVox.

Le portage se fait **module par module**. La version Rust reprend la même base
SQLite, le même `.env` et les mêmes fichiers de traduction que la version Python :
vous pouvez passer de l'une à l'autre sans perdre de données.

## Avancement du portage

| Module (`COG_*`)                  | État        |
|-----------------------------------|-------------|
| Configuration `.env` + validation | ✅ porté     |
| Base SQLite + migrations + données par défaut | ✅ porté |
| Traductions fr/en (`locales/`)    | ✅ porté     |
| Plateforme GameVox                | ✅ porté (à tester en conditions réelles) |
| `ping`, `ping_bot`, `coinflip`    | ✅ porté     |
| `count` (minijeu du compteur)     | ✅ porté     |
| `xp_system` (XP texte, /level, /leaderboard) | ✅ porté |
| Quêtes : progression (`messages_sent`, `coinflip_used`, `counting_participation`) | ✅ porté |
| `xp_voice`                        | ⏳ à faire   |
| `moderation`, `moderation_config`, `moderation_context`, `user_moderation` | ⏳ à faire |
| `ai` + modération IA (Ollama / OpenAI-compatible) | ⏳ à faire |
| `stream` (Twitch)                 | ⏳ à faire   |
| `youtube`                         | ⏳ à faire   |
| `bluesky`                         | ⏳ à faire   |
| `minigame` (quêtes, boutique, échanges, captures, duels) | ⏳ à faire |
| Commandes de gestion des fonctionnalités / reload | ⏳ à faire |
| Sauvegardes automatiques planifiées | ⏳ à faire |

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
  config.rs          .env : synchronisation et validation
  db/                SQLite : schéma, migrations, données par défaut
  i18n.rs            traductions (locales/*.json embarqués)
  features.rs        activation des modules (COG_*_ENABLED)
  errors.rs          gestion globale des erreurs de commandes
  quests.rs          progression des quêtes
  xp.rs              formule XP ↔ niveau
  commands/          une commande ou un groupe par fichier
locales/             fr.json, en.json (identiques à la version Python)
```

### Ajouter un module porté

1. Créer `src/commands/<module>.rs` avec les commandes `#[poise::command]`.
2. L'ajouter dans la liste de `commands::all()` avec sa clé `COG_*`.
3. Ajouter la clé `cmd.<nom>.desc` dans `locales/fr.json` et `locales/en.json`
   (les descriptions des commandes sont localisées automatiquement).

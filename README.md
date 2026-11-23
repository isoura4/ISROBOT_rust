# ISROBOT (Rust)

Portage en Rust du bot [ISROBOT_python](https://github.com/isoura4/ISROBOT_python),
basé sur [serenity](https://github.com/serenity-rs/serenity) et
[poise](https://github.com/serenity-rs/poise). Compatible Discord et GameVox.

Successeur de la version Python : il réutilise la même base SQLite, le même `.env` et
les mêmes fichiers de traduction, on peut donc migrer sans perte de données.

## Fonctionnalités

| Module (`COG_*`) | Contenu |
|---|---|
| socle | `.env` (créé et complété automatiquement), SQLite (migrations, mode WAL, sauvegardes), traductions fr/en, Discord **et** GameVox |
| `ping`, `ping_bot`, `coinflip` | commandes simples |
| `count` | jeu du compteur (un salon par serveur) |
| `xp_system`, `xp_voice` | XP texte (/level, /leaderboard), XP vocal horaire |
| `ai` | /ai (Ollama ou API compatible OpenAI), filtre de contenu, délai anti-spam |
| `stream`, `youtube`, `bluesky` | annonces Twitch, YouTube (vidéos/shorts), Bluesky |
| administration | /reload, /feature_list, /feature_enable, /feature_disable, interface web |

### Différences avec la version Python

- **Retirés volontairement** : le minijeu (quêtes, boutique, objets, échanges, capture,
  duel) et la modération (/warn, /mute, appels, modération IA), Discord proposant déjà
  ses propres outils. Les tables correspondantes restent dans la base (aucune perte).
- **Pas de rechargement de code à chaud** (Rust est compilé) : `/reload` relit le `.env`
  et resynchronise les commandes. Activer/désactiver un module via `/feature_*`
  ou l'interface web prend effet immédiatement, sans redémarrer.
- Toutes les commandes sont déclarées sur le serveur `server_id` (instantané) ;
  les anciennes commandes globales sont retirées au démarrage.
- Compteur : un seul salon par serveur ; une erreur ne remet à zéro que ce salon.
- Twitch : une seule requête pour 100 chaînes suivies.
- Les logs tournent chaque jour (`LOG_BACKUP_COUNT` fichiers conservés).

## Prérequis

- Rust stable récent (`rustup` : <https://rustup.rs>)
- Aucune dépendance système : SQLite est embarqué (`rusqlite` en mode `bundled`).

## Installation

Au premier lancement, le bot crée un `.env` complet dans son dossier courant puis s'arrête
en indiquant les valeurs à renseigner ; les nouvelles variables sont ajoutées
automatiquement lors des mises à jour.

### Binaire précompilé

Téléchargez l'archive de votre système dans les
[Releases](https://github.com/isoura4/ISROBOT_rust/releases) (Linux, Windows, macOS),
décompressez-la, lancez `isrobot` une première fois, remplissez `.env`, relancez.

### Docker

```bash
git clone https://github.com/isoura4/ISROBOT_rust && cd ISROBOT_rust
docker compose up -d --build     # crée ./data/.env puis s'arrête
nano data/.env                   # renseignez token, IDs...
docker compose restart
```

`./data` contient `.env`, la base, les logs et les sauvegardes. Une image prête à l'emploi
est aussi publiée à chaque version : `ghcr.io/isoura4/isrobot_rust`.

### Service Linux (systemd)

Le fichier [`deploy/isrobot.service`](deploy/isrobot.service) contient l'unité et les
commandes d'installation (utilisateur dédié, redémarrage automatique, durcissement).

### Depuis les sources

```bash
cargo run --release
```

### Publier une version

```bash
git tag v1.0.0 && git push origin v1.0.0
```

La CI compile les binaires Linux/Windows/macOS, les attache à la Release GitHub et publie
l'image Docker.

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
fonctionnalités, notifications Twitch / YouTube / Bluesky, salon du compteur, membres & XP
(classement, modification de l'XP, réinitialisation), sauvegardes (création, téléchargement)
et journal des actions faites depuis l'interface.

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
   WEB_BIND=127.0.0.1:8080        # 0.0.0.0:8080 dans Docker (déjà réglé par docker-compose)
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
  ai.rs              fournisseurs IA et filtre de contenu
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

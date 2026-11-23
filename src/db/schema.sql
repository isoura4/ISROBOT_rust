-- Schéma complet de la base ISROBOT.
-- Strictement identique à database.py + db_migrations.py de la version Python
-- afin que la version Rust puisse réutiliser une base existante telle quelle.

CREATE TABLE IF NOT EXISTS users (
    guildId TEXT NOT NULL,
    userId TEXT NOT NULL,
    xp REAL DEFAULT 0,
    level INTEGER DEFAULT 1,
    messages INTEGER DEFAULT 0,
    coins REAL DEFAULT 0,
    PRIMARY KEY (guildId, userId)
);

CREATE TABLE IF NOT EXISTS streamers (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    streamerName TEXT NOT NULL,
    streamChannelId TEXT,
    roleId TEXT,
    announced INTEGER DEFAULT 0,
    startTime TEXT
);

CREATE TABLE IF NOT EXISTS youtube_channels (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    channelId TEXT NOT NULL,
    channelName TEXT NOT NULL,
    discordChannelId TEXT NOT NULL,
    roleId TEXT,
    lastVideoId TEXT,
    lastShortId TEXT,
    lastLiveId TEXT,
    notifyVideos INTEGER DEFAULT 1,
    notifyShorts INTEGER DEFAULT 1,
    notifyLive INTEGER DEFAULT 1
);

CREATE TABLE IF NOT EXISTS bluesky_accounts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    handle TEXT NOT NULL,
    did TEXT,
    displayName TEXT,
    discordChannelId TEXT NOT NULL,
    roleId TEXT,
    lastPostUri TEXT,
    lastPostCid TEXT,
    createdAt TEXT
);

CREATE TABLE IF NOT EXISTS counter_game (
    guildId TEXT NOT NULL,
    channelId TEXT NOT NULL,
    messageId TEXT DEFAULT '',
    userId TEXT NOT NULL,
    lastUserId TEXT DEFAULT '0',
    count INTEGER DEFAULT 0,
    PRIMARY KEY (guildId, channelId)
);

-- --- Modération ---

CREATE TABLE IF NOT EXISTS warnings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    warn_count INTEGER DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(guild_id, user_id)
);
CREATE INDEX IF NOT EXISTS idx_warnings_guild_user ON warnings(guild_id, user_id);

CREATE TABLE IF NOT EXISTS warning_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    action TEXT NOT NULL,
    warn_count_before INTEGER NOT NULL,
    warn_count_after INTEGER NOT NULL,
    moderator_id TEXT,
    reason TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_history_guild_user ON warning_history(guild_id, user_id);

CREATE TABLE IF NOT EXISTS moderation_appeals (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    warning_history_id INTEGER,
    appeal_reason TEXT NOT NULL,
    moderator_id TEXT,
    status TEXT DEFAULT 'pending',
    moderator_decision TEXT,
    created_at TEXT NOT NULL,
    reviewed_at TEXT,
    FOREIGN KEY(warning_history_id) REFERENCES warning_history(id)
);
CREATE INDEX IF NOT EXISTS idx_appeals_status ON moderation_appeals(guild_id, status);

CREATE TABLE IF NOT EXISTS moderation_config (
    guild_id TEXT PRIMARY KEY,
    log_channel_id TEXT,
    appeal_channel_id TEXT,
    ai_enabled INTEGER DEFAULT 1,
    ai_confidence_threshold INTEGER DEFAULT 60,
    ai_flag_channel_id TEXT,
    ai_model TEXT DEFAULT 'llama2',
    ollama_host TEXT DEFAULT 'http://localhost:11434',
    decay_multiplier REAL DEFAULT 1.0,
    warn_1_decay_days INTEGER DEFAULT 7,
    warn_2_decay_days INTEGER DEFAULT 14,
    warn_3_decay_days INTEGER DEFAULT 21,
    mute_duration_warn_2 INTEGER DEFAULT 3600,
    mute_duration_warn_3 INTEGER DEFAULT 86400,
    rules_message_id TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS ai_flags (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    channel_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    message_content TEXT NOT NULL,
    ai_score INTEGER NOT NULL,
    ai_category TEXT NOT NULL,
    ai_reason TEXT NOT NULL,
    moderator_action TEXT DEFAULT 'pending',
    moderator_id TEXT,
    created_at TEXT NOT NULL,
    reviewed_at TEXT,
    UNIQUE(message_id)
);
CREATE INDEX IF NOT EXISTS idx_ai_flags_status ON ai_flags(guild_id, moderator_action);

CREATE TABLE IF NOT EXISTS active_mutes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    guild_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    moderator_id TEXT,
    reason TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    is_auto INTEGER DEFAULT 0,
    UNIQUE(guild_id, user_id)
);
CREATE INDEX IF NOT EXISTS idx_mutes_expires ON active_mutes(expires_at);

-- --- Minijeu (db_migrations.create_minigame_tables) ---

CREATE TABLE IF NOT EXISTS guild_settings (
    guildId TEXT PRIMARY KEY,
    minigame_enabled INTEGER DEFAULT 1,
    minigame_channel_id TEXT,
    xp_trading_enabled INTEGER DEFAULT 1,
    trade_tax_percent REAL DEFAULT 10.0,
    duel_tax_percent REAL DEFAULT 10.0,
    daily_xp_transfer_cap_percent REAL DEFAULT 10.0,
    daily_xp_transfer_cap_max INTEGER DEFAULT 500,
    capture_cooldown_seconds INTEGER DEFAULT 60,
    duel_cooldown_seconds INTEGER DEFAULT 300,
    created_at TEXT DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS quest_exception_channels (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    guildId TEXT NOT NULL,
    channelId TEXT NOT NULL,
    created_at TEXT DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(guildId, channelId)
);

CREATE TABLE IF NOT EXISTS quests (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    type TEXT NOT NULL CHECK(type IN ('daily', 'random', 'event')),
    target_type TEXT NOT NULL,
    target_value INTEGER NOT NULL DEFAULT 1,
    reward_coins INTEGER DEFAULT 0,
    reward_xp INTEGER DEFAULT 0,
    allow_other_channels INTEGER DEFAULT 0,
    rarity TEXT DEFAULT 'common' CHECK(
        rarity IN ('common', 'uncommon', 'rare', 'epic', 'legendary')
    ),
    metadata TEXT DEFAULT '{}',
    active INTEGER DEFAULT 1,
    created_at TEXT DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS user_quests (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    guildId TEXT NOT NULL,
    userId TEXT NOT NULL,
    questId INTEGER NOT NULL,
    progress INTEGER DEFAULT 0,
    completed INTEGER DEFAULT 0,
    claimed INTEGER DEFAULT 0,
    assigned_at TEXT DEFAULT CURRENT_TIMESTAMP,
    completed_at TEXT,
    FOREIGN KEY (questId) REFERENCES quests(id)
);

CREATE TABLE IF NOT EXISTS user_daily_tracking (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    guildId TEXT NOT NULL,
    userId TEXT NOT NULL,
    last_daily_claim TEXT,
    streak INTEGER DEFAULT 0,
    daily_xp_transferred INTEGER DEFAULT 0,
    last_xp_transfer_reset TEXT,
    last_capture_at TEXT,
    last_duel_at TEXT,
    UNIQUE(guildId, userId)
);

CREATE TABLE IF NOT EXISTS shop_items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    price_coins INTEGER DEFAULT 0,
    price_xp INTEGER DEFAULT 0,
    consumable INTEGER DEFAULT 1,
    stock INTEGER DEFAULT -1,
    metadata TEXT DEFAULT '{}',
    active INTEGER DEFAULT 1,
    created_at TEXT DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS user_inventory (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    guildId TEXT NOT NULL,
    userId TEXT NOT NULL,
    itemId INTEGER NOT NULL,
    quantity INTEGER DEFAULT 1,
    acquired_at TEXT DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (itemId) REFERENCES shop_items(id),
    UNIQUE(guildId, userId, itemId)
);

CREATE TABLE IF NOT EXISTS user_active_effects (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    guildId TEXT NOT NULL,
    userId TEXT NOT NULL,
    effect_type TEXT NOT NULL,
    effect_data TEXT DEFAULT '{}',
    expires_at TEXT NOT NULL,
    UNIQUE(guildId, userId, effect_type)
);

CREATE TABLE IF NOT EXISTS trades (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    guildId TEXT NOT NULL,
    fromUserId TEXT NOT NULL,
    toUserId TEXT NOT NULL,
    coins INTEGER DEFAULT 0,
    xp INTEGER DEFAULT 0,
    status TEXT DEFAULT 'pending' CHECK(
        status IN ('pending', 'accepted', 'completed', 'canceled', 'expired')
    ),
    tax_coins INTEGER DEFAULT 0,
    tax_xp INTEGER DEFAULT 0,
    created_at TEXT DEFAULT CURRENT_TIMESTAMP,
    accepted_at TEXT,
    escrow_release_at TEXT,
    completed_at TEXT
);

CREATE TABLE IF NOT EXISTS transactions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    guildId TEXT NOT NULL,
    userId TEXT NOT NULL,
    kind TEXT NOT NULL,
    amount REAL NOT NULL,
    currency TEXT DEFAULT 'coins' CHECK(currency IN ('coins', 'xp')),
    balance_after REAL,
    metadata TEXT DEFAULT '{}',
    related_id INTEGER,
    related_type TEXT,
    created_at TEXT DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS user_cooldowns (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    guildId TEXT NOT NULL,
    userId TEXT NOT NULL,
    action_type TEXT NOT NULL,
    last_action_at TEXT DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(guildId, userId, action_type)
);

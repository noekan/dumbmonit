-- Jalons 4 et 5 : règles d'alerte, machine à états, silences, canaux de
-- notification et baselines saisonnières.
--
-- Les horodatages sont stockés en TEXT au format RFC 3339 en UTC. C'est le choix
-- déjà retenu par la migration initiale, et il garde la base lisible avec un
-- simple `sqlite3` quand il faut comprendre pourquoi une alerte n'est pas partie.

-- Canaux de notification. `settings` porte ce qui peut s'afficher dans
-- l'interface, `secret_enc` porte ce qui ne doit jamais en sortir : la séparation
-- est structurelle, pas conventionnelle, pour qu'un `SELECT *` d'audit ne fuite
-- aucun jeton.
CREATE TABLE notification_channels (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    name         TEXT    NOT NULL UNIQUE,
    kind         TEXT    NOT NULL CHECK (kind IN
                     ('discord', 'ntfy', 'gotify', 'telegram', 'slack', 'webhook', 'smtp')),
    enabled      INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    settings     TEXT    NOT NULL DEFAULT '{}',  -- objet JSON, non sensible
    secret_enc   BLOB,                           -- objet JSON sérialisé puis chiffré
    -- Diagnostic du dernier envoi, affiché à côté du bouton « message de test ».
    last_error   TEXT,
    last_sent_at TEXT,
    created_at   TEXT    NOT NULL DEFAULT (datetime('now')),
    updated_at   TEXT    NOT NULL DEFAULT (datetime('now'))
);

-- Règles d'alerte. `uid` est l'identité stable utilisée dans les empreintes : il
-- survit à un renommage, ce qui évite de « résoudre » puis « redéclencher » toutes
-- les alertes d'une règle simplement parce qu'on a corrigé une faute dans son nom.
CREATE TABLE alert_rules (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    uid                 TEXT    NOT NULL UNIQUE,
    name                TEXT    NOT NULL,
    description         TEXT    NOT NULL DEFAULT '',
    kind                TEXT    NOT NULL CHECK (kind IN ('threshold', 'anomaly', 'predict')),
    query               TEXT    NOT NULL,        -- expression MetricsQL évaluée telle quelle
    operator            TEXT    NOT NULL CHECK (operator IN ('>', '>=', '<', '<=')),
    threshold           REAL    NOT NULL DEFAULT 0,
    for_secs            INTEGER NOT NULL DEFAULT 0 CHECK (for_secs >= 0),
    severity            TEXT    NOT NULL CHECK (severity IN ('info', 'warning', 'critical')),
    -- Sélecteur de cibles : {"kind":"all"} | {"kind":"ids","ids":[..]}
    --                     | {"kind":"labels","labels":{..}}
    selector            TEXT    NOT NULL DEFAULT '{"kind":"all"}',
    channels            TEXT    NOT NULL DEFAULT '[]',  -- tableau JSON d'identifiants
    params              TEXT    NOT NULL DEFAULT '{}',  -- réglages d'anomalie / de prédiction
    unit                TEXT    NOT NULL DEFAULT '',    -- suffixe d'affichage : « % », « °C »
    repeat_secs         INTEGER,                        -- NULL = pas de rappel
    escalate_after_secs INTEGER,                        -- NULL = pas d'escalade
    enabled             INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    builtin             INTEGER NOT NULL DEFAULT 0 CHECK (builtin IN (0, 1)),
    created_at          TEXT    NOT NULL DEFAULT (datetime('now')),
    updated_at          TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_alert_rules_enabled ON alert_rules(enabled);

-- État courant, une ligne par empreinte (règle + série). La table ne grossit pas
-- indéfiniment : les empreintes qui ne sont plus évaluées sont purgées.
CREATE TABLE alert_state (
    fingerprint      TEXT    PRIMARY KEY,
    rule_uid         TEXT    NOT NULL,
    target_id        INTEGER,                    -- NULL si la série n'est rattachée à aucune cible
    series_key       TEXT    NOT NULL,
    labels           TEXT    NOT NULL DEFAULT '{}',
    phase            TEXT    NOT NULL CHECK (phase IN ('ok', 'pending', 'firing', 'resolved')),
    -- La suppression et le silence sont des surcouches, pas des phases : la
    -- condition continue d'être suivie pendant qu'on se tait, sans quoi la sortie
    -- de maintenance renotifierait tout depuis zéro.
    suppressed       INTEGER NOT NULL DEFAULT 0 CHECK (suppressed IN (0, 1)),
    suppressed_by    INTEGER,                    -- ancêtre injoignable responsable
    silenced         INTEGER NOT NULL DEFAULT 0 CHECK (silenced IN (0, 1)),
    learning         INTEGER NOT NULL DEFAULT 0 CHECK (learning IN (0, 1)),
    severity         TEXT    NOT NULL,
    value            REAL,
    score            REAL,                       -- score d'anomalie, NULL pour un seuil
    condition_since  TEXT,                       -- début de la condition vraie (pilote `for`)
    firing_since     TEXT,
    last_eval_at     TEXT    NOT NULL,
    last_notified_at TEXT,
    notify_count     INTEGER NOT NULL DEFAULT 0,
    resolved_at      TEXT
);

CREATE INDEX idx_alert_state_phase  ON alert_state(phase);
CREATE INDEX idx_alert_state_target ON alert_state(target_id);
CREATE INDEX idx_alert_state_rule   ON alert_state(rule_uid);

-- Journal des transitions. Sert l'historique de l'interface et, en mode
-- apprentissage, garde la trace de ce qui « aurait déclenché » sans notifier.
CREATE TABLE alert_history (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    fingerprint TEXT    NOT NULL,
    rule_uid    TEXT    NOT NULL,
    target_id   INTEGER,
    from_phase  TEXT    NOT NULL,
    to_phase    TEXT    NOT NULL,
    severity    TEXT    NOT NULL,
    value       REAL,
    notified    INTEGER NOT NULL DEFAULT 0 CHECK (notified IN (0, 1)),
    reason      TEXT    NOT NULL DEFAULT '',
    at          TEXT    NOT NULL
);

CREATE INDEX idx_alert_history_at ON alert_history(at);
CREATE INDEX idx_alert_history_fp ON alert_history(fingerprint, at);

-- Fenêtres de maintenance. `schedule` porte du JSON :
--   {"kind":"once","starts_at":"...","ends_at":"..."}
--   {"kind":"weekly","days":[0..6],"start_minute":0,"end_minute":240,"utc_offset_minutes":60}
CREATE TABLE silences (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    name       TEXT    NOT NULL,
    comment    TEXT    NOT NULL DEFAULT '',
    target_id  INTEGER REFERENCES targets(id) ON DELETE CASCADE,
    matchers   TEXT    NOT NULL DEFAULT '{}',   -- objet JSON étiquette -> valeur
    schedule   TEXT    NOT NULL,
    enabled    INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_silences_enabled ON silences(enabled);

-- Première observation d'une série : c'est elle, et non le nombre de points, qui
-- décide de la sortie du mode apprentissage (une série vue depuis quinze jours
-- mais interrogée une fois par heure reste légitime).
CREATE TABLE anomaly_series (
    series_key  TEXT PRIMARY KEY,
    first_seen  TEXT    NOT NULL,
    last_update TEXT    NOT NULL,
    updates     INTEGER NOT NULL DEFAULT 0
);

-- Baseline saisonnière : 168 seaux par série, un par couple (jour de semaine, heure).
CREATE TABLE anomaly_baselines (
    series_key  TEXT    NOT NULL,
    bucket      INTEGER NOT NULL CHECK (bucket BETWEEN 0 AND 167),
    ewma        REAL    NOT NULL,
    mad         REAL    NOT NULL,
    samples     INTEGER NOT NULL DEFAULT 0,
    last_update TEXT    NOT NULL,
    PRIMARY KEY (series_key, bucket)
) WITHOUT ROWID;

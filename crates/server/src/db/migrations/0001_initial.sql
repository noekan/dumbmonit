-- Métadonnées de l'instance : une seule ligne, garantie par la contrainte sur `id`.
CREATE TABLE instance (
    id            INTEGER PRIMARY KEY CHECK (id = 1),
    salt          BLOB    NOT NULL,  -- sel de dérivation Argon2id
    canary        BLOB    NOT NULL,  -- témoin chiffré, valide le secret au démarrage
    schema_note   TEXT    NOT NULL DEFAULT '',
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

-- Équipements surveillés.
CREATE TABLE targets (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    name           TEXT    NOT NULL,
    address        TEXT    NOT NULL,
    kind           TEXT    NOT NULL,             -- 'snmp', 'agent', 'proxmox', ...
    profile_id     TEXT,                         -- NULL tant que non détecté
    parent_id      INTEGER REFERENCES targets(id) ON DELETE SET NULL,
    interval_secs  INTEGER NOT NULL DEFAULT 60 CHECK (interval_secs >= 10),
    enabled        INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    tags           TEXT    NOT NULL DEFAULT '{}',-- objet JSON
    credential_enc BLOB,                         -- Credential sérialisé puis chiffré
    -- Résultat de la dernière interrogation, pour l'affichage et le diagnostic.
    last_probe_at  TEXT,
    last_error     TEXT,
    created_at     TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at     TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE (kind, address)
);

CREATE INDEX idx_targets_due    ON targets(enabled, id);
CREATE INDEX idx_targets_parent ON targets(parent_id);

-- Profils de collecte. Les profils livrés avec le produit sont réinsérés à chaque
-- démarrage (builtin = 1) ; ceux créés par l'utilisateur sont préservés.
CREATE TABLE profiles (
    id          TEXT PRIMARY KEY,
    name        TEXT    NOT NULL,
    kind        TEXT    NOT NULL,       -- collecteur concerné
    builtin     INTEGER NOT NULL DEFAULT 0 CHECK (builtin IN (0, 1)),
    definition  TEXT    NOT NULL,       -- YAML du profil
    updated_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

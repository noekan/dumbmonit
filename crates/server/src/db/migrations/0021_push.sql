-- Moniteurs en poussée (« heartbeat », interrupteur d'homme mort) : un travail
-- planifié, un script de sauvegarde ou une automatisation appelle DumbMonit
-- régulièrement ; si l'appel manque, c'est lui qui est en panne.
--
-- Une ligne par cible de type `push`. Le jeton est l'URL secrète que le script
-- appelle : stocké chiffré (pour pouvoir le réafficher sur la page de
-- l'équipement, où il est copié dans une ligne de cron) et haché (pour la
-- recherche indexée à chaque appel, sans déchiffrer toute la table).
CREATE TABLE push_monitors (
    target_id      INTEGER PRIMARY KEY REFERENCES targets(id) ON DELETE CASCADE,
    -- SHA-256 hexadécimal du jeton présenté dans l'URL.
    token_hash     TEXT    NOT NULL UNIQUE,
    -- Jeton chiffré avec la clé de l'instance (crypto.rs), comme un identifiant.
    token_enc      BLOB    NOT NULL,
    -- Dernier appel reçu, en millisecondes depuis l'époque Unix. NULL tant que
    -- le script n'a jamais appelé : la cible attend son premier signe de vie.
    last_seen_ms   INTEGER,
    last_seen_at   TEXT,
    -- Ce que le dernier appel a déclaré : `up` (défaut) ou `down` (le script
    -- signale lui-même un échec, `?status=down`).
    last_status    TEXT    NOT NULL DEFAULT 'up',
    -- Message libre transmis par le dernier appel (`?msg=…`), tronqué.
    last_message   TEXT    NOT NULL DEFAULT '',
    -- Nombre total d'appels reçus depuis la création du jeton.
    received_total INTEGER NOT NULL DEFAULT 0,
    created_at     TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

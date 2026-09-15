-- Notifications intelligentes : hystérésis, surcharges par équipement, politique
-- par canal, file d'attente (regroupement, heures calmes, plafond horaire) et
-- registre des envois (délai minimal, détection de battement).
--
-- La politique globale (fenêtre de regroupement, plafond horaire, réglages de
-- battement, URL publique) vit dans `settings` sous la clé `notify_policy`.

-- Seuil de retour au calme. NULL : la règle se résout dès que la condition
-- retombe, comme avant. Renseigné, la condition reste vraie tant que la valeur
-- n'est pas repassée de l'autre côté de ce seuil (« déclenche > 90, se résout
-- < 80 »), ce qui coupe les allers-retours autour du seuil.
ALTER TABLE alert_rules ADD COLUMN clear_threshold REAL;

-- Surcharges par équipement : un seuil, un seuil de retour ou une désactivation
-- propres à un équipement, sans dupliquer la règle. Une colonne NULL laisse la
-- valeur de la règle s'appliquer.
CREATE TABLE rule_overrides (
    rule_uid        TEXT    NOT NULL,
    target_id       INTEGER NOT NULL REFERENCES targets(id) ON DELETE CASCADE,
    threshold       REAL,
    clear_threshold REAL,
    enabled         INTEGER CHECK (enabled IN (0, 1)),
    updated_at      TEXT    NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (rule_uid, target_id)
) WITHOUT ROWID;

-- Politique par canal, objet JSON :
--   {"min_severity":"info","notify_resolved":true,"min_interval_secs":0,
--    "quiet_hours":{"kind":"weekly","days":[..],"start_minute":..,"end_minute":..,
--                   "utc_offset_minutes":..}}
ALTER TABLE notification_channels ADD COLUMN policy TEXT NOT NULL DEFAULT '{}';

-- File d'attente des lignes retenues avant envoi. Une ligne par canal et par
-- empreinte : une alerte qui se répète pendant l'attente remplace sa ligne au
-- lieu de s'empiler.
CREATE TABLE notify_queue (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id  INTEGER NOT NULL REFERENCES notification_channels(id) ON DELETE CASCADE,
    fingerprint TEXT    NOT NULL,
    target_id   INTEGER,
    target_name TEXT    NOT NULL,
    hold        TEXT    NOT NULL CHECK (hold IN ('batch', 'quiet')),
    item        TEXT    NOT NULL,                -- GroupItem en JSON
    -- Vrai quand l'alerte s'est résolue pendant l'attente : le message ne la
    -- cite plus que pour mémoire (« resolved while held »).
    resolved_meanwhile INTEGER NOT NULL DEFAULT 0 CHECK (resolved_meanwhile IN (0, 1)),
    queued_at   TEXT    NOT NULL,
    UNIQUE (channel_id, fingerprint)
);

CREATE INDEX idx_notify_queue_channel ON notify_queue(channel_id, queued_at);

-- Registre des lignes traitées. `channel_id = 0` : entrée dans le pipeline (sert
-- à compter les battements), sinon envoi effectif sur ce canal (sert au délai
-- minimal par empreinte et au « ne résoudre que ce qui a été annoncé »).
CREATE TABLE notify_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id  INTEGER NOT NULL,
    fingerprint TEXT    NOT NULL,
    reason      TEXT    NOT NULL,
    at          TEXT    NOT NULL
);

CREATE INDEX idx_notify_log_at ON notify_log(at);
CREATE INDEX idx_notify_log_fp ON notify_log(fingerprint, at);

-- Messages effectivement partis, pour le plafond horaire par canal.
CREATE TABLE notify_messages (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id INTEGER NOT NULL,
    items      INTEGER NOT NULL DEFAULT 1,
    at         TEXT    NOT NULL
);

CREATE INDEX idx_notify_messages_channel ON notify_messages(channel_id, at);

-- Empreintes en battement : plus rien ne part pour elles avant `held_until`.
CREATE TABLE notify_flap (
    fingerprint TEXT PRIMARY KEY,
    held_until  TEXT NOT NULL
) WITHOUT ROWID;

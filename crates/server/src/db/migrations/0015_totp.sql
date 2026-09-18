-- Second facteur (TOTP) sur les comptes locaux.
--
-- `totp_secret` est chiffré avec le secret d'instance, comme les identifiants
-- d'équipement ; NULL tant que le compte n'a rien enrôlé. `totp_enabled` ne
-- passe à 1 qu'une fois un premier code vérifié : un enrôlement abandonné à
-- mi-chemin ne verrouille personne dehors.
ALTER TABLE users ADD COLUMN totp_secret BLOB;
ALTER TABLE users ADD COLUMN totp_enabled INTEGER NOT NULL DEFAULT 0;

-- Codes de secours, hachés (SHA-256) et à usage unique : `used_at` marque ceux
-- qui ont servi. Ils disparaissent avec le compte, ou quand le second facteur
-- est désactivé.
CREATE TABLE totp_recovery_codes (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id   INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    code_hash BLOB    NOT NULL,
    used_at   TEXT
);
CREATE INDEX idx_totp_recovery_codes_user ON totp_recovery_codes(user_id);

-- Journal d'audit des gestes de sécurité : connexions, jetons, comptes, second
-- facteur. Lisible par les administrateurs dans les réglages ; jamais purgé
-- automatiquement au-delà des dernières entrées (voir `auth::audit`).
CREATE TABLE audit_log (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    at         TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    -- Compte à l'origine du geste, s'il y en a un ; conservé en texte pour
    -- survivre à la suppression du compte.
    actor      TEXT,
    -- Verbe court : `login`, `login.failed`, `totp.enabled`, `token.created`…
    action     TEXT    NOT NULL,
    -- Ce sur quoi porte le geste : un identifiant de compte, le nom d'un jeton.
    subject    TEXT,
    -- Adresse du client telle que le serveur l'a vue.
    ip         TEXT
);
CREATE INDEX idx_audit_log_at ON audit_log(at);

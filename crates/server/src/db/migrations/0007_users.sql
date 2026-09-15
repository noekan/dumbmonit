-- Comptes utilisateurs et connexion OpenID Connect.
--
-- Jusqu'ici l'instance n'avait qu'un mot de passe. Elle a désormais des comptes,
-- chacun avec un rôle : `admin` (tout) ou `viewer` (lecture seule). L'ancien mot de
-- passe devient le compte `admin`, et les sessions ouvertes lui sont rattachées :
-- une instance existante continue de fonctionner sans que personne ne se
-- reconnecte.

CREATE TABLE users (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Insensible à la casse : « Admin » et « admin » sont la même personne, et un
    -- fournisseur OIDC renvoie rarement la casse qu'on a tapée à la création.
    username      TEXT    NOT NULL UNIQUE COLLATE NOCASE,
    display_name  TEXT    NOT NULL DEFAULT '',
    role          TEXT    NOT NULL CHECK (role IN ('admin', 'viewer')),
    -- Chaîne PHC Argon2id. NULL pour un compte qui ne se connecte que par OIDC.
    password_hash TEXT,
    -- Identité chez le fournisseur OIDC (`sub`), renseignée au premier lien.
    oidc_subject  TEXT    UNIQUE,
    oidc_issuer   TEXT,
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    last_login_at TEXT,
    -- Désactivation par marquage : la ligne reste, les sessions sont fermées.
    disabled      INTEGER NOT NULL DEFAULT 0
);

-- Rattachement des sessions à leur compte. Nullable pour la compatibilité de la
-- migration ; en pratique, le serveur refuse une session sans compte.
ALTER TABLE auth_sessions ADD COLUMN user_id INTEGER REFERENCES users(id) ON DELETE CASCADE;
CREATE INDEX idx_auth_sessions_user ON auth_sessions(user_id);

-- Reprise de l'ancien mot de passe d'instance : il devient le compte `admin`.
INSERT INTO users (username, display_name, role, password_hash, created_at)
SELECT 'admin', 'Administrator', 'admin', password_hash, created_at
FROM auth_password
WHERE id = 1;

UPDATE auth_sessions
SET user_id = (SELECT id FROM users WHERE username = 'admin')
WHERE user_id IS NULL;

-- L'ancienne table n'a plus de lecteur : « l'instance est-elle configurée ? » se
-- lit désormais dans `users`.
DROP TABLE auth_password;

-- Réglages persistants, un JSON par clé. Les valeurs sensibles (secret client
-- OIDC) sont chiffrées avec le secret d'instance, comme les identifiants.
CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value      BLOB NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

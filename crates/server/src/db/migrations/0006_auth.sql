-- Authentification de l'instance : un seul mot de passe, aucune notion de compte.
--
-- Le produit protège une instance, pas des utilisateurs : il n'y a donc ni table
-- `users`, ni rôle, ni permission. Tout ce qui est nécessaire tient en deux
-- tables — le mot de passe haché, et les sessions ouvertes.

-- Mot de passe unique de l'instance. La contrainte sur `id` interdit
-- structurellement une seconde ligne : impossible de créer un « deuxième compte »
-- par erreur, même en écrivant directement dans la base.
CREATE TABLE auth_password (
    id            INTEGER PRIMARY KEY CHECK (id = 1),
    -- Chaîne PHC Argon2id (algorithme, paramètres, sel et empreinte). Le sel est
    -- tiré au hasard à chaque enregistrement du mot de passe.
    password_hash TEXT    NOT NULL,
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

-- Sessions ouvertes. Le cookie porte « <id>.<jeton> » : `id` sert à retrouver la
-- ligne, le jeton — jamais stocké en clair — sert à la prouver.
--
-- Séparer les deux n'est pas une coquetterie : cela permet de retrouver la session
-- par une clé publique, puis de comparer le seul secret en temps constant. Un vol
-- de la base ne rend aucune session utilisable, puisqu'elle ne contient que
-- l'empreinte SHA-256 du jeton.
CREATE TABLE auth_sessions (
    id         TEXT PRIMARY KEY,           -- partie publique du cookie, hexadécimale
    token_hash BLOB NOT NULL,              -- SHA-256 de la partie secrète
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL               -- RFC 3339 UTC, comparable lexicalement
);

-- La purge balaie la table par date : sans cet index, elle ferait un parcours
-- complet à chaque connexion.
CREATE INDEX idx_auth_sessions_expiry ON auth_sessions(expires_at);

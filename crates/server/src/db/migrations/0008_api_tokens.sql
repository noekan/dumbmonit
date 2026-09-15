-- Jetons d'API : ce qu'un assistant (Claude, ChatGPT, Cursor…) présente pour
-- parler au serveur MCP, et demain ce que n'importe quel client HTTP pourra
-- présenter à la place d'une session de navigateur.
--
-- Même politique que pour les jetons d'agent : seule l'empreinte SHA-256 est
-- stockée, le jeton en clair n'existe qu'une fois, dans la réponse à sa création.
-- Le préfixe lisible sert à la fois à l'afficher dans une liste et à retrouver la
-- ligne avant de comparer l'empreinte en temps constant — le même découpage
-- « partie publique / partie secrète » que les sessions.
CREATE TABLE api_tokens (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    name         TEXT    NOT NULL,
    -- Début du jeton (`dmt_` + quelques caractères), jamais le secret.
    prefix       TEXT    NOT NULL,
    -- SHA-256 brut du jeton complet.
    token_hash   BLOB    NOT NULL UNIQUE,
    -- `read` : consulter seulement. `write` : aussi poser des silences, lancer
    -- une interrogation, activer ou désactiver un équipement ou une règle.
    scope        TEXT    NOT NULL CHECK (scope IN ('read', 'write')),
    created_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    -- Mis à jour au plus une fois par minute : c'est un indice pour repérer un
    -- jeton oublié, pas un journal d'accès.
    last_used_at TEXT,
    -- Révocation par marquage : la ligne reste visible dans la liste.
    revoked_at   TEXT,
    -- Compte ayant créé le jeton. NULL tant que les comptes n'existent pas ;
    -- `SET NULL` plutôt que `CASCADE` pour qu'une suppression de compte laisse au
    -- jeton le temps d'être révoqué explicitement plutôt que de disparaître.
    user_id      INTEGER REFERENCES users(id) ON DELETE SET NULL
);

CREATE INDEX idx_api_tokens_prefix ON api_tokens(prefix);

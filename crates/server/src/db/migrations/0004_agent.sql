-- Agent système : jetons d'enregistrement et machines qui poussent leurs mesures.
--
-- Comme dans les migrations précédentes, les horodatages sont du texte RFC 3339
-- en UTC, pour que la base reste lisible avec un simple `sqlite3` le jour où il
-- faut comprendre pourquoi une machine n'apparaît pas.

-- Jetons d'enregistrement, partagés par toutes les machines qu'on installe avec
-- la même commande.
--
-- Seule l'empreinte est stockée : un vol de la base ne fournit aucun jeton
-- utilisable, exactement comme pour un mot de passe. Le jeton en clair n'existe
-- qu'une seule fois, dans la réponse à sa création — s'il est perdu, on en crée
-- un autre, on ne le « retrouve » pas.
CREATE TABLE agent_tokens (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    name         TEXT    NOT NULL,
    -- SHA-256 hexadécimal du jeton présenté. Unique : deux jetons identiques
    -- seraient de toute façon indiscernables à la vérification.
    token_hash   TEXT    NOT NULL UNIQUE,
    -- Début lisible du jeton, pour que l'interface puisse dire « celui-ci » sans
    -- jamais réafficher le secret.
    prefix       TEXT    NOT NULL,
    created_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    -- Dernier usage : c'est ce qui permet de repérer un jeton oublié et de le
    -- révoquer sans craindre de couper une machine encore active.
    last_used_at TEXT,
    -- Révocation par marquage plutôt que par suppression : les machines déjà
    -- enregistrées gardent ainsi la trace du jeton qui les a fait entrer.
    revoked_at   TEXT
);

CREATE INDEX idx_agent_tokens_revoked ON agent_tokens(revoked_at);

-- Machines équipées d'un agent, rattachées à leur cible.
--
-- `agent_key` est l'identité que l'agent s'attribue — identifiant machine quand le
-- système en expose un, nom d'hôte sinon. C'est elle, et non le nom affiché, qui
-- fait le lien d'un envoi au suivant : renommer une machine dans l'interface ne
-- doit pas la faire réapparaître en double au prochain lot.
CREATE TABLE agent_hosts (
    target_id      INTEGER PRIMARY KEY REFERENCES targets(id) ON DELETE CASCADE,
    agent_key      TEXT    NOT NULL UNIQUE,
    hostname       TEXT    NOT NULL,
    os             TEXT    NOT NULL DEFAULT '',
    os_version     TEXT,
    kernel_version TEXT,
    arch           TEXT,
    agent_version  TEXT    NOT NULL DEFAULT '',
    -- Jeton ayant servi à l'enregistrement. `SET NULL` plutôt que `CASCADE` : la
    -- suppression d'un jeton ne doit jamais faire disparaître l'historique d'une
    -- machine.
    token_id       INTEGER REFERENCES agent_tokens(id) ON DELETE SET NULL,
    -- Réception du dernier lot, en millisecondes depuis l'époque Unix.
    --
    -- Redondant avec `last_seen_at`, et volontairement : c'est la fraîcheur de
    -- cette valeur qui décide si la machine est considérée injoignable, et ce
    -- calcul a lieu à chaque cycle du planificateur. Un entier s'y compare sans
    -- analyse de chaîne ni dépendance au fuseau.
    last_seen_ms   INTEGER,
    last_seen_at   TEXT,
    last_samples   INTEGER NOT NULL DEFAULT 0,
    registered_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX idx_agent_hosts_last_seen ON agent_hosts(last_seen_ms);

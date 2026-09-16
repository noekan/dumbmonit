-- Expiration des commandes et capacités de l'agent.
--
-- Une commande n'était retirée de la file que lorsque l'agent venait la
-- chercher : une machine dont l'agent est arrêté, trop ancien pour connaître le
-- canal, ou configuré avec `commands: false`, gardait donc ses commandes
-- « en attente » indéfiniment — et refusait toute nouvelle demande (doublon).
-- Le serveur les fait désormais expirer lui-même, sous un état distinct de
-- l'annulation par un utilisateur.
--
-- SQLite ne modifie pas une contrainte CHECK en place : la table est recréée.
CREATE TABLE agent_commands_new (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    target_id    INTEGER NOT NULL REFERENCES targets(id) ON DELETE CASCADE,
    kind         TEXT    NOT NULL,
    args         TEXT    NOT NULL DEFAULT '{}',
    status       TEXT    NOT NULL DEFAULT 'queued'
                 CHECK (status IN ('queued', 'running', 'done', 'failed', 'cancelled', 'expired')),
    requested_by TEXT,
    created_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    started_at   TEXT,
    finished_at  TEXT,
    result       TEXT
);

INSERT INTO agent_commands_new
    (id, target_id, kind, args, status, requested_by, created_at, started_at, finished_at, result)
SELECT id, target_id, kind, args, status, requested_by, created_at, started_at, finished_at, result
FROM agent_commands;

DROP TABLE agent_commands;
ALTER TABLE agent_commands_new RENAME TO agent_commands;

CREATE INDEX idx_agent_commands_target ON agent_commands(target_id, id);
CREATE INDEX idx_agent_commands_status ON agent_commands(status);

-- Les commandes restées en attente au-delà du délai (dix minutes) sont
-- réputées expirées dès maintenant : c'est ce qui libère les files bloquées.
UPDATE agent_commands
SET status = 'expired',
    finished_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
    result = 'Expired: the agent did not pick it up within 10 minutes.'
WHERE status = 'queued'
  AND created_at < strftime('%Y-%m-%dT%H:%M:%SZ', 'now', '-600 seconds');

-- Ce que l'agent déclare de ses capacités à chaque lot. NULL : un agent
-- antérieur au canal de commandes, qui ne dit rien — et n'exécutera rien.
ALTER TABLE agent_hosts ADD COLUMN commands_enabled INTEGER;

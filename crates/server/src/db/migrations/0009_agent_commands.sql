-- Actions sur les machines équipées de l'agent : file de commandes et
-- politiques par conteneur.
--
-- Le serveur ne joint jamais l'agent : il dépose ici ce qu'il attend de lui, et
-- l'agent vient le lire après chaque lot de mesures. La table est donc à la fois
-- la file d'attente et le journal de ce qui a été fait.
CREATE TABLE agent_commands (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    target_id    INTEGER NOT NULL REFERENCES targets(id) ON DELETE CASCADE,
    -- `container.restart`, `container.update`.
    kind         TEXT    NOT NULL,
    -- Arguments de la commande, en JSON (`{"name": "…", "prune": true}`).
    args         TEXT    NOT NULL DEFAULT '{}',
    status       TEXT    NOT NULL DEFAULT 'queued'
                 CHECK (status IN ('queued', 'running', 'done', 'failed', 'cancelled')),
    -- Qui a demandé : un compte de l'interface, ou `policy` pour l'automate.
    requested_by TEXT,
    created_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    started_at   TEXT,
    finished_at  TEXT,
    -- Compte rendu de l'agent : extrait de journal, lisible tel quel.
    result       TEXT
);

CREATE INDEX idx_agent_commands_target ON agent_commands(target_id, id);
CREATE INDEX idx_agent_commands_status ON agent_commands(status);

-- Ce que l'automate a le droit de faire tout seul, conteneur par conteneur.
--
-- Tout est refusé par défaut : un conteneur ne se redémarre ni ne se met à jour
-- sans qu'on l'ait demandé explicitement dans l'interface. `only_in_maintenance`
-- réserve la mise à jour aux fenêtres de maintenance (silences) de la cible.
CREATE TABLE container_policies (
    target_id           INTEGER NOT NULL REFERENCES targets(id) ON DELETE CASCADE,
    container           TEXT    NOT NULL,
    auto_restart        INTEGER NOT NULL DEFAULT 0,
    auto_update         INTEGER NOT NULL DEFAULT 0,
    prune_old_image     INTEGER NOT NULL DEFAULT 1,
    only_in_maintenance INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (target_id, container)
);

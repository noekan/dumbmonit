-- Historique des tâches Proxmox Datacenter Manager et dernière vue de chaque sonde.
--
-- Le panneau d'une console de datacenter ne peut pas se lire dans
-- VictoriaMetrics : savoir qu'une instance fédérée est injoignable est une
-- série, savoir *pourquoi* demande le message que la console a reçu, et lister
-- les tâches en échec demande les tâches elles-mêmes. Le collecteur les livre à
-- chaque interrogation ; elles sont conservées ici pour que l'API les serve sans
-- réinterroger la console, et pour que la liste survive à un redémarrage.
--
-- La table est bornée : dix-neuf jours, et au plus cinq mille tâches par cible
-- (voir `db/pdm.rs`).
CREATE TABLE pdm_task_history (
    target_id   INTEGER NOT NULL REFERENCES targets(id) ON DELETE CASCADE,
    -- `RemoteUpid` complet, tel que PDM le donne : `site-b!UPID:…`.
    upid        TEXT    NOT NULL,
    -- Nom de l'instance fédérée, extrait de l'UPID ; vide pour une tâche locale.
    remote      TEXT    NOT NULL DEFAULT '',
    worker_type TEXT    NOT NULL DEFAULT '',
    worker_id   TEXT    NOT NULL DEFAULT '',
    node        TEXT,
    user        TEXT,
    starttime   INTEGER NOT NULL,
    endtime     INTEGER,
    -- `OK`, `WARNINGS: n`, `TASK ERROR: …` ; NULL tant que la tâche tourne.
    status      TEXT,
    PRIMARY KEY (target_id, upid)
);

CREATE INDEX idx_pdm_task_history_start ON pdm_task_history(target_id, starttime);

-- Ce que la dernière interrogation a vu (instances fédérées et leur état, totaux
-- du parc, hôte de la console), en JSON. Une ligne par cible, réécrite à chaque
-- interrogation réussie. Rien de ce qui est stocké ici n'est un secret.
CREATE TABLE pdm_probe_view (
    target_id INTEGER PRIMARY KEY REFERENCES targets(id) ON DELETE CASCADE,
    probed_at INTEGER NOT NULL,
    view      TEXT    NOT NULL
);

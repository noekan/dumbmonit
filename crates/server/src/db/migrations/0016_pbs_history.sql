-- Historique des tâches Proxmox Backup Server et dernière vue de chaque sonde.
--
-- Le calendrier des sauvegardes (trente jours, un point par jour et par
-- machine) ne peut pas se lire dans VictoriaMetrics : il lui faut les tâches
-- elles-mêmes — laquelle a échoué, quand, avec quel message. Le collecteur les
-- livre à chaque interrogation ; elles sont conservées ici pour que l'API les
-- serve sans réinterroger PBS, et pour que l'historique survive à un
-- redémarrage. La table est bornée : trente-cinq jours, et au plus dix mille
-- tâches par cible (voir `db/pbs.rs`).
CREATE TABLE pbs_task_history (
    target_id   INTEGER NOT NULL REFERENCES targets(id) ON DELETE CASCADE,
    upid        TEXT    NOT NULL,
    worker_type TEXT    NOT NULL,
    worker_id   TEXT    NOT NULL DEFAULT '',
    user        TEXT,
    starttime   INTEGER NOT NULL,
    endtime     INTEGER,
    -- `OK`, `WARNINGS: n`, `TASK ERROR: …` ; NULL tant que la tâche tourne.
    status      TEXT,
    PRIMARY KEY (target_id, upid)
);

CREATE INDEX idx_pbs_task_history_start ON pbs_task_history(target_id, starttime);

-- Ce que la dernière interrogation a vu (datastores, groupes de sauvegarde et
-- leurs instantanés, travaux planifiés, disques), en JSON. Une ligne par cible,
-- réécrite à chaque interrogation réussie.
CREATE TABLE pbs_probe_view (
    target_id INTEGER PRIMARY KEY REFERENCES targets(id) ON DELETE CASCADE,
    probed_at INTEGER NOT NULL,
    view      TEXT    NOT NULL
);

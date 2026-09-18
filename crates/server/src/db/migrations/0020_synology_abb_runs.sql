-- Historique des sauvegardes Active Backup for Business, par appareil.
--
-- Le NAS garde cet historique, mais le modèle de rythme (alerting/abb_rhythm.rs)
-- a besoin de trente jours d'exécutions par appareil à chaque évaluation : les
-- relire à chaque interrogation coûterait une requête lourde par minute, et un
-- redémarrage du serveur repartirait de zéro. Les lignes sont copiées ici au fil
-- des interrogations, et bornées à quatre-vingt-dix jours (db/abb_runs.rs).
--
-- Une ligne = une exécution pour un appareil (`device_result_id` d'ABB), telle
-- que renvoyée par SYNO.ActiveBackup.Overview list_device_transfer_size.
CREATE TABLE synology_abb_runs (
    target_id        INTEGER NOT NULL REFERENCES targets(id) ON DELETE CASCADE,
    device_id        INTEGER NOT NULL,
    device_result_id INTEGER NOT NULL,
    task_id          INTEGER NOT NULL DEFAULT 0,
    task_name        TEXT    NOT NULL DEFAULT '',
    result_id        INTEGER NOT NULL DEFAULT 0,
    device_name      TEXT    NOT NULL DEFAULT '',
    -- 2 réussite, 3 réussite partielle, 4 échec, 5 annulation, 6 sans sauvegarde.
    status           INTEGER NOT NULL,
    time_start       INTEGER NOT NULL,
    -- 0 tant que l'exécution est en cours.
    time_end         INTEGER NOT NULL DEFAULT 0,
    transfered_bytes INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (target_id, device_id, device_result_id)
);

CREATE INDEX idx_synology_abb_runs_device ON synology_abb_runs(target_id, device_id, time_start);

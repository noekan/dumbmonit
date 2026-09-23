-- Journal des sauvegardes locales planifiées (`crate::backup::local`).
--
-- Les fichiers sur disque font foi pour « ce qui est gardé » ; cette table dit
-- « ce qui s'est passé », y compris quand rien n'a été écrit. Sans elle, une
-- sauvegarde qui échoue chaque nuit pour un volume plein ne laisserait qu'une
-- ligne de journal que personne ne lit — et le répertoire aurait exactement
-- l'air d'un répertoire à jour, avec les fichiers de la semaine dernière.
CREATE TABLE backup_runs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    at          TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    ok          INTEGER NOT NULL DEFAULT 0 CHECK (ok IN (0, 1)),
    -- Nom du fichier écrit, vide en cas d'échec.
    file        TEXT    NOT NULL DEFAULT '',
    bytes       INTEGER NOT NULL DEFAULT 0,
    duration_ms INTEGER NOT NULL DEFAULT 0,
    -- Vrai quand `secret.key` a pu être copié à côté de la base. Faux quand le
    -- secret vient de l'environnement : la sauvegarde est alors incomplète à
    -- elle seule, et l'interface le dit.
    with_secret INTEGER NOT NULL DEFAULT 0 CHECK (with_secret IN (0, 1)),
    error       TEXT
);

CREATE INDEX idx_backup_runs_at ON backup_runs(at DESC);

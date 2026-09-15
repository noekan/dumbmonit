-- Pages de statut publiques : ce qu'un homelab montre à ses utilisateurs sans
-- leur ouvrir l'interface. Une page choisit des cibles, leur donne un nom
-- public et les range par groupe ; les incidents et fenêtres de maintenance
-- s'annoncent par-dessus.
--
-- Rien de ce qui est stocké ici n'est secret, mais la page publique ne renvoie
-- que ce qu'elle a explicitement choisi d'exposer : les identifiants et
-- adresses des cibles restent côté administration.
CREATE TABLE status_pages (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Segment d'URL de la page (`/s/<slug>`) : minuscules, chiffres, tirets.
    slug             TEXT    NOT NULL UNIQUE,
    title            TEXT    NOT NULL,
    description      TEXT    NOT NULL DEFAULT '',
    -- Une page non publiée répond 404 au public, comme si elle n'existait pas.
    published        INTEGER NOT NULL DEFAULT 0,
    theme            TEXT    NOT NULL DEFAULT 'auto' CHECK (theme IN ('auto', 'light', 'dark')),
    -- Profondeur de la barre d'historique affichée, en jours.
    show_uptime_days INTEGER NOT NULL DEFAULT 90,
    created_at       TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at       TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

-- Les services affichés par une page, dans l'ordre. `label` est le nom public ;
-- `group_name` vide signifie « sans groupe ».
CREATE TABLE status_page_items (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    page_id    INTEGER NOT NULL REFERENCES status_pages(id) ON DELETE CASCADE,
    target_id  INTEGER NOT NULL REFERENCES targets(id) ON DELETE CASCADE,
    label      TEXT    NOT NULL,
    group_name TEXT    NOT NULL DEFAULT '',
    position   INTEGER NOT NULL DEFAULT 0,
    UNIQUE (page_id, target_id)
);

CREATE INDEX idx_status_page_items_page ON status_page_items(page_id, position);

-- Incidents et maintenances annoncés. `page_id` NULL : l'annonce vaut pour
-- toutes les pages. Les statuts suivent deux cycles distincts selon le genre :
-- incident → investigating, identified, monitoring, resolved ;
-- maintenance → scheduled, in_progress, completed.
CREATE TABLE incidents (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    page_id    INTEGER REFERENCES status_pages(id) ON DELETE CASCADE,
    title      TEXT    NOT NULL,
    kind       TEXT    NOT NULL CHECK (kind IN ('incident', 'maintenance')),
    status     TEXT    NOT NULL CHECK (status IN (
                   'investigating', 'identified', 'monitoring', 'resolved',
                   'scheduled', 'in_progress', 'completed')),
    severity   TEXT    NOT NULL DEFAULT 'minor' CHECK (severity IN ('minor', 'major')),
    starts_at  TEXT    NOT NULL,
    -- Fin réelle (incident résolu) ou fin prévue (maintenance) ; NULL tant que
    -- l'incident est ouvert.
    ends_at    TEXT,
    created_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX idx_incidents_page ON incidents(page_id, starts_at);

-- Fil chronologique d'un incident : chaque message porte le statut auquel il
-- fait passer l'incident.
CREATE TABLE incident_updates (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    incident_id INTEGER NOT NULL REFERENCES incidents(id) ON DELETE CASCADE,
    status      TEXT    NOT NULL,
    body        TEXT    NOT NULL,
    created_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX idx_incident_updates_incident ON incident_updates(incident_id, id);

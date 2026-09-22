-- Dernière vue de chaque sonde Proxmox Mail Gateway.
--
-- Le tableau de bord d'une passerelle ne peut pas se lire entièrement dans
-- VictoriaMetrics : la ventilation d'une file d'attente par domaine de
-- destination, le nom des virus détectés dans la journée, la version de chaque
-- base de signatures et le détail de la grappe sont des libellés, pas des
-- nombres. Le collecteur les livre à chaque interrogation ; ils sont conservés
-- ici pour que l'API les serve sans réinterroger la passerelle, et pour que la
-- page reste lisible entre deux sondes.
--
-- Une ligne par cible, réécrite à chaque interrogation réussie : rien ne
-- s'accumule, et la cascade efface tout avec l'équipement.
CREATE TABLE pmg_probe_view (
    target_id INTEGER PRIMARY KEY REFERENCES targets(id) ON DELETE CASCADE,
    probed_at INTEGER NOT NULL,
    view      TEXT    NOT NULL
);

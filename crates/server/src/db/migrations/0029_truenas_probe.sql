-- Dernière vue de chaque sonde TrueNAS.
--
-- La page d'un NAS ne peut pas se lire entièrement dans VictoriaMetrics : le
-- nom du disque qui a lâché dans un vdev qui sert toujours les données, la
-- phrase de `zpool status`, le dernier instantané répliqué et le texte des
-- alertes que TrueNAS a lui-même levées sont des libellés, pas des nombres. Le
-- collecteur les livre à chaque interrogation ; ils sont conservés ici pour que
-- l'API les serve sans réinterroger le NAS.
--
-- Une ligne par cible, réécrite à chaque interrogation réussie : rien ne
-- s'accumule, et la cascade efface tout avec l'équipement.
CREATE TABLE truenas_probe_view (
    target_id INTEGER PRIMARY KEY REFERENCES targets(id) ON DELETE CASCADE,
    probed_at INTEGER NOT NULL,
    view      TEXT    NOT NULL
);

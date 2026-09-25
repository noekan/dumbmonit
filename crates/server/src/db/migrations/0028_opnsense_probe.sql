-- Dernière vue de chaque sonde OPNsense.
--
-- La page d'un pare-feu ne peut pas se lire entièrement dans VictoriaMetrics :
-- le nom et l'adresse de chaque passerelle, l'adresse publique du moment, la
-- version que le miroir propose, le nom du tunnel VPN qui ne se rétablit pas et
-- l'état CARP de la paire sont des libellés, pas des nombres. Le collecteur les
-- livre à chaque interrogation ; ils sont conservés ici pour que l'API les serve
-- sans réinterroger le pare-feu, et pour que la page reste lisible entre deux
-- sondes.
--
-- Une ligne par cible, réécrite à chaque interrogation réussie : rien ne
-- s'accumule, et la cascade efface tout avec l'équipement.
CREATE TABLE opnsense_probe_view (
    target_id INTEGER PRIMARY KEY REFERENCES targets(id) ON DELETE CASCADE,
    probed_at INTEGER NOT NULL,
    view      TEXT    NOT NULL
);

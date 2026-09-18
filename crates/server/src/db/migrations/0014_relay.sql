-- Agents relais : un agent posé dans un autre réseau interroge, à la place du
-- serveur, les équipements de son site.
--
-- `via_agent` désigne la cible « agent » qui relaie les sondes de celle-ci.
-- NULL — le cas de toutes les cibles existantes — signifie que le serveur
-- interroge lui-même : rien ne change pour elles. `SET NULL` à la suppression
-- de l'agent : l'équipement redevient interrogé en direct plutôt que de rester
-- orphelin d'un relais disparu.
ALTER TABLE targets ADD COLUMN via_agent INTEGER REFERENCES targets(id) ON DELETE SET NULL;

CREATE INDEX idx_targets_via_agent ON targets(via_agent);

-- Ce que l'agent déclare de lui-même à chaque lot : accepte-t-il de relayer
-- (`relay: true` dans sa configuration), et sur quel site est-il posé.
ALTER TABLE agent_hosts ADD COLUMN relay INTEGER NOT NULL DEFAULT 0;
ALTER TABLE agent_hosts ADD COLUMN site TEXT;

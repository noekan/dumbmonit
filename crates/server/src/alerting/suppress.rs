//! Suppression par dépendance.
//!
//! Quand le routeur tombe, les vingt machines derrière lui deviennent injoignables
//! en même temps. Notifier vingt et une fois pour une seule panne est le meilleur
//! moyen de faire ignorer les alertes ; on ne notifie donc que la cause racine, et
//! les descendants passent en `suppressed`.

use std::collections::{HashMap, HashSet};

use crate::alerting::model::{TargetId, TargetNode};

/// Profondeur maximale explorée en remontant la filiation.
///
/// La détection de cycle par `visited` suffit à garantir la terminaison ; cette
/// borne protège en plus d'une hiérarchie absurdement profonde importée par erreur.
const MAX_DEPTH: usize = 64;

/// Index de filiation, construit une fois par cycle.
///
/// Une cible a jusqu'à deux ascendants directs : son parent déclaré, et l'agent
/// relais qui l'interroge — si ce dernier tombe, l'équipement n'est plus observé,
/// et ses alertes n'ont pas plus de sens que derrière un routeur éteint.
pub struct Topology {
    parents: HashMap<TargetId, Vec<TargetId>>,
}

impl Topology {
    pub fn new(targets: &[TargetNode]) -> Self {
        Self {
            parents: targets
                .iter()
                .map(|t| {
                    let mut up: Vec<TargetId> = t.parent_id.into_iter().collect();
                    if let Some(relay) = t.via_agent
                        && !up.contains(&relay)
                    {
                        up.push(relay);
                    }
                    (t.id, up)
                })
                .collect(),
        }
    }

    /// Premier ancêtre strict présent dans `down`, en remontant la filiation.
    ///
    /// Renvoie l'ancêtre le plus proche, et non la racine : c'est lui qui explique
    /// le mieux la panne à l'utilisateur (« derrière le switch d'étage », pas
    /// « derrière la box »). À distance égale, le parent déclaré passe avant le
    /// relais.
    ///
    /// Un cycle dans les données — que la base autorise, `parent_id` n'étant qu'une
    /// clé étrangère vers la même table — ne fait jamais boucler : chaque nœud n'est
    /// visité qu'une fois.
    pub fn first_ancestor_down(
        &self,
        target: TargetId,
        down: &HashSet<TargetId>,
    ) -> Option<TargetId> {
        let mut visited = HashSet::new();
        visited.insert(target);

        // Parcours en largeur : la génération courante, puis la suivante.
        let mut generation: Vec<TargetId> = self.parents.get(&target)?.clone();
        for _ in 0..MAX_DEPTH {
            if generation.is_empty() {
                return None;
            }
            let mut next = Vec::new();
            for ancestor in generation {
                if !visited.insert(ancestor) {
                    // Déjà vu (cycle ou ascendant commun) : rien de neuf par là.
                    continue;
                }
                if down.contains(&ancestor) {
                    return Some(ancestor);
                }
                if let Some(up) = self.parents.get(&ancestor) {
                    next.extend(up.iter().copied());
                }
            }
            generation = next;
        }
        None
    }

    /// Vrai si la cible existe dans la topologie chargée.
    pub fn contains(&self, target: TargetId) -> bool {
        self.parents.contains_key(&target)
    }
}

/// Raison pour laquelle une alerte ne notifie pas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suppression {
    /// Un ancêtre est injoignable.
    AncestorDown(TargetId),
    /// La cible elle-même est injoignable : ses autres alertes ne sont que le
    /// symptôme de la même panne, et leurs valeurs sont de toute façon périmées.
    SelfDown,
}

impl Suppression {
    pub fn culprit(self, target: TargetId) -> TargetId {
        match self {
            Self::AncestorDown(ancestor) => ancestor,
            Self::SelfDown => target,
        }
    }
}

/// Décide si une alerte doit être supprimée.
///
/// `is_host_down_rule` évite l'auto-suppression : l'alerte « équipement
/// injoignable » d'une cible ne peut pas être étouffée par le fait que cette cible
/// soit injoignable, sans quoi plus personne ne serait jamais prévenu.
pub fn suppression_for(
    topology: &Topology,
    target: Option<TargetId>,
    is_host_down_rule: bool,
    down: &HashSet<TargetId>,
) -> Option<Suppression> {
    let target = target?;

    if let Some(ancestor) = topology.first_ancestor_down(target, down) {
        return Some(Suppression::AncestorDown(ancestor));
    }
    if !is_host_down_rule && down.contains(&target) {
        return Some(Suppression::SelfDown);
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn node(id: TargetId, parent: Option<TargetId>) -> TargetNode {
        TargetNode {
            id,
            name: format!("device-{id}"),
            address: format!("10.0.0.{id}"),
            parent_id: parent,
            via_agent: None,
            tags: BTreeMap::new(),
            enabled: true,
        }
    }

    /// box(1) → switch(2) → nas(3) → conteneur(4)
    fn chaine() -> Topology {
        Topology::new(&[node(1, None), node(2, Some(1)), node(3, Some(2)), node(4, Some(3))])
    }

    fn down(ids: &[TargetId]) -> HashSet<TargetId> {
        ids.iter().copied().collect()
    }

    #[test]
    fn le_relais_tombe_supprime_les_cibles_qu_il_interroge() {
        // agent(9) interroge nas(3) ; switch(2) est son parent déclaré.
        let mut relayed = node(3, Some(2));
        relayed.via_agent = Some(9);
        let topo = Topology::new(&[node(2, None), node(9, None), relayed, node(4, Some(3))]);

        assert_eq!(topo.first_ancestor_down(3, &down(&[9])), Some(9));
        // Le parent déclaré garde son rôle, et passe devant à distance égale.
        assert_eq!(topo.first_ancestor_down(3, &down(&[2])), Some(2));
        assert_eq!(topo.first_ancestor_down(3, &down(&[2, 9])), Some(2));
        // La descendance de la cible relayée est couverte aussi.
        assert_eq!(topo.first_ancestor_down(4, &down(&[9])), Some(9));
        assert_eq!(topo.first_ancestor_down(3, &down(&[])), None);
    }

    #[test]
    fn le_parent_direct_supprime_ses_enfants() {
        assert_eq!(chaine().first_ancestor_down(3, &down(&[2])), Some(2));
    }

    #[test]
    fn la_suppression_traverse_plusieurs_niveaux() {
        let topology = chaine();
        // La box tombe : le switch, le NAS et le conteneur sont tous supprimés.
        assert_eq!(topology.first_ancestor_down(2, &down(&[1])), Some(1));
        assert_eq!(topology.first_ancestor_down(3, &down(&[1])), Some(1));
        assert_eq!(topology.first_ancestor_down(4, &down(&[1])), Some(1));
    }

    #[test]
    fn l_ancetre_le_plus_proche_est_designe() {
        // Box et switch tombés : c'est le switch qu'on montre au NAS.
        assert_eq!(chaine().first_ancestor_down(3, &down(&[1, 2])), Some(2));
    }

    #[test]
    fn la_racine_n_est_supprimee_par_personne() {
        assert_eq!(chaine().first_ancestor_down(1, &down(&[1, 2, 3])), None);
    }

    #[test]
    fn rien_n_est_supprime_quand_tout_va_bien() {
        assert_eq!(chaine().first_ancestor_down(4, &down(&[])), None);
    }

    #[test]
    fn un_cycle_de_parente_ne_boucle_pas_indefiniment() {
        // 1 → 2 → 3 → 1 : données incohérentes, mais la base ne l'interdit pas.
        let topology = Topology::new(&[node(1, Some(3)), node(2, Some(1)), node(3, Some(2))]);
        assert_eq!(topology.first_ancestor_down(1, &down(&[])), None);
        assert_eq!(topology.first_ancestor_down(2, &down(&[])), None);
        // Et la détection reste correcte quand un nœud du cycle est effectivement down.
        assert_eq!(topology.first_ancestor_down(2, &down(&[1])), Some(1));
        assert_eq!(topology.first_ancestor_down(3, &down(&[1])), Some(1));
    }

    #[test]
    fn une_boucle_sur_soi_meme_ne_se_supprime_pas() {
        let topology = Topology::new(&[node(1, Some(1))]);
        assert_eq!(topology.first_ancestor_down(1, &down(&[1])), None);
    }

    #[test]
    fn un_parent_inconnu_interrompt_proprement_la_remontee() {
        // Le parent a été supprimé de la base entre deux chargements.
        let topology = Topology::new(&[node(5, Some(99))]);
        assert_eq!(topology.first_ancestor_down(5, &down(&[99])), Some(99));
        assert_eq!(topology.first_ancestor_down(5, &down(&[42])), None);
    }

    #[test]
    fn une_cible_injoignable_etouffe_ses_propres_alertes_secondaires() {
        let topology = chaine();
        assert_eq!(
            suppression_for(&topology, Some(3), false, &down(&[3])),
            Some(Suppression::SelfDown)
        );
    }

    #[test]
    fn l_alerte_injoignable_elle_meme_n_est_jamais_etouffee() {
        let topology = chaine();
        assert_eq!(suppression_for(&topology, Some(3), true, &down(&[3])), None);
        // Mais elle l'est si c'est le parent qui est tombé : une seule notification.
        assert_eq!(
            suppression_for(&topology, Some(3), true, &down(&[2, 3])),
            Some(Suppression::AncestorDown(2))
        );
    }

    #[test]
    fn une_serie_sans_cible_n_est_pas_supprimable() {
        assert_eq!(suppression_for(&chaine(), None, false, &down(&[1, 2, 3])), None);
    }
}

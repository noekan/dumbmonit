//! Représentation d'un OID sous forme d'arcs.
//!
//! `snmp2` expose l'`Oid` d'`asn1-rs`, dont la représentation interne est l'encodage
//! ASN.1. Travailler sur les arcs plutôt que sur ces octets rend le moteur de profils
//! testable sans réseau : on peut construire un OID à partir d'une chaîne, comparer
//! des préfixes et extraire un index de table sans jamais ouvrir de socket.

use std::fmt;
use std::str::FromStr;

use dumbmonit_proto::ProbeError;

/// Un identifiant d'objet, décomposé en arcs.
///
/// L'ordre dérivé sur `Vec<u64>` est lexicographique, ce qui coïncide exactement avec
/// l'ordre SNMP des OID : c'est ce qui permet de détecter un agent qui renvoie des
/// varbinds non croissantes et ferait boucler un walk indéfiniment.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectId(Vec<u64>);

impl ObjectId {
    pub fn new(arcs: Vec<u64>) -> Self {
        Self(arcs)
    }

    pub fn arcs(&self) -> &[u64] {
        &self.0
    }

    /// Vrai si `prefix` est un préfixe d'arcs de cet OID, ou lui est égal.
    pub fn starts_with(&self, prefix: &ObjectId) -> bool {
        self.0.starts_with(&prefix.0)
    }

    /// Vrai si cet OID est strictement à l'intérieur du sous-arbre `root`.
    ///
    /// C'est le test de fin de walk : dès qu'un OID renvoyé sort du sous-arbre
    /// demandé, l'agent a débordé sur la MIB suivante et il faut s'arrêter.
    pub fn is_inside(&self, root: &ObjectId) -> bool {
        self.0.len() > root.0.len() && self.starts_with(root)
    }

    /// Suffixe d'index d'une ligne de table : ce qui reste après l'OID de colonne.
    ///
    /// C'est la clé de jointure entre une métrique et ses étiquettes : `ifHCInOctets.3`
    /// et `ifName.3` partagent l'index « 3 ». Certaines tables ont un index composite
    /// (`prtMarkerSupplies` est indexée par `hrDeviceIndex.supplyIndex`), d'où un
    /// suffixe rendu sous forme de chaîne pointée et non d'entier.
    pub fn index_after(&self, base: &ObjectId) -> Option<String> {
        if !self.is_inside(base) {
            return None;
        }
        let mut index = String::new();
        for (position, arc) in self.0[base.0.len()..].iter().enumerate() {
            if position > 0 {
                index.push('.');
            }
            index.push_str(&arc.to_string());
        }
        Some(index)
    }

    /// Concatène un suffixe d'index à cet OID de colonne.
    pub fn with_index(&self, index: &str) -> Option<ObjectId> {
        let mut arcs = self.0.clone();
        for part in index.split('.') {
            arcs.push(part.parse().ok()?);
        }
        Some(ObjectId(arcs))
    }

    /// Conversion vers le type attendu par `snmp2`.
    pub fn to_snmp(&self) -> Result<snmp2::Oid<'static>, ProbeError> {
        snmp2::Oid::from(&self.0)
            .map_err(|error| ProbeError::Config(format!("OID \"{self}\" is unusable: {error:?}")))
    }

    /// Conversion depuis le type de `snmp2`.
    ///
    /// Renvoie `None` si un arc dépasse `u64` — cas théorique qu'aucun équipement réel
    /// ne produit, mais qui ne doit pas faire échouer toute une interrogation.
    pub fn from_snmp(oid: &snmp2::Oid<'_>) -> Option<Self> {
        Some(Self(oid.iter()?.collect()))
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (position, arc) in self.0.iter().enumerate() {
            if position > 0 {
                f.write_str(".")?;
            }
            write!(f, "{arc}")?;
        }
        Ok(())
    }
}

impl FromStr for ObjectId {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        // Les MIB s'écrivent indifféremment « .1.3.6.1 » ou « 1.3.6.1 ».
        let trimmed = raw.trim().trim_start_matches('.');
        if trimmed.is_empty() {
            return Err("OID vide".to_string());
        }
        let mut arcs = Vec::new();
        for part in trimmed.split('.') {
            let arc: u64 =
                part.parse().map_err(|_| format!("invalid arc \"{part}\" in OID \"{raw}\""))?;
            arcs.push(arc);
        }
        // `asn1-rs` code les deux premiers arcs sur un seul octet et impose donc
        // qu'ils existent ; refuser ici donne un message bien plus clair.
        if arcs.len() < 2 {
            return Err(format!("OID \"{raw}\" must have at least two arcs"));
        }
        Ok(Self(arcs))
    }
}

impl<'de> serde::Deserialize<'de> for ObjectId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepte_les_deux_ecritures() {
        let attendu = ObjectId::new(vec![1, 3, 6, 1, 2, 1, 1, 1, 0]);
        assert_eq!("1.3.6.1.2.1.1.1.0".parse::<ObjectId>().unwrap(), attendu);
        assert_eq!(".1.3.6.1.2.1.1.1.0".parse::<ObjectId>().unwrap(), attendu);
        assert_eq!(" 1.3.6.1.2.1.1.1.0 ".parse::<ObjectId>().unwrap(), attendu);
    }

    #[test]
    fn parse_refuse_les_saisies_invalides() {
        for invalide in ["", ".", "1", "1.3.a.1", "1..3", "-1.3"] {
            assert!(invalide.parse::<ObjectId>().is_err(), "« {invalide} » aurait dû échouer");
        }
    }

    #[test]
    fn affichage_est_reversible() {
        let brut = "1.3.6.1.4.1.9.9.13.1.3.1.3";
        assert_eq!(brut.parse::<ObjectId>().unwrap().to_string(), brut);
    }

    #[test]
    fn prefixe_compare_des_arcs_et_non_des_chiffres() {
        let base: ObjectId = "1.3.6.1.2.1.2".parse().unwrap();
        // 1.3.6.1.2.1.2 est un préfixe textuel de 1.3.6.1.2.1.21, mais pas un
        // préfixe d'arcs : c'est précisément le piège d'une comparaison de chaînes.
        let voisin: ObjectId = "1.3.6.1.2.1.21.1".parse().unwrap();
        assert!(!voisin.starts_with(&base));
        assert!(!voisin.is_inside(&base));

        let enfant: ObjectId = "1.3.6.1.2.1.2.2.1.10.3".parse().unwrap();
        assert!(enfant.starts_with(&base));
        assert!(enfant.is_inside(&base));
    }

    #[test]
    fn la_racine_n_est_pas_a_l_interieur_d_elle_meme() {
        let base: ObjectId = "1.3.6.1.2.1.2".parse().unwrap();
        assert!(base.starts_with(&base));
        assert!(!base.is_inside(&base));
    }

    #[test]
    fn index_simple_et_composite() {
        let colonne: ObjectId = "1.3.6.1.2.1.31.1.1.1.6".parse().unwrap();
        let cellule: ObjectId = "1.3.6.1.2.1.31.1.1.1.6.42".parse().unwrap();
        assert_eq!(cellule.index_after(&colonne).as_deref(), Some("42"));

        let supplies: ObjectId = "1.3.6.1.2.1.43.11.1.1.10".parse().unwrap();
        let cellule: ObjectId = "1.3.6.1.2.1.43.11.1.1.10.1.4".parse().unwrap();
        assert_eq!(cellule.index_after(&supplies).as_deref(), Some("1.4"));
    }

    #[test]
    fn index_absent_hors_du_sous_arbre() {
        let colonne: ObjectId = "1.3.6.1.2.1.31.1.1.1.6".parse().unwrap();
        let ailleurs: ObjectId = "1.3.6.1.2.1.31.1.1.1.7.42".parse().unwrap();
        assert_eq!(ailleurs.index_after(&colonne), None);
        assert_eq!(colonne.index_after(&colonne), None);
    }

    #[test]
    fn with_index_est_l_inverse_de_index_after() {
        let colonne: ObjectId = "1.3.6.1.2.1.43.11.1.1.10".parse().unwrap();
        let cellule = colonne.with_index("1.4").unwrap();
        assert_eq!(cellule.to_string(), "1.3.6.1.2.1.43.11.1.1.10.1.4");
        assert_eq!(cellule.index_after(&colonne).as_deref(), Some("1.4"));
        assert_eq!(colonne.with_index("a"), None);
    }

    #[test]
    fn l_ordre_derive_est_l_ordre_snmp() {
        let mut oids: Vec<ObjectId> =
            ["1.3.6.1.2.1.2.2.1.10.10", "1.3.6.1.2.1.2.2.1.10.2", "1.3.6.1.2.1.2.2.1.9.99"]
                .iter()
                .map(|raw| raw.parse().unwrap())
                .collect();
        oids.sort();
        let rendus: Vec<String> = oids.iter().map(ToString::to_string).collect();
        assert_eq!(
            rendus,
            vec!["1.3.6.1.2.1.2.2.1.9.99", "1.3.6.1.2.1.2.2.1.10.2", "1.3.6.1.2.1.2.2.1.10.10"]
        );
    }

    #[test]
    fn conversion_aller_retour_avec_snmp2() {
        let attendu: ObjectId = "1.3.6.1.2.1.31.1.1.1.6.7".parse().unwrap();
        let converti = attendu.to_snmp().unwrap();
        assert_eq!(ObjectId::from_snmp(&converti).unwrap(), attendu);
    }
}

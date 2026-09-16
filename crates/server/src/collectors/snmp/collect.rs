//! Application d'un profil à une session ouverte.
//!
//! C'est ici que se joue la jointure indexée : la valeur d'une métrique et celle de
//! ses étiquettes viennent de deux colonnes distinctes, parcourues séparément, et
//! seul le suffixe d'index de l'OID permet de les rapprocher.

use std::collections::{BTreeMap, HashMap};

use dumbmonit_proto::{ProbeError, Sample};
use tracing::{debug, warn};

use super::oid::ObjectId;
use super::profile::ResolvedMetric;
use super::session::Session;
use super::value::SnmpValue;

/// Une colonne de table parcourue, indexée par suffixe d'index.
type Column = HashMap<String, SnmpValue>;

/// Mémorise les colonnes déjà parcourues pendant une interrogation.
///
/// Sans ce cache, un profil IF-MIB parcourrait `ifName` une fois par métrique — onze
/// fois de suite la même table de quarante-huit lignes. C'est le facteur qui décide
/// qu'un switch tient ou non dans le délai d'interrogation.
#[derive(Default)]
struct WalkCache {
    columns: HashMap<ObjectId, Column>,
}

impl WalkCache {
    async fn column(
        &mut self,
        session: &mut Session,
        base: &ObjectId,
    ) -> Result<&Column, ProbeError> {
        if !self.columns.contains_key(base) {
            let varbinds = session.walk(base).await?;
            let mut column = Column::with_capacity(varbinds.len());
            for (oid, value) in varbinds {
                if let Some(index) = oid.index_after(base) {
                    column.insert(index, value);
                }
            }
            self.columns.insert(base.clone(), column);
        }
        Ok(&self.columns[base])
    }
}

/// Interroge la cible selon les métriques résolues et renvoie les échantillons.
///
/// Une métrique en échec n'interrompt pas les autres : un profil décrit une famille
/// d'équipements, et il est normal qu'un modèle donné n'implémente pas toute la MIB.
/// Seule une erreur qui condamne toute la session — délai dépassé, authentification
/// refusée — remonte.
pub async fn collect(
    session: &mut Session,
    metrics: &[ResolvedMetric],
    now_ms: i64,
) -> Result<Vec<Sample>, ProbeError> {
    let mut cache = WalkCache::default();
    let mut samples = Vec::new();

    let (scalars, tables): (Vec<_>, Vec<_>) =
        metrics.iter().partition(|resolved| !resolved.metric.walk);

    samples.extend(collect_scalars(session, &scalars, now_ms).await?);
    for resolved in tables {
        match collect_table(session, &mut cache, resolved, now_ms).await {
            Ok(produced) => samples.extend(produced),
            Err(error) if error.means_down() || matches!(error, ProbeError::Auth(_)) => {
                return Err(error);
            }
            Err(error) => {
                debug!(metric = resolved.metric.name, %error, "métrique ignorée");
            }
        }
    }

    Ok(samples)
}

/// Lit les métriques scalaires, en groupant les OID dans le moins de requêtes possible.
async fn collect_scalars(
    session: &mut Session,
    metrics: &[&ResolvedMetric],
    now_ms: i64,
) -> Result<Vec<Sample>, ProbeError> {
    if metrics.is_empty() {
        return Ok(Vec::new());
    }

    // Une même instance peut porter la valeur d'une métrique et l'étiquette d'une
    // autre — sysName en est le cas typique : une seule lecture suffit.
    let mut wanted: Vec<ObjectId> = Vec::new();
    for resolved in metrics {
        for oid in std::iter::once(&resolved.metric.oid)
            .chain(resolved.metric.labels.values())
            .chain(resolved.metric.multiply_by.iter())
        {
            if !wanted.contains(oid) {
                wanted.push(oid.clone());
            }
        }
    }

    let values: HashMap<ObjectId, SnmpValue> =
        session.get_many(&wanted).await?.into_iter().collect();

    let mut samples = Vec::new();
    for resolved in metrics {
        let metric = &resolved.metric;
        let Some(raw) = values.get(&metric.oid).and_then(SnmpValue::as_f64) else {
            continue;
        };
        let Some(value) =
            apply_factors(raw, metric.scale, metric.multiply_by.as_ref(), |oid| values.get(oid))
        else {
            continue;
        };

        let mut labels = metric.static_labels.clone();
        for (name, oid) in &metric.labels {
            if let Some(text) = values.get(oid).and_then(SnmpValue::as_label) {
                labels.insert(name.clone(), text);
            }
        }
        samples.push(build_sample(metric, labels, value, now_ms));
    }
    Ok(samples)
}

/// Lit une métrique tabulaire : parcours de la colonne, résolution des étiquettes
/// indexées, filtrage de cardinalité.
async fn collect_table(
    session: &mut Session,
    cache: &mut WalkCache,
    resolved: &ResolvedMetric,
    now_ms: i64,
) -> Result<Vec<Sample>, ProbeError> {
    let metric = &resolved.metric;

    // Les colonnes auxiliaires sont parcourues d'abord : le cache les partage entre
    // toutes les métriques du profil, et l'emprunt immuable qui suit l'exige.
    let mut auxiliary: Vec<ObjectId> = metric.labels.values().cloned().collect();
    auxiliary.extend(metric.multiply_by.iter().cloned());
    if let Some(filters) = &resolved.filters {
        auxiliary.extend(filters.required_oids().into_iter().cloned());
    }
    for base in &auxiliary {
        cache.column(session, base).await?;
    }

    let rows: Vec<(String, SnmpValue)> = cache
        .column(session, &metric.oid)
        .await?
        .iter()
        .map(|(index, value)| (index.clone(), value.clone()))
        .collect();

    let max_rows = resolved.max_rows();
    let mut samples = Vec::new();
    let mut dropped = 0usize;

    // L'ordre du parcours d'une table de hachage varie d'une exécution à l'autre ; le
    // tri rend la troncature de cardinalité déterministe, donc les séries stables.
    let mut rows = rows;
    rows.sort_by(|(left, _), (right, _)| compare_indexes(left, right));

    for (index, raw) in rows {
        let Some(raw) = raw.as_f64() else { continue };

        let mut labels = metric.static_labels.clone();
        labels.insert("index".to_string(), index.clone());
        for (name, base) in &metric.labels {
            if let Some(text) = cache
                .columns
                .get(base)
                .and_then(|column| column.get(&index))
                .and_then(SnmpValue::as_label)
            {
                labels.insert(name.clone(), text);
            }
        }

        if let Some(filters) = &resolved.filters
            && !filters.keeps(&labels, |base| {
                cache.columns.get(base).and_then(|column| column.get(&index))
            })
        {
            dropped += 1;
            continue;
        }

        let Some(value) = apply_factors(raw, metric.scale, metric.multiply_by.as_ref(), |base| {
            cache.columns.get(base).and_then(|column| column.get(&index))
        }) else {
            continue;
        };

        if samples.len() >= max_rows {
            warn!(metric = metric.name, max_rows, "table tronquée : affinez les filtres du profil");
            break;
        }
        samples.push(build_sample(metric, labels, value, now_ms));
    }

    debug!(metric = metric.name, kept = samples.len(), dropped, "table collectée");
    Ok(samples)
}

/// Applique le facteur d'échelle puis, s'il y a lieu, la colonne multiplicatrice.
fn apply_factors<'a>(
    raw: f64,
    scale: Option<f64>,
    multiply_by: Option<&ObjectId>,
    lookup: impl Fn(&ObjectId) -> Option<&'a SnmpValue>,
) -> Option<f64> {
    let mut value = raw * scale.unwrap_or(1.0);
    if let Some(base) = multiply_by {
        // Sans le facteur, la mesure serait exprimée dans une unité inconnue de
        // l'utilisateur — des blocs plutôt que des octets : mieux vaut ne rien émettre.
        value *= lookup(base)?.as_f64()?;
    }
    value.is_finite().then_some(value)
}

fn build_sample(
    metric: &super::profile::Metric,
    labels: BTreeMap<String, String>,
    value: f64,
    now_ms: i64,
) -> Sample {
    Sample { metric: metric.name.clone(), labels, value, kind: metric.kind.into(), ts_ms: now_ms }
}

/// Compare deux suffixes d'index arc par arc, et non caractère par caractère.
///
/// L'ordre lexicographique placerait l'interface 10 avant la 2 ; la troncature de
/// cardinalité écarterait alors des ports au hasard plutôt que les derniers.
fn compare_indexes(left: &str, right: &str) -> std::cmp::Ordering {
    let parse = |index: &str| -> Vec<u64> {
        index.split('.').map(|part| part.parse().unwrap_or(u64::MAX)).collect()
    };
    parse(left).cmp(&parse(right))
}

#[cfg(test)]
mod tests {
    use super::super::profile::{Catalog, Filters, Profile};
    use super::*;

    fn oid(raw: &str) -> ObjectId {
        raw.parse().unwrap()
    }

    /// Reproduit hors réseau la jointure indexée qu'opère `collect_table`.
    fn resolve_labels(
        columns: &HashMap<ObjectId, Column>,
        labels: &BTreeMap<String, ObjectId>,
        index: &str,
    ) -> BTreeMap<String, String> {
        let mut resolved = BTreeMap::new();
        resolved.insert("index".to_string(), index.to_string());
        for (name, base) in labels {
            if let Some(text) =
                columns.get(base).and_then(|column| column.get(index)).and_then(SnmpValue::as_label)
            {
                resolved.insert(name.clone(), text);
            }
        }
        resolved
    }

    fn colonne(entries: &[(&str, SnmpValue)]) -> Column {
        entries.iter().map(|(index, value)| ((*index).to_string(), value.clone())).collect()
    }

    #[test]
    fn la_jointure_indexee_associe_la_bonne_etiquette() {
        let if_name = oid("1.3.6.1.2.1.31.1.1.1.1");
        let if_alias = oid("1.3.6.1.2.1.31.1.1.1.18");
        let mut columns = HashMap::new();
        columns.insert(
            if_name.clone(),
            colonne(&[
                ("1", SnmpValue::Bytes(b"lo".to_vec())),
                ("2", SnmpValue::Bytes(b"eth0".to_vec())),
                ("48", SnmpValue::Bytes(b"Gi1/0/48".to_vec())),
            ]),
        );
        columns.insert(
            if_alias.clone(),
            colonne(&[("48", SnmpValue::Bytes(b"uplink coeur".to_vec()))]),
        );

        let mut wanted = BTreeMap::new();
        wanted.insert("ifname".to_string(), if_name);
        wanted.insert("ifalias".to_string(), if_alias);

        let labels = resolve_labels(&columns, &wanted, "48");
        assert_eq!(labels.get("ifname").map(String::as_str), Some("Gi1/0/48"));
        assert_eq!(labels.get("ifalias").map(String::as_str), Some("uplink coeur"));
        assert_eq!(labels.get("index").map(String::as_str), Some("48"));

        // Étiquette absente pour cet index : elle est simplement omise, la mesure
        // reste utilisable.
        let labels = resolve_labels(&columns, &wanted, "2");
        assert_eq!(labels.get("ifname").map(String::as_str), Some("eth0"));
        assert!(!labels.contains_key("ifalias"));

        // Index inconnu de toutes les colonnes d'étiquettes.
        let labels = resolve_labels(&columns, &wanted, "99");
        assert_eq!(labels.keys().collect::<Vec<_>>(), vec!["index"]);
    }

    #[test]
    fn un_index_composite_reste_une_cle_valide() {
        let description = oid("1.3.6.1.2.1.43.11.1.1.7");
        let mut columns = HashMap::new();
        columns.insert(
            description.clone(),
            colonne(&[
                ("1.1", SnmpValue::Bytes(b"Black Toner".to_vec())),
                ("1.4", SnmpValue::Bytes(b"Yellow Toner".to_vec())),
            ]),
        );
        let mut wanted = BTreeMap::new();
        wanted.insert("supply".to_string(), description);

        assert_eq!(
            resolve_labels(&columns, &wanted, "1.4").get("supply").map(String::as_str),
            Some("Yellow Toner")
        );
    }

    #[test]
    fn le_walk_alimente_la_colonne_par_index() {
        // Ce que fait `WalkCache::column` : découper l'OID complet en index.
        let base = oid("1.3.6.1.2.1.31.1.1.1.6");
        let varbinds = vec![
            (oid("1.3.6.1.2.1.31.1.1.1.6.1"), SnmpValue::Counter(10)),
            (oid("1.3.6.1.2.1.31.1.1.1.6.2"), SnmpValue::Counter(20)),
        ];
        let column: Column = varbinds
            .into_iter()
            .filter_map(|(oid, value)| Some((oid.index_after(&base)?, value)))
            .collect();
        assert_eq!(column.get("1"), Some(&SnmpValue::Counter(10)));
        assert_eq!(column.get("2"), Some(&SnmpValue::Counter(20)));
        assert_eq!(column.len(), 2);
    }

    #[test]
    fn les_facteurs_sont_appliques_dans_l_ordre() {
        let unit = oid("1.3.6.1.2.1.25.2.3.1.4");
        let block_size = SnmpValue::Integer(4096);

        // 2 048 blocs de 4 Kio = 8 Mio.
        let value = apply_factors(2048.0, None, Some(&unit), |asked| {
            (*asked == unit).then_some(&block_size)
        });
        assert_eq!(value, Some(8_388_608.0));

        // Centièmes de seconde vers secondes.
        assert_eq!(apply_factors(360_000.0, Some(0.01), None, |_| None), Some(3600.0));

        // Colonne multiplicatrice absente : rien n'est émis plutôt qu'une unité fausse.
        assert_eq!(apply_factors(2048.0, None, Some(&unit), |_| None), None);
    }

    #[test]
    fn les_index_se_trient_numeriquement() {
        let mut indexes = vec!["10", "2", "1.4", "1.10", "1.2"];
        indexes.sort_by(|left, right| compare_indexes(left, right));
        assert_eq!(indexes, vec!["1.2", "1.4", "1.10", "2", "10"]);
    }

    #[test]
    fn le_filtre_s_applique_apres_la_resolution_des_etiquettes() {
        // L'ordre compte : un filtre sur « ifname » ne peut agir que si l'étiquette a
        // déjà été résolue par la jointure indexée.
        let source = r#"
id: t
name: T
metrics: []
filters:
  drop_when_label_matches:
    ifname: ["^veth"]
"#;
        let filters: Filters = Profile::parse(source).unwrap().filters.unwrap();
        let if_name = oid("1.3.6.1.2.1.31.1.1.1.1");
        let mut columns = HashMap::new();
        columns.insert(
            if_name.clone(),
            colonne(&[
                ("1", SnmpValue::Bytes(b"eth0".to_vec())),
                ("2", SnmpValue::Bytes(b"veth9f2a".to_vec())),
            ]),
        );
        let mut wanted = BTreeMap::new();
        wanted.insert("ifname".to_string(), if_name);

        let gardes: Vec<&str> = ["1", "2"]
            .into_iter()
            .filter(|index| {
                let labels = resolve_labels(&columns, &wanted, index);
                filters.keeps(&labels, |_| None)
            })
            .collect();
        assert_eq!(gardes, vec!["1"]);
    }

    #[test]
    fn les_profils_livres_se_resolvent_tous() {
        let catalog: &Catalog = super::super::profile::embedded();
        assert!(!catalog.ids().is_empty(), "le catalogue embarqué est vide");
        for id in catalog.ids() {
            let metrics = catalog
                .resolve(id)
                .unwrap_or_else(|error| panic!("profil « {id} » irrésoluble : {error}"));
            assert!(!metrics.is_empty(), "profil « {id} » sans métrique");
        }
    }
}

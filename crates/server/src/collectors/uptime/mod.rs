//! Moniteurs de disponibilité : HTTP, TCP, DNS, ICMP et expiration de certificat.
//!
//! Là où les collecteurs SNMP, Proxmox ou l'agent surveillent des *équipements*,
//! ceux-ci surveillent des *services* : une page web répond-elle, un port
//! s'ouvre-t-il, un nom se résout-il vers la bonne adresse, un certificat va-t-il
//! expirer. C'est l'équivalent d'Uptime Kuma, branché sur le planificateur, les
//! alertes et les graphiques déjà en place.
//!
//! # Un collecteur par type de sonde
//!
//! Chaque sonde est un [`Collector`](dumbmonit_proto::Collector) à part entière —
//! `http`, `tcp`, `dns`, `ping`, `tls` — plutôt qu'un collecteur unique
//! paramétré. Elle hérite ainsi sans rien écrire du planificateur, de l'intervalle
//! par cible, de la découverte, des alertes et de l'interface ; et l'utilisateur
//! choisit un type dans une liste au lieu de deviner une étiquette.
//!
//! # Le point de conception : `probe_success`
//!
//! Toutes les sondes émettent `probe_success` (0 ou 1) **en plus** du `up` posé par
//! le registre, et renvoient `Ok` même quand le service est en panne. Le raisonnement
//! complet est dans [`outcome`], mais il tient en deux phrases : un service qui
//! répond 500 est en panne sans être injoignable, et un taux de disponibilité se
//! calcule sur des points écrits, pas sur des trous dans la série. `Err` est réservé
//! aux erreurs de configuration de l'utilisateur.
//!
//! ```text
//! # Taux de disponibilité sur trente jours, par cible :
//! avg_over_time(dumbmonit_probe_success[30d])
//! ```
//!
//! # Métriques produites
//!
//! Communes à toutes les sondes, étiquetées `probe="http|tcp|dns|ping|tls"` :
//!
//! | Métrique | Unité | Sens |
//! |---|---|---|
//! | `probe_success` | 0 / 1 | Le service a répondu correctement. |
//! | `probe_duration_seconds` | s | Durée totale de la sonde. |
//! | `probe_failure_info` | 1 | Présence ; l'étiquette `reason` porte le motif. |
//!
//! Selon la sonde :
//!
//! | Métrique | Sondes | Sens |
//! |---|---|---|
//! | `probe_http_status_code` | http | Code de statut obtenu. |
//! | `probe_http_first_byte_seconds` | http | Délai jusqu'aux en-têtes de réponse. |
//! | `probe_http_content_bytes` | http | Taille du corps. |
//! | `probe_connect_seconds` | tcp, tls, http | Établissement de la connexion TCP. |
//! | `probe_tls_handshake_seconds` | tls, http | Négociation TLS seule. |
//! | `probe_tls_version_info` | tls, http | Présence ; étiquette `version`. |
//! | `probe_ssl_cert_expiry_days` | tls, http | Jours avant expiration, négatif si périmé. |
//! | `probe_ssl_cert_valid` | tls, http | La chaîne remonte à une autorité connue. |
//! | `probe_ssl_cert_issuer_info` | tls, http | Présence ; étiquette `issuer`. |
//! | `probe_dns_lookup_seconds` | dns | Temps de résolution. |
//! | `probe_dns_answer_records` | dns | Enregistrements du type demandé. |
//! | `probe_icmp_rtt_seconds` | ping | Aller-retour moyen. |
//! | `probe_icmp_rtt_min_seconds` | ping | Aller-retour le plus court. |
//! | `probe_icmp_rtt_max_seconds` | ping | Aller-retour le plus long. |
//! | `probe_icmp_packet_loss_ratio` | ping | Perte entre 0 et 1. |
//! | `probe_icmp_packets_sent` | ping | Échos partis. |
//! | `probe_icmp_packets_received` | ping | Échos revenus. |
//!
//! Toutes sont des jauges : rien ici n'est cumulatif, chaque interrogation mesure
//! un instant. Le préfixe `dumbmonit_` est ajouté à l'écriture, comme partout.
//!
//! Les étiquettes d'identité restent de faible cardinalité : `probe` prend cinq
//! valeurs, `reason` onze, `record_type` dix, et `url`, `port`, `server_name`,
//! `resolver` en prennent une par cible. S'y ajoutent `target`, `host` et les
//! `tag_*` posés par le registre.

mod dns;
mod http;
mod outcome;
mod ping;
pub(crate) mod tags;
mod tcp;
mod tls;

pub use dns::DnsCollector;
pub use http::HttpCollector;
pub use ping::PingCollector;
pub use tcp::TcpCollector;
pub use tls::TlsCollector;

#[cfg(test)]
mod tests {
    use dumbmonit_proto::Collector;

    use super::*;

    /// Les types sont écrits en toutes lettres dans le `main.rs`, dans l'interface
    /// et dans les cibles enregistrées en base : les renommer casserait des cibles
    /// existantes sans que rien ne le signale.
    #[test]
    fn les_types_de_sonde_sont_ceux_annonces() {
        assert_eq!(HttpCollector::new().kind(), "http");
        assert_eq!(TcpCollector::new().kind(), "tcp");
        assert_eq!(DnsCollector::new().kind(), "dns");
        assert_eq!(PingCollector::new().kind(), "ping");
        assert_eq!(TlsCollector::new().kind(), "tls");
    }

    #[test]
    fn chaque_sonde_a_son_type_bien_a_elle() {
        let kinds = [
            HttpCollector::new().kind(),
            TcpCollector::new().kind(),
            DnsCollector::new().kind(),
            PingCollector::new().kind(),
            TlsCollector::new().kind(),
        ];
        let mut uniques = kinds.to_vec();
        uniques.sort_unstable();
        uniques.dedup();
        assert_eq!(uniques.len(), kinds.len(), "deux sondes se disputent le même type");
    }
}

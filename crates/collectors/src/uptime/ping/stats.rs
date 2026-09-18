//! Agrégation des envois ICMP, et traduction des refus de socket brute.
//!
//! Module purement fonctionnel, y compris la partie la plus importante pour
//! l'utilisateur : le message affiché quand le conteneur n'a pas le droit d'ouvrir
//! une socket ICMP.

use std::io;
use std::time::Duration;

use dumbmonit_proto::ProbeError;

/// Ce qu'une salve d'échos a donné.
#[derive(Debug, Default, Clone)]
pub struct Burst {
    /// Paquets réellement partis. Peut être inférieur au nombre demandé si le
    /// budget de temps de la sonde s'est épuisé en route.
    sent: u32,
    /// Temps d'aller-retour des réponses reçues, en secondes.
    rtt_seconds: Vec<f64>,
}

impl Burst {
    pub fn record_sent(&mut self) {
        self.sent += 1;
    }

    pub fn record_reply(&mut self, rtt: Duration) {
        self.rtt_seconds.push(rtt.as_secs_f64());
    }

    pub fn sent(&self) -> u32 {
        self.sent
    }

    pub fn received(&self) -> u32 {
        self.rtt_seconds.len() as u32
    }

    /// Taux de perte entre 0 et 1.
    ///
    /// Aucun paquet parti vaut perte totale : le budget de temps s'est épuisé sans
    /// qu'un seul écho ne sorte, ce qui est un échec, pas une absence de mesure.
    pub fn loss_ratio(&self) -> f64 {
        if self.sent == 0 {
            return 1.0;
        }
        f64::from(self.sent - self.received()) / f64::from(self.sent)
    }

    /// Moyenne des temps d'aller-retour, `None` si rien n'est revenu.
    pub fn mean_rtt(&self) -> Option<f64> {
        if self.rtt_seconds.is_empty() {
            return None;
        }
        Some(self.rtt_seconds.iter().sum::<f64>() / self.rtt_seconds.len() as f64)
    }

    pub fn min_rtt(&self) -> Option<f64> {
        self.rtt_seconds.iter().copied().reduce(f64::min)
    }

    pub fn max_rtt(&self) -> Option<f64> {
        self.rtt_seconds.iter().copied().reduce(f64::max)
    }
}

/// Traduit l'impossibilité d'ouvrir la socket ICMP.
///
/// Sans cette traduction, l'utilisateur lit « Permission denied (os error 13) » et
/// conclut naturellement que son équipement est éteint — alors que c'est le
/// conteneur DumbMonit qui n'a pas le droit d'émettre un écho. Le message dit donc
/// exactement quoi ajouter et où.
pub fn socket_error(error: &io::Error) -> ProbeError {
    if error.kind() != io::ErrorKind::PermissionDenied {
        return ProbeError::Config(format!("ICMP socket unusable: {error}"));
    }

    ProbeError::Config(
        "The DumbMonit container is not allowed to send ICMP packets. \
         Add the NET_RAW capability to the \"dumbmonit\" service in docker-compose.yml:\n\
         \n    cap_add:\n      - NET_RAW\n\n\
         then restart with \"docker compose up -d\". Alternatively, without an extra \
         capability, allow unprivileged ICMP echoes on the host with \
         \"sysctl -w net.ipv4.ping_group_range='0 2147483647'\". \
         Until one of the two is done, \"ping\" probes cannot work; TCP and HTTP probes \
         need nothing."
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn burst(sent: u32, rtts_ms: &[u64]) -> Burst {
        let mut burst = Burst::default();
        for _ in 0..sent {
            burst.record_sent();
        }
        for ms in rtts_ms {
            burst.record_reply(Duration::from_millis(*ms));
        }
        burst
    }

    #[test]
    fn une_salve_complete_ne_perd_rien() {
        let burst = burst(4, &[10, 20, 30, 40]);
        assert_eq!(burst.sent(), 4);
        assert_eq!(burst.received(), 4);
        assert!((burst.loss_ratio() - 0.0).abs() < 1e-12);
        assert!((burst.mean_rtt().unwrap() - 0.025).abs() < 1e-12);
        assert!((burst.min_rtt().unwrap() - 0.010).abs() < 1e-12);
        assert!((burst.max_rtt().unwrap() - 0.040).abs() < 1e-12);
    }

    #[test]
    fn la_perte_partielle_se_calcule_sur_les_paquets_reellement_envoyes() {
        let burst = burst(4, &[10, 30]);
        assert!((burst.loss_ratio() - 0.5).abs() < 1e-12);
        assert!((burst.mean_rtt().unwrap() - 0.020).abs() < 1e-12);
    }

    #[test]
    fn une_perte_totale_ne_produit_aucun_temps_daller_retour() {
        let burst = burst(4, &[]);
        assert!((burst.loss_ratio() - 1.0).abs() < 1e-12);
        assert_eq!(burst.mean_rtt(), None);
        assert_eq!(burst.min_rtt(), None);
        assert_eq!(burst.max_rtt(), None);
    }

    #[test]
    fn aucun_paquet_envoye_compte_comme_perte_totale() {
        assert!((Burst::default().loss_ratio() - 1.0).abs() < 1e-12);
    }

    /// Le test le plus utile du module : sans ce message, l'utilisateur croit à
    /// une panne d'équipement alors que c'est sa configuration Docker qui manque.
    #[test]
    fn un_refus_de_socket_brute_explique_quoi_ajouter_au_docker_compose() {
        let error = socket_error(&io::Error::from(io::ErrorKind::PermissionDenied));

        assert!(matches!(error, ProbeError::Config(_)), "ce n'est pas une panne d'équipement");
        assert!(!error.means_down(), "cela ne doit pas déclencher « équipement injoignable »");

        let message = error.to_string();
        assert!(message.contains("cap_add"), "{message}");
        assert!(message.contains("NET_RAW"), "{message}");
        assert!(message.contains("docker-compose.yml"), "{message}");
        assert!(message.contains("net.ipv4.ping_group_range"), "{message}");
    }

    #[test]
    fn les_autres_defauts_de_socket_restent_des_erreurs_de_configuration() {
        let error = socket_error(&io::Error::from(io::ErrorKind::AddrNotAvailable));
        assert!(matches!(error, ProbeError::Config(_)));
        assert!(!error.to_string().contains("NET_RAW"), "pas de conseil hors sujet");
    }
}

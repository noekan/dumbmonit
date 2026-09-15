# Moniteurs de disponibilité

L'équivalent d'Uptime Kuma dans EzyMonit : surveiller des **services** (une page
web, un port, un nom DNS, un hôte, un certificat) et non seulement des
**équipements**. Cinq collecteurs autonomes, qui ne dépendent que de
`ezymonit-proto`, `reqwest`, `tokio`, `serde`, `chrono`, `hickory-resolver`,
`surge-ping`, `x509-parser`, `rustls` / `tokio-rustls` / `webpki-roots`.

## Câblage

Deux endroits à compléter en dehors de ce répertoire.

* `crates/server/src/collectors/mod.rs`, sous `mod proxmox;` :

  ```rust
  mod uptime;
  ```

  puis, sous `pub use proxmox::ProxmoxCollector;` :

  ```rust
  pub use uptime::{DnsCollector, HttpCollector, PingCollector, TcpCollector, TlsCollector};
  ```

* `crates/server/src/main.rs`, à côté des autres enregistrements :

  ```rust
  registry.register(Arc::new(collectors::HttpCollector::new()));
  registry.register(Arc::new(collectors::TcpCollector::new()));
  registry.register(Arc::new(collectors::DnsCollector::new()));
  registry.register(Arc::new(collectors::PingCollector::new()));
  registry.register(Arc::new(collectors::TlsCollector::new()));
  ```

## `probe_success`, et pourquoi il ne remplace pas `up`

`Registry::probe` pose `up = 1` quand un collecteur renvoie `Ok`, et n'écrit rien
quand il renvoie `Err` : la règle « équipement injoignable » détecte l'interruption
de la série. Ce modèle convient au matériel, pas à un moniteur de disponibilité.

1. **Un service peut répondre et être en panne.** Un `500`, un mot-clé disparu, un
   certificat périmé : le serveur répond, `Err` serait faux, et pourtant le service
   ne rend pas son office.
2. **Un pourcentage de disponibilité se calcule sur des points écrits.**
   `avg_over_time` ignore les intervalles sans échantillon : si une panne se
   traduisait par l'absence d'écriture, une coupure de trois jours laisserait le
   taux à 100 %.

D'où la règle appliquée par les cinq sondes :

| Situation | Retour | Effet |
|---|---|---|
| Option ou adresse invalide, `CAP_NET_RAW` absent | `Err(ProbeError::Config)` | Rien n'est écrit, l'interface affiche l'erreur, aucune alerte de panne. |
| Connexion refusée, délai dépassé, TLS refusé, `500`, mot-clé absent, perte totale | `Ok` avec `probe_success = 0` | La panne est enregistrée, datée et comptée. |
| Tout va bien | `Ok` avec `probe_success = 1` | — |

Conséquence assumée : sur ces cibles, `up = 1` signifie « le moniteur a tourné », et
non « le service va bien ». Les deux signaux sont alors distincts au lieu d'être
confondus : `probe_success = 0` dit que le **service** est tombé, l'interruption de
`ezymonit_up` dit que la **surveillance** est tombée. L'interface doit donc afficher
l'état de ces cibles d'après `probe_success`.

C'est aussi pourquoi chaque sonde a son propre délai (`timeout_seconds`, cinq
secondes par défaut), plus court que `EZYMONIT_PROBE_TIMEOUT_SECS` : interrompue par
le registre, elle n'écrirait pas son zéro.

```
# Taux de disponibilité sur trente jours, par cible :
avg_over_time(ezymonit_probe_success[30d])
```

## Les cinq sondes

| `kind` | Adresse | Ce qui est vérifié |
|---|---|---|
| `http` | `https://exemple.fr/sante` | Statut, mot-clé, valeur JSON, certificat. |
| `tcp` | `nas.lan:445` | Ouverture de connexion. |
| `dns` | `www.exemple.fr` | Résolution, et valeur résolue. |
| `ping` | `10.0.0.1` | Aller-retour et perte de paquets. |
| `tls` | `mail.exemple.fr:993` | Expiration et validité du certificat. |

### `http`

Sans schéma, `https` est retenu. L'authentification vient de l'identifiant de la
cible — `UsernamePassword` produit une authentification basique, `ApiToken` un jeton
porteur — et jamais d'une étiquette : les étiquettes sont recopiées en clair sur
chaque série.

| Étiquette | Défaut | Rôle |
|---|---|---|
| `method` | `GET` | Méthode HTTP. |
| `accepted_status` | `200-299` | Codes normaux : `200-299,301,404`. |
| `keyword` | — | Texte cherché dans le corps. |
| `keyword_absent` | `false` | Le mot-clé doit être **absent** (page d'erreur qui répond 200). |
| `keyword_case_sensitive` | `false` | Respecte la casse. |
| `json_path` | — | Chemin à extraire : `$.etat`, `services[0].sain`. |
| `json_expect` | — | Valeur attendue à ce chemin. |
| `headers` | — | `Nom: valeur`, séparés par `\|` ou par des retours à la ligne. |
| `body` | — | Corps de la requête. |
| `follow_redirects` | `true` | Suit les redirections. |
| `max_redirects` | `10` | Nombre maximal de redirections suivies (1 à 20). |
| `insecure_tls` | `false` | Accepte un certificat non vérifiable. |
| `check_certificate` | `true` | Relève le certificat (HTTPS seulement). |
| `max_body_bytes` | `524288` | Corps rapatrié au plus. |
| `user_agent` | `EzyMonit/…` | En-tête `User-Agent`. |
| `timeout_seconds` | `5` | Budget total de la sonde (1 à 60). |

### `tcp`

| Étiquette | Défaut | Rôle |
|---|---|---|
| `port` | — | Port, si l'adresse n'en précise pas. Obligatoire dans l'un des deux. |
| `timeout_seconds` | `5` | Délai propre à la sonde. |

### `dns`

| Étiquette | Défaut | Rôle |
|---|---|---|
| `record_type` | `A` | `A`, `AAAA`, `CNAME`, `MX`, `TXT`, `NS`, `SOA`, `SRV`, `PTR`, `CAA`. |
| `resolver` | système | Adresse **IP** du résolveur, port facultatif : `1.1.1.1`, `10.0.0.1:5353`. |
| `expect` | — | Valeurs devant toutes figurer dans la réponse, séparées par des virgules. |
| `timeout_seconds` | `5` | Délai propre à la sonde. |

### `ping`

| Étiquette | Défaut | Rôle |
|---|---|---|
| `count` | `4` | Nombre d'échos (1 à 20). |
| `packet_timeout_ms` | `1000` | Attente d'une réponse, par paquet. |
| `interval_ms` | `100` | Espacement entre deux envois. |
| `payload_bytes` | `56` | Taille de la charge utile. |
| `ip_version` | `auto` | `auto`, `4` ou `6`. |
| `loss_threshold_percent` | `100` | Perte au-delà de laquelle la sonde échoue. |
| `timeout_seconds` | `5` | Budget total. Refusé si `count × packet_timeout_ms` le dépasse. |

**Le ping demande une capacité supplémentaire.** L'image finale est construite depuis
`scratch` et n'en reçoit aucune : il faut compléter le `docker-compose.yml`.

```yaml
services:
  ezymonit:
    cap_add:
      - NET_RAW
```

À défaut, `sysctl -w net.ipv4.ping_group_range="0 2147483647"` sur l'hôte autorise
les échos non privilégiés. Sans l'un des deux, la sonde renvoie un
`ProbeError::Config` qui dit exactement cela — et non un « Permission denied » qui
ferait croire à un équipement éteint.

### `tls`

Distincte de `http` parce que tout ce qui présente un certificat ne parle pas HTTP
(SMTPS 465, IMAPS 993, LDAPS 636, MQTTS 8883), parce qu'on ne veut pas toujours
envoyer du trafic applicatif sur un service surveillé, et parce que le terminateur
TLS répond encore quand l'application est tombée.

| Étiquette | Défaut | Rôle |
|---|---|---|
| `server_name` | l'hôte | Nom envoyé en SNI, à préciser derrière un proxy inverse interrogé par IP. |
| `insecure_tls` | `false` | Une chaîne non vérifiable ne fait plus échouer la sonde (autorité privée). |
| `timeout_seconds` | `5` | Délai propre à la sonde. |

La poignée de main aboutit même si le certificat est refusé — c'est ce qui permet de
lire la date d'expiration d'un certificat justement périmé. Aucune donnée applicative
n'y transite ; le verdict de la vérification ressort dans `probe_ssl_cert_valid`.

## Métriques

Toutes des jauges, toutes préfixées `ezymonit_` à l'écriture, toutes étiquetées
`probe="http|tcp|dns|ping|tls"` en plus de `target`, `host` et des `tag_*` posés par
le registre.

| Métrique | Sondes | Sens |
|---|---|---|
| `probe_success` | toutes | 1 si le service répond correctement, 0 sinon. |
| `probe_duration_seconds` | toutes | Durée totale de la sonde. |
| `probe_failure_info` | toutes | Présence (1) ; étiquette `reason`. |
| `probe_http_status_code` | http | Code de statut obtenu. |
| `probe_http_first_byte_seconds` | http | Délai jusqu'aux en-têtes de réponse. |
| `probe_http_content_bytes` | http | Taille du corps. |
| `probe_connect_seconds` | tcp, tls, http | Établissement de la connexion TCP. |
| `probe_tls_handshake_seconds` | tls, http | Négociation TLS seule. |
| `probe_tls_version_info` | tls, http | Présence ; étiquette `version`. |
| `probe_ssl_cert_expiry_days` | tls, http | Jours avant expiration, négatif si périmé. |
| `probe_ssl_cert_valid` | tls, http | 1 si la chaîne remonte à une autorité connue. |
| `probe_ssl_cert_issuer_info` | tls, http | Présence ; étiquette `issuer`. |
| `probe_dns_lookup_seconds` | dns | Temps de résolution. |
| `probe_dns_answer_records` | dns | Enregistrements du type demandé. |
| `probe_icmp_rtt_seconds` | ping | Aller-retour moyen. |
| `probe_icmp_rtt_min_seconds` | ping | Aller-retour le plus court. |
| `probe_icmp_rtt_max_seconds` | ping | Aller-retour le plus long. |
| `probe_icmp_packet_loss_ratio` | ping | Perte entre 0 et 1. |
| `probe_icmp_packets_sent` | ping | Échos partis. |
| `probe_icmp_packets_received` | ping | Échos revenus. |

Étiquettes supplémentaires, de faible cardinalité : `url`, `port`, `server_name`,
`record_type`, `resolver` (une valeur par cible), `reason` (onze valeurs), `version`
et `issuer`.

Valeurs de `reason` : `dns`, `connect`, `timeout`, `tls`, `cert_expired`, `status`,
`keyword`, `json`, `body`, `packet_loss`, `record`.

## Règles d'alerte à livrer

```
Service indisponible          ezymonit_probe_success == 0            for 2m   critique
Certificat bientôt expiré     ezymonit_probe_ssl_cert_expiry_days    < 14     avertissement
Certificat expiré             ezymonit_probe_ssl_cert_expiry_days    < 0      critique
Perte de paquets              ezymonit_probe_icmp_packet_loss_ratio  > 0.2    for 10m  avertissement
Réponse lente                 ezymonit_probe_duration_seconds        > 2      for 15m  info
```

## Organisation

| Fichier | Rôle |
|---|---|
| `mod.rs` | Documentation d'ensemble et réexport des cinq collecteurs. |
| `outcome.rs` | Le rapport de sonde et la décision `probe_success`. Pur. |
| `tags.rs` | Lecture des étiquettes, découpage `hôte:port`. Pur. |
| `http/mod.rs` | Le collecteur HTTP : requête, budget de temps, classement des échecs. |
| `http/options.rs` | Lecture des options HTTP, normalisation de l'URL. |
| `http/check.rs` | Plages de statuts, mot-clé, chemin JSON. Pur. |
| `tcp.rs` | Le collecteur TCP, options comprises. |
| `dns/mod.rs` | Le collecteur DNS et la construction du résolveur. |
| `dns/options.rs` | Lecture des options DNS. |
| `dns/answer.rs` | Types d'enregistrement, normalisation, valeurs attendues. Pur. |
| `ping/mod.rs` | Le collecteur ICMP : résolution, socket, salve. |
| `ping/options.rs` | Options ICMP et vérification du budget de temps. |
| `ping/stats.rs` | Perte, aller-retours, traduction du refus de socket brute. Pur. |
| `tls/mod.rs` | Le collecteur TLS, partagé avec la sonde HTTP. |
| `tls/options.rs` | Lecture des options TLS. |
| `tls/handshake.rs` | La seule poignée de main : vérificateur qui note au lieu d'appliquer. |
| `tls/cert.rs` | Dates de validité, jours restants. Pur, testé sur certificats en constante. |

Tout ce qui est testable l'est sans réseau : les modules purs couvrent la lecture des
options, la recherche de mot-clé, l'extraction JSON, les plages de statuts, le calcul
des jours avant expiration à partir de deux certificats figés, l'analyse des réponses
DNS, le calcul de la perte de paquets et la traduction du défaut de `CAP_NET_RAW`.

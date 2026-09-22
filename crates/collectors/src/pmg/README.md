# Collecteur Proxmox Mail Gateway

Intégration de Proxmox Mail Gateway (PMG) : files d'attente Postfix et
ancienneté du plus vieux message, trafic du jour et ce qui y a été filtré
(spam, virus, rebonds, greylisting, rejets), occupation des quarantaines, âge
des bases de signatures ClamAV et SpamAssassin, services, certificats, mises à
jour et état de la grappe. Le module est calqué sur celui de Proxmox Backup
Server ; il ne dépend que de `dumbmonit-proto`, `reqwest` (via le client
partagé `collectors::http`), `serde`, `chrono`, `tokio` et `futures`.

## Câblage

Déjà fait, mais pour mémoire :

* `crates/collectors/src/lib.rs` : `pub mod pmg;`, `pub use pmg::PmgCollector;`
  et l'enregistrement dans `Registry::remote` ;
* `crates/server/src/collectors/mod.rs` : la réexportation et `pmg_history` ;
* `crates/server/src/main.rs` :
  `registry.register(Arc::new(collectors::PmgCollector::new().with_observer(…)))` ;
* `crates/server/src/api/collectors.rs` : la notice de mise en route et
  `PMG_OPTIONS` ;
* `crates/server/src/alerting/rules.rs` : les règles `pmg_*` ;
* `crates/server/src/api/pmg.rs` : les panneaux de la page d'équipement.

Les cibles de type `pmg` sont alors interrogées par le planificateur.

## Configuration d'une cible

* **Adresse** : `10.0.0.40`, `mail.lan`, `mail.lan:8006`, `[fd00::4]` ou une URL
  complète `https://mail.example.net`. Le schéma `https` et le port `8006` sont
  ajoutés si besoin.
* **Identifiant** :
  * `Credential::UsernamePassword` — `utilisateur@pmg` (ou `@pam`) et le mot de
    passe. **Mode recommandé, et le seul que PMG propose.** Un ticket est obtenu
    sur `POST /access/ticket`, transmis dans le cookie `PMGAuthCookie`, mis en
    cache et renouvelé dix minutes avant sa fin de vie (deux heures).
  * `Credential::ApiToken` — la chaîne `utilisateur@realm!nom=secret`, convertie
    en en-tête `Authorization: PMGAPIToken=utilisateur@realm!nom:secret`.
    **Proxmox Mail Gateway 9 ne délivre pas de jeton d'API** : il n'existe
    aucun point d'entrée `/access/users/{id}/token`, contrairement à PVE et
    PBS. Le mode est gardé pour une version ultérieure qui en offrirait, et
    pour une passerelle placée derrière un frontal qui en attendrait un.
* **Droits nécessaires**, en lecture seule : le rôle **`Audit`**, et rien
  d'autre. Il couvre tout ce que le collecteur lit — état du nœud, services,
  files Postfix, statistiques, décomptes de quarantaine, bases ClamAV et
  SpamAssassin, grappe, certificats, abonnement, mises à jour. Les rôles
  inférieurs ne suffisent pas : `quser` ne voit que son propre courrier,
  `helpdesk` ne lit ni le nœud ni les statistiques. `Audit` ne peut rien
  modifier, ne libère aucun message de quarantaine et n'en lit aucun.

  Rien de plus n'est requis, le collecteur ne fait que des `GET`.

### Certificat auto-signé

Une installation PMG par défaut présente un certificat auto-signé, que le
collecteur refuse. Le message d'erreur le dit explicitement. Deux issues :

1. installer un certificat reconnu sur la passerelle (ACME est intégré à PMG) ;
2. ajouter l'étiquette `insecure_tls = true` sur la cible.

L'option n'est jamais activée implicitement.

### Étiquettes reconnues

| Étiquette | Défaut | Rôle |
|---|---|---|
| `insecure_tls` | `false` | Accepte un certificat non vérifiable. |
| `port` | `8006` | Port de l'API, si l'adresse n'en précise pas. |
| `request_timeout_seconds` | `15` | Délai par requête HTTP, de 1 à 120. |
| `node` | tous | Restreint la collecte à un nœud de la grappe. |
| `recent_hours` | `12` | Fenêtre de la courbe de trafic, de 1 à 24. |
| `queues` | `true` | Files d'attente Postfix (`qshape`, quatre appels). |
| `quarantine` | `true` | Occupation des quarantaines spam et antivirus. |
| `attachment_quarantine` | `false` | Compte aussi la quarantaine de pièces jointes. |
| `signatures` | `true` | Âge des bases ClamAV et SpamAssassin. |
| `services` | `true` | État des unités systemd. |
| `certificates` | `true` | Certificats servis par l'interface. |
| `updates` | `true` | Mises à jour de paquets en attente. |
| `subscription` | `true` | Abonnement du nœud. |

## Deux pièges de l'API, et ce que le collecteur en fait

### `/statistics/mail` agrège par journée locale, pas sur la fenêtre demandée

`PMG::Statistic::total_mail_stat` lit la table agrégée `DailyStat` sur
`localdayspan()` : **`endtime` est ignoré**, seul `starttime` choisit la journée.
Interrogé avec la valeur par défaut documentée (`starttime = maintenant - 1
jour`), l'appel renvoie la journée *précédente*, c'est-à-dire des zéros partout
sur une installation ordinaire — et une intégration écrite d'après la seule
documentation publierait ces zéros. Le collecteur demande donc
`starttime = maintenant` : les totaux depuis minuit, ce que montre le tableau de
bord de PMG lui-même. Les séries correspondantes sont des `Gauge` qui retombent
à zéro à minuit, jamais des `Counter`. La courbe des dernières heures
(`/statistics/recent`) lit la table brute `cstatistic` et roule vraiment.

### `qshape` ne donne que des tranches d'âge

Les colonnes sont `5m`, `10m`, `20m`, … `1280m`, `1280m+`, chacune couvrant la
tranche ouverte par la précédente. `queue_oldest_age_seconds` retient la borne
**basse** de la plus haute tranche occupée : c'est un minorant, documenté comme
tel. Un message annoncé « au moins vingt minutes » peut en avoir trente-neuf.
Minorer fait sonner l'alerte plus tard, jamais sur un message qui n'est pas
réellement en retard.

## Ce qui ne sort jamais de la passerelle

Le contenu des messages. Les quarantaines sont **comptées**, jamais lues : ni
sujet, ni expéditeur, ni destinataire, ni corps. Les points d'entrée de
statistiques par adresse (`/statistics/sender`, `/statistics/receiver`,
`/statistics/contact`) ne sont pas appelés du tout. La quarantaine de pièces
jointes est la seule qui oblige à lister pour compter, faute de point d'entrée
de décompte : seule la longueur de la liste est retenue, et l'option est
désactivée par défaut.

## Dégradation silencieuse

Un appel qui répond 403 (droit absent) ou 404 (paquet ou version absents) donne
`Ok(None)` : ni série, ni erreur de collecte. C'est le cas d'une passerelle sans
ClamAV, d'un rôle qui ne voit pas les mises à jour, d'une version antérieure à
un point d'entrée. Seul `/version` condamne l'interrogation entière.

Plus généralement, aucune valeur que l'API ne renvoie pas ne produit de
métrique : un état de nœud vide donne *zéro* échantillon plutôt qu'un mur de
zéros, une file vide n'a pas d'âge de plus vieux message (mais
`queue_messages = 0` reste une mesure), une unité `not-found` n'a pas de série
`service_running`, une date ClamAV illisible ne produit pas d'âge.

## Bornes

Dix virus au plus produisent une série, vingt domaines au plus entrent dans la
vue d'une file, seize nœuds au plus sont interrogés. Les nœuds sont interrogés
en parallèle : la durée de la sonde suit le nœud le plus lent, pas leur somme.
Dans une grappe, les files sont additionnées pour la vue, l'âge du plus vieux
message étant le **maximum** — c'est le message le plus en souffrance que l'on
veut voir, pas une moyenne qui le noierait.

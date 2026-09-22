# DumbMonit — rapport du 16 septembre 2026

## Analyse du 22/09 : ce qui manque encore

**Priorité haute (petit/moyen effort)** : moniteur push/heartbeat (cron, scripts de sauvegarde) · acquitter/snoozer une alerte · jetons API pour toute l'API REST (les `dmt_` n'ouvrent que MCP) · export/import de la config + sauvegarde SQLite planifiée · export Prometheus `/metrics` · test live du relais et du 2FA.

**Intégrations** : agent macOS/FreeBSD (TrueNAS, pfSense, OPNsense), capteurs (températures, SMART, GPU, ZFS) côté agent, Windows jamais exercé ; moniteurs gRPC/MQTT/SQL/SMTP/WebSocket ; Docker sans agent ; TrueNAS/OPNsense/Unifi/NUT ; diff lisible des paquets PVE dans la notification.

**Alerting** : maintenance cron/mensuelle · routage par tag vers les canaux · escalade · bouton « tester ce canal » + journal des envois.

**Pages de statut** : logo, CSS, badges uptime, widget, abonnés e-mail, domaine perso natif.

**Sécurité (reste de l'audit)** : jeton d'agent non lié à la machine (un hôte compromis peut en usurper un autre) · commandes distantes actives par défaut et jetons en clair en `http://` · CSP complet, taille des rapports de commande, URIs en logs debug, CSRF login OIDC · passkeys.

**Exploitation** : test de migration N→N+1 en CI (la bascule root→65532 l'a montré), nettoyage des séries orphelines, « pas de données » explicite plutôt qu'une sparkline vide.

**Produit** : onboarding après le setup, PWA/manifest, filtre par tag et par site, route `/dev/sky` à retirer, `reference/api.md` et `faq.md` périmés, page des règles incomplète (~60 règles).

**Qualité** : test d'intégration flaky sous charge, pas de tests navigateur automatisés.

**Ordre retenu** : 1) tests live relais + 2FA ; 2) push monitor + ack/snooze + jetons API ; 3) export/import + sauvegarde ; 4) jeton agent ↔ machine ; 5) onboarding, PWA, doc ; 6) intégrations.

**Mise à jour 21/09** — retour collègue (round 6) livré : `v0.1.0-alpha.2` (commit `98e75d2`) puis `v0.1.0-alpha.3` (`0cf8cbc`, correctifs ci-dessous). Image `ghcr.io/noekan/dumbmonit:latest` = alpha.3 dès que le workflow Release finit (~2 h) ; en attendant, `:edge`.

- **Vérifié en live sur :8080** (fakes PVE/PBS/DSM du labo local) : panneau Invités PVE (7 VM/CT, statut, CPU, RAM, disque, réseau, uptime, dernière sauvegarde, HA), page PBS (échecs 30 j, calendrier à points, jobs sync/verify/prune/GC, santé), page Synology (CPU/RAM/température/uptime, volumes, disques SMART/usure, Active Backup avec cadence apprise), formulaire Proxmox « Token ID + Secret » avec tuto utilisateur dédié en lecture seule, nouvelle navigation Overview · Devices · Alerts · Status · Settings, 2FA dans Réglages, CSRF (`X-Requested-With`), en-têtes de sécurité, alertes intégrées PVE/ABB qui montent. Captures README/docs régénérées.
- **Cassé dans alpha.2, corrigé en alpha.3** : (1) **mise à jour depuis alpha.1 plantait en boucle** — le conteneur tourne en utilisateur 65532 et le volume créé par alpha.1 appartient à root ; il faut une fois `docker run --rm -v dumbmonit-data:/data alpine chown -R 65532:65532 /data` (le serveur le dit maintenant au démarrage ; doc « Upgrading »). (2) **Telegram rejetait chaque message** (Markdown legacy : `**gras**` et `_` dans les noms de machines) → envoi en HTML échappé. (3) Résumés d'alertes qui listaient `tag_port`, `device_id`… → masqués.
- **Pas vérifié en live** : mode relais (agent `relay: true` + image `dumbmonit-agent`) — seulement tests unitaires/intégration ; Plakar auto-détecté ; 2FA de bout en bout (enrôlement/QR) ; Windows.
- **À faire par toi** : rendre le package ghcr public ; réinstaller l'agent sur nuci3/Windows (vieux binaire) ; sauvegarder le volume `dumbmonit-data` avant de mettre à jour.

**Mise à jour 16/09 soir** : les points 1, 2, 3, 4, 5, 6 ci-dessous et la faille OIDC sont **corrigés** (commit `47fb461`, vérifiés en live : 0 alerte fantôme, commandes expirées, agent ancien signalé, mobile OK). Tag `v0.1.0-alpha.1` posé → image `ghcr.io/noekan/dumbmonit:latest` (à rendre publique sur GitHub → Packages → dumbmonit → Change visibility).

Tout ce qui était demandé est construit, testé et poussé (`main`, image mono-conteneur sur http://localhost:8080, login `admin` / `dumbmonit-dev-2026`).

## Ce qui marche (vérifié sur des équipements simulés + nuci3 + Windows)

- 15 cibles vertes : SNMP (UPS, imprimante, switch), Proxmox VE (94 familles de métriques), PBS (55), Synology DSM + Active Backup (55), agents Linux/Windows (41), HTTP/TCP/DNS/ping/TLS.
- Multi-utilisateurs admin/viewer + OIDC (PKCE, groupes → rôle), déconnexion.
- MCP (`POST /api/mcp`, jetons `dmt_`) : Claude/ChatGPT lisent l'état et agissent.
- Docker via l'agent : redémarrage auto, mise à jour auto avec rollback, nettoyage des vieilles images ; Plakar (âge/résultat des sauvegardes).
- Pages de statut publiques `/s/<slug>` (groupes, incidents, RSS, badge).
- Notifications intelligentes : hystérésis, anti-flap, cooldown, heures calmes, regroupement, plafond horaire, 22 canaux.
- 41 règles intégrées, suppression par dépendance, baseline saisonnière, prévisions.
- Ressources : 123 Mo RAM en moyenne (serveur + VictoriaMetrics embarqué), CPU ≈ 1 %.
- Non couvert par les tests de bout en bout (agent à court de temps) : OIDC/comptes et MCP en conditions réelles — vérifiés seulement par les tests unitaires/intégration et la revue de sécurité.
- Qualité : fmt/clippy/tests (≈ 1 100) verts, `svelte-check` 0 erreur, docs `mkdocs --strict` OK.
- Doc : **tout est sur Read the Docs** désormais ; la page `/docs/notifications` de l'app est supprimée, l'app renvoie vers https://dumbmonit.readthedocs.io.

## Ce qui a cassé (trouvé par les tests de bout en bout)

1. **Supprimer, désactiver ou renommer un appareil laisse des alertes fantômes** (`host_down` évalue les anciennes séries pendant 7 jours, un e-mail est parti pour un appareil supprimé ; l'accueil, le mur et l'historique affichent des cartes sans nom). → règles à clé sur `target`, purge de l'état à la suppression. **Priorité 1.**
2. **Commandes Docker en attente qui n'expirent jamais** : si l'agent ne répond pas (ancien binaire, `commands: false`), tout nouveau « Restart/Update » renvoie 409 pour toujours et la politique automatique saute le conteneur. → expiration côté serveur + bouton Annuler + afficher la version/capacités de l'agent.
3. **Mobile (390 px)** : Réglages › Pages de statut et Utilisateurs, la colonne texte s'écrase à 60 px ; titres d'alertes tronqués ; pastille de navigation mal placée quand le badge Alertes se charge.
4. Page de statut publique : bannière « Major outage » alors que tout est opérationnel.
5. Notifications de règles par série (« Container stopped », « VM stopped », tâche ABB…) ne nomment pas la VM/le conteneur, seulement l'hôte, et ajoutent « 1 (threshold > 0) » ; sévérité par emoji seul dans mail/ntfy.
6. Erreurs de configuration (URL invalide, SNMP sans community) affichées « Unreachable » au lieu de « Misconfigured ».
7. API : nom d'appareil sans limite (5 000 caractères acceptés, VictoriaMetrics jette tout) ; `PUT` sans `profile_id` efface le profil détecté ; `parent_id` inexistant → 500 ; erreurs 422 en texte brut ; erreurs MetricsQL illisibles ; commande d'installation générée avec `http://0.0.0.0:8080`.
8. Détail appareil : « Proxmox backup guests total » affiché comme un taux (/s) ; pas de vue « Essentials » pour UPS/Proxmox/Synology (noms bruts) ; « 3887999s » au lieu de « 45 j ».
9. Règles manquantes : disque Synology en mauvaise santé (SMART/secteurs) et port de switch down / erreurs — métriques collectées, aucune alerte.
10. Les agents nuci3 et Windows tournent **l'ancien binaire ezymonit** (pas de `container_health`, pas de canal de commandes) : à réinstaller (commandes en bas).

## Sécurité (revue complète, détail dans le rapport interne)

- **Haut** : un premier login OIDC dont le `preferred_username`/`email` correspond à un compte local (ex. `admin`) est lié à ce compte sans vérification → prise de contrôle possible selon l'IdP. À corriger avant toute exposition.
- Moyen : jetons d'agent non liés à une machine ; moniteur HTTP = SSRF pour un admin ; redirection ouverte `?redirect=/\evil` ; rustls à mettre à jour (RUSTSEC-2026-0285) ; `/api/discovery` accessible aux viewers ; conteneur en root.

## Top 10 de ce qui manque (vs Uptime Kuma / Pulse / Kener / Gatus)

1. Moniteur **push / heartbeat** (cron, scripts de sauvegarde) — petit
2. Jetons API pour **toute** l'API REST (aujourd'hui MCP seulement) — petit
3. Export/import de configuration chiffré — moyen
4. 2FA TOTP — moyen
5. Export Prometheus `/metrics` pour Grafana — petit
6. Acquitter / snoozer une alerte depuis l'interface — petit
7. Agent macOS/FreeBSD + températures/SMART/ZFS — moyen
8. Pages de statut : logo, CSS, badge d'uptime, abonnés e-mail — moyen
9. Moniteurs gRPC/MQTT/SQL/SMTP, assertions DNS — moyen
10. Fenêtres de maintenance cron/mensuelles liées aux pages de statut — petit

Ce que personne d'autre n'a : suppression par dépendance + baseline saisonnière + prévisions, profondeur Proxmox VE **et** PBS, mise à jour Docker avec rollback, SNMP auto-profilé + découverte CIDR, MCP.

## Prochaines étapes proposées

1. Corriger les 3 gros : alertes fantômes, commandes Docker bloquées, lien OIDC non vérifié (+ la limite de nom) — une journée.
2. Mobile Réglages/Alertes + bannière page de statut + libellés des notifications.
3. Doc : API (`/tokens`, `/users`, `/status-pages`…), `settings.md`, PRODUCT.md (dit encore « deux conteneurs »).
4. Push monitor + jetons API globaux (les deux « petits » à plus forte valeur).
5. Puis : première release, image ghcr publique, Discussions GitHub.

## Réinstaller les agents (migration ezymonit → dumbmonit)

nuci3 :
```sh
TOKEN=$(sudo sed -n 's/^token: *//p' /etc/ezymonit/agent.yaml)
curl -sSL http://192.168.10.254:8080/install.sh | sudo sh -s -- --token="$TOKEN" --url=http://192.168.10.254:8080
```
Windows (PowerShell admin) :
```powershell
$t = (Select-String -Path "$env:ProgramData\EzyMonit\agent.yaml" -Pattern '^token:\s*(.+)$').Matches[0].Groups[1].Value
& ([scriptblock]::Create((irm http://192.168.10.254:8080/install.ps1))) -Token $t -Url http://192.168.10.254:8080
```

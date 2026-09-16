# DumbMonit — rapport du 16 septembre 2026

Tout ce qui était demandé est construit, testé et poussé (`main`, image mono-conteneur sur http://localhost:8080, login `admin` / `dumbmonit-dev-2026`).

## Ce qui marche (vérifié sur le lab Docker + nuci3 + Windows)

- 15 cibles vertes : SNMP (UPS, imprimante, switch), Proxmox VE (94 familles de métriques), PBS (55), Synology DSM + Active Backup (55), agents Linux/Windows (41), HTTP/TCP/DNS/ping/TLS.
- Multi-utilisateurs admin/viewer + OIDC (PKCE, groupes → rôle), déconnexion.
- MCP (`POST /api/mcp`, jetons `dmt_`) : Claude/ChatGPT lisent l'état et agissent.
- Docker via l'agent : redémarrage auto, mise à jour auto avec rollback, nettoyage des vieilles images ; Plakar (âge/résultat des sauvegardes).
- Pages de statut publiques `/s/<slug>` (groupes, incidents, RSS, badge).
- Notifications intelligentes : hystérésis, anti-flap, cooldown, heures calmes, regroupement, plafond horaire, 22 canaux.
- 41 règles intégrées, suppression par dépendance, baseline saisonnière, prévisions.
- Ressources : 123 Mo RAM en moyenne (serveur + VictoriaMetrics embarqué), CPU ≈ 1 %.
- Qualité : fmt/clippy/tests (≈ 1 100) verts, `svelte-check` 0 erreur, docs `mkdocs --strict` OK.
- Doc : **tout est sur Read the Docs** désormais ; la page `/docs/notifications` de l'app est supprimée, l'app renvoie vers https://dumbmonit.readthedocs.io.

## Ce qui a cassé (trouvé par les tests de bout en bout)

1. **Supprimer ou renommer un appareil laisse des alertes fantômes** (`host_down` évalue les anciennes séries pendant 7 jours, un e-mail est parti pour un appareil supprimé). → règles à clé sur `target`, purge de l'état à la suppression. **Priorité 1.**
2. Nom d'appareil sans limite de longueur (5 000 caractères acceptés, VictoriaMetrics jette tout en silence).
3. `PUT /api/targets` sans `profile_id` efface le profil détecté ; `parent_id` inexistant → 500.
4. Erreurs 422 en texte brut au lieu de l'enveloppe `{"error"}` ; erreurs MetricsQL illisibles.
5. L'agent installé sur nuci3 est **l'ancienne version** (pas de `container_health`/`restart_count`) : à réinstaller (commandes ci-dessous).

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

1. Corriger les alertes fantômes + le lien OIDC non vérifié + la limite de nom (une demi-journée).
2. Mettre à jour la doc (API : `/tokens`, `/users`, `/status-pages`… ; `settings.md` ; PRODUCT.md dit encore « deux conteneurs »).
3. Push monitor + jetons API globaux (les deux « petits » à plus forte valeur).
4. Puis : image ghcr publique + Discussions GitHub.

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

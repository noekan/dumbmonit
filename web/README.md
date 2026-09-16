# Interface web d'DumbMonit

Application monopage SvelteKit, compilée en site statique et embarquée dans le
binaire Rust du serveur. Il n'y a pas de serveur Node en production.

## Pile technique

- **SvelteKit** avec `@sveltejs/adapter-static` en mode SPA (`fallback: index.html`),
  car les routes dynamiques comme `/targets/42` sont résolues côté client
- **Tailwind CSS 4** via le greffon Vite
- **uPlot** pour les graphes
- **TypeScript** en mode strict

Aucune ressource n'est chargée depuis un CDN : polices, styles et scripts sont
empaquetés par le build.

## Développement

```bash
npm install
npm run dev
```

L'interface est servie sur <http://localhost:5173>. Les requêtes `/api/*` sont
relayées vers le serveur Rust sur `http://localhost:8080` par le proxy Vite
configuré dans `vite.config.ts` — démarrez donc le backend en parallèle :

```bash
cargo run          # depuis la racine du dépôt
```

Sans backend démarré, l'interface reste utilisable mais affiche ses messages
d'erreur de connexion.

## Construction

```bash
npm run build
```

Le site statique est écrit dans `web/build/`. C'est ce dossier que le binaire
Rust doit embarquer.

## Vérifications

```bash
npm run check   # svelte-check : types et accessibilité
```

Le dépôt est tenu à zéro erreur et zéro avertissement.

## Organisation du code

| Chemin | Rôle |
| --- | --- |
| `src/lib/api/` | Couche d'accès réseau. **Tout** appel HTTP passe par ici. |
| `src/lib/api/types.ts` | Types du contrat d'API du serveur. |
| `src/lib/api/client.ts` | `fetch` centralisé, erreurs normalisées en français. |
| `src/lib/metrics.ts` | Requêtes MetricsQL et mise en forme pour les graphes. |
| `src/lib/format.ts` | Dates, durées, état d'un équipement. |
| `src/lib/components/` | Composants partagés (graphe, badges, formulaires). |
| `src/lib/stores/auth.svelte.ts` | État de la session, garde de navigation, réaction aux 401. |
| `src/routes/` | Un dossier par écran. |

### Règle sur les identifiants

L'API ne renvoie jamais le secret d'un équipement, seulement son type
(`credential_kind`). À l'inverse, le serveur remplace l'identifiant à chaque
écriture : omettre le champ `credential` dans un `PUT` **efface** le secret
enregistré. Le formulaire de modification impose donc de ressaisir le secret, et
l'explique à l'utilisateur.

### Routes d'API anticipées

`/api/metrics/query_range`, `/api/alerts` et `/api/discovery` ne sont pas encore
servies par le backend. La couche d'accès les marque `anticipated` : un 404 y est
traité comme « fonctionnalité pas encore disponible » et dégradé proprement
(graphe vide, aucune alerte, message dédié sur la découverte réseau) plutôt
qu'affiché comme une panne.

### Ajout d'un équipement

L'écran `/targets/new` se déroule en deux temps : on choisit d'abord le type
d'équipement, puis on saisit son adresse et son secret. Le formulaire proprement
dit ne montre jamais plus de deux champs ; le nom, la période et le profil sont
pré-remplis et rangés sous « Options avancées ».

Tout le contenu de cet écran vient de `GET /api/collectors` : la liste des types,
leur description, l'adresse d'exemple, le port par défaut, les formes
d'authentification acceptées et la **notice de mise en route** affichée à côté du
formulaire. Rien n'est écrit en dur côté interface. Un type qu'une version
ultérieure du serveur ajoutera apparaîtra donc ici sans modification, avec sa
propre notice ; un type que le serveur ne sait pas décrire reçoit une
présentation neutre et une consigne générique plutôt qu'un encadré vide.

### Authentification

Un seul mot de passe protège l'instance : pas de compte, pas d'identifiant, pas
d'inscription, pas de récupération. Trois écrans en découlent — `/setup` au tout
premier démarrage, `/login` ensuite, et une section « Sécurité » dans les
réglages pour changer le mot de passe.

Le cookie de session est `HttpOnly` : l'interface ne peut pas le lire et ne
stocke aucun jeton. L'état de connexion se déduit uniquement de
`GET /api/auth/status` et des 401 reçus. Un 401 sur n'importe quelle route est
intercepté **une seule fois**, dans `src/lib/api/client.ts`, qui prévient
`src/lib/stores/auth.svelte.ts` ; celui-ci renvoie vers `/login` en mémorisant la
page demandée. Aucun composant n'a à s'en préoccuper.

Les routes où un 401 est une réponse métier attendue — mot de passe erroné à la
connexion ou lors d'un changement — passent `allowUnauthorized` pour ne pas
déclencher cette redirection.

Si le serveur ne sert pas encore `/api/auth/status` (404), l'instance est
considérée comme non protégée et l'interface reste entièrement utilisable.

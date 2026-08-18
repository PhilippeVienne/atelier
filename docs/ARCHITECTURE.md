# Architecture d'Atelier

## Objectif

Fournir a un agent de code (Claude Code, Gemini CLI, etc.) un environnement
d'execution auquel on peut accorder des pouvoirs larges (shell, reseau, ecriture
disque) sans risque pour le reste du systeme, parce qu'il est execute dans une
prison suffisamment etanche : une microVM Firecracker, elle-meme orchestree
depuis un pod Kubernetes.

## Definition de l'environnement : le devcontainer comme source de verite

L'environnement livre a l'agent n'est pas decrit par une image ad hoc : il est
defini par un `.devcontainer/devcontainer.json` standard, au sens de la
[specification VS Code Dev Containers](https://containers.dev/). N'importe
quel depot deja equipe pour Dev Containers (VS Code, GitHub Codespaces,
`devcontainer` CLI) peut donc etre servi tel quel par Atelier : c'est le
`devcontainer.json` du projet qui pilote l'image de base, le Dockerfile
eventuel, les features et les commandes de setup.

Le composant **image-builder** ne reimplemente pas la resolution du
devcontainer.json : il delegue a
[envbuilder](https://github.com/coder/envbuilder) (appele en sous-processus,
present dans l'image du job de build), qui sait deja construire un
devcontainer sans daemon Docker (buildkit rootless embarque). `image-builder`
se charge d'invoquer envbuilder avec la source resolue, puis d'empaqueter le
resultat en image ext4 publiee dans un cache content-addresse (cle = digest
du contenu resolu), reference dans `WorkshopStatus.image_digest` que
`vm-supervisor` consomme pour booter la microVM. Un `Workshop` passe donc par
une phase `BuildingImage` avant `Provisioning`/`Running`.

## Vue d'ensemble

```
                         ┌────────────────────────────────────────┐
Client externe (JWT) ───►│              api-server                │
                         │  (auth JWT, expose Workshop CRUD)       │
                         └───────────────┬──────────────────────────┘
                                          │ cree/lit des CR
                                          ▼
                         ┌────────────────────────────────────────┐
                         │              controller                │
                         │  (operateur, reconcilie Workshop → Pod) │
                         └───────────────┬──────────────────────────┘
                                          │ spec.devcontainer
                                          ▼
                         ┌────────────────────────────────────────┐
                         │             image-builder               │
                         │  (devcontainer.json → rootfs Firecracker)│
                         │  status.image_digest                     │
                         └───────────────┬──────────────────────────┘
                                          │ cree le pod parent
                                          ▼
      ┌───────────────────────── Pod parent (namespace isole) ─────────────────────────┐
      │                                                                                  │
      │   ┌───────────────┐   ┌───────────┐   ┌────────────────┐   ┌─────────────────┐  │
      │   │ vm-supervisor │   │ net-proxy │   │ identity-proxy │   │   mcp-gateway    │  │
      │   │ (lifecycle    │   │ (egress   │   │ (injection de  │   │ (agent ↔ monde   │  │
      │   │  Firecracker) │   │  allowlist│   │  credentials)  │   │  exterieur, MCP) │  │
      │   └───────┬───────┘   └─────┬─────┘   └────────┬───────┘   └────────┬─────────┘  │
      │           │ vsock/API       │ reseau            │ reseau            │ vsock       │
      │           ▼                 └──────────┬────────┴───────────────────┘             │
      │   ┌─────────────────────────────────────▼─────────────────────────────────────┐   │
      │   │                     microVM Firecracker (jailer)                          │   │
      │   │   agent de code (Claude Code, ...) + shell + acces disque de travail       │   │
      │   └─────────────────────────────────────────────────────────────────────────┘   │
      │                                                                                  │
      └──────────────────────────────────────────────────────────────────────────────────┘
```

## Composants

### control plane (hors du pod parent)

- **api-server** (`crates/api-server`) : API HTTP externe. Authentifie les
  appels via un JWT dont l'issuer est signe par un provider externe
  pre-enregistre (liste d'issuers de confiance + JWKS). Cree/lit/detruit des
  CR `Workshop`.
- **controller** (`crates/controller`) : operateur Kubernetes (kube-rs) qui
  reconcilie les CR `Workshop` en ressources concretes (pod parent,
  ResourceQuota, NetworkPolicy) et met a jour leur statut.
- **CRD `Workshop`** (`crates/common/src/crd.rs`, manifeste genere dans
  `crds/workshop.yaml`) : source de verite declarative pour un environnement
  (source devcontainer, ressources, allowlist reseau, outils/simulateurs
  actifs, proprietaire).
- **image-builder** (`crates/image-builder`) : construit le rootfs Firecracker
  a partir de `WorkshopSpec.devcontainer` en invoquant `envbuilder` en
  sous-processus pour la resolution du devcontainer.json, puis empaquette le
  resultat en ext4 et le publie dans le cache content-addressed. Voir section
  dediee ci-dessus.

### tooling du pod parent (a cote de la microVM, pas dedans)

- **vm-supervisor** (`crates/vm-supervisor`) : demarre/arrete la microVM
  Firecracker via jailer + socket API, expose un canal de controle (vsock)
  vers l'agent.
- **net-proxy** (`crates/net-proxy`) : seul chemin de sortie reseau autorise
  pour la microVM ; n'autorise que les domaines listes dans
  `Workshop.spec.egress_allowlist`, journalise chaque appel.
- **identity-proxy** (`crates/identity-proxy`) : injecte des credentials/tokens
  dans les appels sortants (ex: acces a une API necessitant un token) sans
  jamais exposer le secret brut a l'agent dans la VM.
- **mcp-gateway** (`crates/mcp-gateway`) : serveur MCP expose a l'agent (via
  vsock). C'est le seul point d'entree pour que l'agent (1) agisse sur le
  monde exterieur au-dela du simple reseau proxifie, et (2) demande des
  reglages a l'atelier (elargir une allowlist, activer un simulateur
  d'API/AWS, demander un credential). Point d'extension privilegie pour
  ajouter de nouveaux simulateurs (LocalStack pour AWS, mocks d'API, etc.).

### interfaces utilisateur

- **dashboard** (`dashboard/`, Next.js) : vue admin et vue utilisateur final.
  Lister/creer/detruire des Workshops, visualiser leur etat et leur
  consommation de ressources, s'y connecter (SSH ou VS Code via un
  code-server embarque, cf. `coder/code-server` comme reference
  d'implementation).

## Mise en veille : snapshot/restore Firecracker

Un Workshop n'est pas seulement demarre/detruit : il peut etre **suspendu**.
Firecracker expose nativement `PUT /snapshot/create` (fige l'etat de la VM et
son contenu memoire) et `PUT /snapshot/load` (restaure a l'identique), ce qui
permet de :

- liberer les ressources du pod parent (CPU/memoire/pod du cluster) pendant
  qu'un Workshop est inactif, sans perdre l'etat de travail de l'agent ;
- reprendre en quelques centaines de millisecondes, sans rejouer le boot du
  noyau invite ni le setup du devcontainer (contrairement a une destruction
  suivie d'un nouveau `Provisioning`).

Ce cycle est pilote par `WorkshopSpec.desired_state` (`Running` /
`Suspended`), que le `controller` fait converger :

- `Running` → `Suspended` : phase `Suspending`, `vm-supervisor` declenche
  `snapshot/create`, publie le resultat dans le cache content-addressed
  (meme mecanisme que les images `image-builder`), le digest est ecrit dans
  `WorkshopStatus.snapshot_digest`, puis le pod parent est libere.
- `Suspended` → `Running` : phase `Resuming`, le `controller` recree le pod
  parent, `vm-supervisor` recupere le snapshot via son digest et appelle
  `snapshot/load` au lieu de rebooter depuis `image_digest`.

L'API expose ce cycle via `POST /v1/workshops/:name/suspend` et `/resume`
(`crates/api-server`), typiquement utilises par le dashboard pour une mise en
veille manuelle ou par une politique d'auto-suspend sur inactivite (a
definir).

## Modele de securite

- La seule surface d'attaque exposee par la microVM vers l'exterieur passe
  par le pod parent : reseau (net-proxy), identite (identity-proxy) et
  controle (mcp-gateway). Aucun acces direct de la VM au reste du cluster.
- Isolation memoire/noyau assuree par Firecracker (jailer, seccomp, cgroups)
  plutot que par la seule isolation de conteneur d'un Pod.
- Authentification externe MVP : JWT signes par un provider externe
  pre-enregistre (liste d'issuers + JWKS statique au demarrage de
  l'api-server). Pas de gestion d'utilisateurs locale dans un premier temps.

## Allocation de ressources et scaling

- Chaque `Workshop` se traduit par un pod avec `resources.requests/limits`
  explicites (`WorkshopSpec.resources`), compatible avec le cluster-autoscaler
  et un HPA standard au niveau du nombre de Workshops actifs.

## Ce qui reste a trancher (hors MVP initial)

- Mecanisme concret d'orchestration Firecracker (jailer pilote directement
  par `vm-supervisor`, decision prise : pas de runtime OCI type Kata).
- Empaquetage concret du resultat d'envbuilder en image ext4 (outillage exact,
  gestion des couches/diffs pour eviter de tout reconstruire a chaque revision)
  et choix du kernel invite partage vs embarque dans le build.
- Support du sous-ensemble de la spec devcontainer.json a couvrir en premier
  (image simple vs build Dockerfile vs features vs docker-compose multi-service).
- Modele d'autorisation fin cote `mcp-gateway` (quelles demandes de
  l'agent sont auto-approuvees vs necessitent une validation humaine).
- Stockage des secrets pour `identity-proxy` (Vault ? Secrets Kubernetes
  projetes ?).
- Politique d'auto-suspend (delai d'inactivite avant snapshot automatique) et
  compatibilite des snapshots entre versions de kernel/Firecracker (un
  snapshot fige une version precise ; que faire s'il faut mettre a jour le
  kernel invite entre deux reprises ?).

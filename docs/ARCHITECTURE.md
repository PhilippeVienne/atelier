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
present dans l'image du job de build). Pipeline reel, verifie de bout en
bout (y compris boot Firecracker du resultat) :

1. `envbuilder` clone le repo, resout le devcontainer.json, construit
   l'environnement (base image + `postCreateCommand` etc.) et **le pousse
   comme image OCI standard** vers un registre de conteneurs
   (`ENVBUILDER_PUSH_IMAGE`/`ENVBUILDER_CACHE_REPO`). Envbuilder ne produit
   *pas* de dossier d'export propre : il construit "en place" en supprimant
   le systeme de fichiers du conteneur qui l'execute (sauf `/.envbuilder`)
   avant d'y extraire l'image cible — `image-builder` tourne donc dans le
   *meme* conteneur qu'envbuilder (`crates/image-builder/Dockerfile`), et
   tout ce dont il a besoin *apres* cet appel doit vivre sur un point de
   montage separe de la racine, sous peine d'etre efface.
2. [`crane export`](https://github.com/google/go-containerregistry) (outil
   externe etabli, pas de client OCI ecrit a la main) aplatit cette image en
   tarball.
3. Le tarball est extrait puis empaquete en image ext4 (`mke2fs -d`).
4. Le digest sha256 du fichier ext4 sert de cle dans le cache
   content-addresse — aujourd'hui un repertoire monte depuis un **PVC
   Kubernetes** partage (lecture-ecriture pour le Job image-builder, lecture
   seule pour les pods parents) ; offload/reload vers S3 quand le PVC est
   trop rempli, envisage plus tard mais pas implemente — puis reference
   dans `WorkshopStatus.image_digest`, que `vm-supervisor` consomme pour
   booter la microVM. Un `Workshop` passe donc par une phase `BuildingImage`
   avant `Provisioning`/`Running`.

Voir `deploy/dev/image-builder/README.md` pour reproduire ce pipeline en
local (registre de dev, extraction des binaires envbuilder/crane).

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
  appels via un JWT dont l'issuer est [Kanidm](https://kanidm.com/) (JWKS
  recuperes au demarrage). Cree/lit/detruit des CR `Workshop`. Voir la
  section dediee « Identite et secrets » ci-dessous.
- **controller** (`crates/controller`) : operateur Kubernetes (kube-rs) qui
  reconcilie les CR `Workshop` en ressources concretes (pod parent,
  ResourceQuota, NetworkPolicy) et met a jour leur statut. Un finalizer
  (`atelier.dev/cleanup`) bloque la suppression effective d'un Workshop tant
  que ses ressources externes (entite Kanidm, role OpenBao) n'ont pas ete
  nettoyees ; les ressources Kubernetes owned (Job, ServiceAccount, Pod)
  n'en ont pas besoin, le garbage collector standard suffit.
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
  Firecracker **jailee** (chroot, cgroups), gere le cycle boot/snapshot/
  restore, via [`fctools`](https://docs.rs/fctools) (SDK Rust, pas un client
  HTTP maison). Le jailer tourne avec des capabilities Linux dediees
  (`setcap`, pas root/sudo). Pas encore de canal de controle vsock vers
  l'agent/le controller.
- **net-proxy** (`crates/net-proxy`) : seul chemin de sortie reseau autorise
  pour la microVM ; n'autorise que les domaines listes dans
  `Workshop.spec.egress_allowlist`, journalise chaque appel.
- **identity-proxy** (`crates/identity-proxy`) : injecte des credentials/tokens
  dans les appels sortants (ex: acces a une API necessitant un token) sans
  jamais exposer le secret brut a l'agent dans la VM. Secrets stockes dans
  OpenBao, recuperes en s'authentifiant avec le ServiceAccount Kubernetes du
  pod parent (pas l'identite Kanidm). Voir la section dediee « Identite et
  secrets » ci-dessous.
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

**Etat actuel de l'implementation** : le `controller` fait bien converger
`status.phase` selon `spec.desiredState` (`Suspending`/`Suspended`/
`Resuming`/`Running`) et libere/recree le pod parent en consequence — verifie
en conditions reelles (suspend puis resume contre un vrai cluster,
`crates/controller/src/reconcile.rs`). `vm-supervisor` sait de son cote
reellement piloter Firecracker pour un snapshot/restore complet (pause,
`snapshot/create`, puis `snapshot/load` dans un nouveau process — verifie en
conditions reelles avec KVM, `crates/vm-supervisor/src/vm.rs`). Ce qui
**manque encore** est le cablage entre les deux : le `controller` n'appelle
pas encore `vm-supervisor` au moment de suspendre/reprendre (pas de canal
vsock), et `status.snapshotDigest` n'est donc jamais peuple — pour l'instant,
suspendre libere juste le pod (le process `firecracker` meurt avec lui, sans
snapshot pris), et reprendre en recree un depuis `status.imageDigest`
(equivalent a un redemarrage, pas a une vraie reprise memoire). L'entite
Kanidm et le role OpenBao du Workshop sont deliberement laisses intacts a
travers ce cycle (pas reprovisionnes a chaque resume), et les endpoints
`/suspend`/`/resume` de l'api-server restent a cabler.

## Identite et secrets : Kanidm + OpenBao

Deux notions d'identite bien distinctes dans Atelier :

- **L'utilisateur humain** proprietaire d'un Workshop (`WorkshopSpec.owner_subject`).
  Son identite est geree par [Kanidm](https://kanidm.com/), qui sert de
  fournisseur d'identite pour l'ensemble d'Atelier (`api-server` ne valide que
  des JWT dont l'issuer est Kanidm) et peut lui-meme federer vers un provider
  externe (OIDC/LDAP d'entreprise) sans qu'Atelier ait a gerer cette
  integration directement.
- **L'environnement lui-meme** : chaque `Workshop` se voit attribuer sa propre
  entite machine dans Kanidm (`WorkshopStatus.kanidm_entity_id`), distincte du
  sujet humain proprietaire. Cette identite reste la reference cote
  utilisateur/dashboard, mais ce n'est **pas** elle qui sert de pont vers
  OpenBao (voir ci-dessous) — decision deliberee, cf. « Pont d'identite vers
  OpenBao ».

Les secrets destines aux environnements (credentials/tokens que
`identity-proxy` injecte dans les appels sortants de l'agent) sont stockes
dans [OpenBao](https://openbao.org/) — deliberement separe des Secrets
Kubernetes du cluster sous-jacent, qui restent geres par les mecanismes k8s
standards pour le fonctionnement du control plane lui-meme. Un secret stocke
la est frequemment lui-meme l'identite de sortie de l'environnement (ex: une
cle d'API que l'environnement presente a un service externe) : **seul**
`identity-proxy` peut la recuperer et l'utiliser — l'agent dans la microVM
n'y a jamais acces directement, meme indirectement via les variables
d'environnement ou le systeme de fichiers de la VM. C'est `identity-proxy`,
et lui seul, qui agit « en tant que » l'environnement aupres des services
externes.

### Pont d'identite vers OpenBao : auth Kubernetes, pas Kanidm

`identity-proxy` s'authentifie aupres d'OpenBao via la **methode d'auth
Kubernetes** d'OpenBao, pas via une federation JWT/OIDC avec Kanidm. Le pod
parent de chaque Workshop recoit son propre ServiceAccount Kubernetes
(`<name>-parent`, cree par le `controller`) ; identity-proxy presente le
token projete de ce ServiceAccount, qu'OpenBao verifie en direct aupres de
l'API Kubernetes (TokenReview) — aucun secret a distribuer ou stocker pour
amorcer cette confiance.

Le `controller` provisionne, par Workshop, une policy OpenBao et un role
`auth/kubernetes/role/workshop-<name>` scopant l'acces au chemin KV
`secret/{data,metadata}/workshops/<name>/*` au seul ServiceAccount de ce
Workshop (`crates/controller/src/openbao.rs`), ce qui borne le rayon d'action
d'un Workshop compromis aux seuls secrets qui lui ont ete explicitement
destines. Optionnel via `OPENBAO_ADDR`/`OPENBAO_TOKEN` (`ReconcileCtx.openbao`),
meme pattern que le provisioning Kanidm.

Ce choix a ete fait deliberement au detriment de la coherence "Kanidm =
identite pour tout" : une federation JWT/OIDC Kanidm -> OpenBao demanderait
de configurer un Resource Server OAuth2 cote Kanidm et un backend JWT/OIDC
cote OpenBao (JWKS, client credentials grant) pour un gain de coherence
conceptuelle, contre une integration nettement plus lourde et une surface de
panne plus grande que l'auth Kubernetes, deja standard et deja testee de
bout en bout (`crates/controller/tests/reconcile.rs`,
`apply_provisions_openbao_role_when_configured`).

## Observabilite : OpenTelemetry

Convention imposee a tous les binaires du control plane et du tooling du pod
parent : chaque `main.rs` appelle `atelier_common::telemetry::init("<nom-du-binaire>")`
en toute premiere instruction (`crates/common/src/telemetry.rs`), et garde le
`TelemetryGuard` renvoye en vie jusqu'a la fin de `main` pour que les traces
en cours soient flush avant l'arret du processus. Ce helper commun :

- configure `tracing-subscriber` (logs structures, filtrable via `RUST_LOG`) ;
- si `OTEL_EXPORTER_OTLP_ENDPOINT` est present dans l'environnement, ajoute en
  plus une couche `tracing-opentelemetry` qui exporte les spans en OTLP/gRPC,
  avec `service.name` = le nom du binaire ;
- sans cette variable (tests d'integration, dev local sans collecteur), reste
  en logging simple sans tenter d'exporter — aucune dependance dure a un
  collecteur pour que le reste du systeme fonctionne.

Les fonctions de la boucle de reconciliation du `controller` sont annotees
`#[tracing::instrument]` (`reconcile`, `apply`, `ensure_image_build_job`,
`ensure_parent_pod`), ce qui produit une hierarchie de spans exploitable
(`reconciling object` → `reconcile` → `apply` → `ensure_*`). Verifie en
conditions reelles avec un OTel Collector local (exporteur `debug`) : les
spans arrivent bien groupes par trace, avec les attributs attendus
(`workshop=<nom>`, `service.name=atelier-controller`).

Backlog (pas encore fait) : deployer un stack d'observabilite complet
(collector + backend de stockage des traces/metriques + **Grafana**) et un
dashboard de supervision dedie, pour visualiser l'activite des Workshops en
plus des traces brutes. A ajouter dans `deploy/dev/` (dev) et `deploy/`
(cible cluster) le moment venu.

## Modele de securite

- La seule surface d'attaque exposee par la microVM vers l'exterieur passe
  par le pod parent : reseau (net-proxy), identite (identity-proxy) et
  controle (mcp-gateway). Aucun acces direct de la VM au reste du cluster.
- Isolation memoire/noyau assuree par Firecracker (jailer, seccomp, cgroups)
  plutot que par la seule isolation de conteneur d'un Pod.
- Authentification externe : JWT emis par Kanidm (JWKS recupere au demarrage
  de l'api-server). Pas de gestion d'utilisateurs locale dans Atelier
  lui-meme ; Kanidm est la seule source de verite identite.

## Allocation de ressources et scaling

- Chaque `Workshop` se traduit par un pod avec `resources.requests/limits`
  explicites (`WorkshopSpec.resources`), compatible avec le cluster-autoscaler
  et un HPA standard au niveau du nombre de Workshops actifs.

## Ce qui reste a trancher (hors MVP initial)

- `vm-supervisor` pilote reellement Firecracker **jaile** : boot depuis un
  kernel/rootfs, snapshot (pause + `snapshot/create`), restauration
  (`snapshot/load` dans un nouveau jail) — via `fctools`
  (`crates/vm-supervisor/src/vm.rs`), teste en conditions reelles (KVM,
  jailer avec capabilities Linux, pas root) contre les artefacts de
  `deploy/dev/firecracker/`. Pas encore de seccomp dedie (le jailer
  applique le seccomp par defaut de Firecracker, pas un profil affine pour
  Atelier). Ce qui manque encore : le canal de controle vsock expose au
  `controller`, la recuperation du kernel/rootfs depuis le cache
  content-addressed (`image_digest`) plutot que des chemins fournis
  directement par variables d'environnement, et — point ouvert non
  resolu — comment reconstituer le `ResourceSystem` source d'une VM au
  moment de la reprise quand celle-ci a lieu dans un tout autre process que
  celui qui a pris le snapshot (le SDK `fctools` modelise la restauration
  comme partant d'une VM source encore en memoire, ce qui ne correspond pas
  telle quelle a un resume declenche bien plus tard depuis un nouveau pod).
- Le pipeline `image-builder` (envbuilder → push OCI → `crane export` →
  ext4 → cache) est implemente et verifie de bout en bout (y compris boot
  Firecracker du resultat), mais le cache est aujourd'hui un simple
  repertoire de dev, pas encore un vrai PVC provisionne par le
  `controller` (ni monte dans le Job image-builder, ni dans le pod parent
  pour que `vm-supervisor` y lise `ATELIER_VM_ROOTFS_PATH`) — reste a
  cabler. Pas de gestion de couches/diffs pour eviter de tout reconstruire
  a chaque revision (chaque build repart de zero). Kernel invite reste un
  fichier fixe, pas encore dans le cache (partage entre tous les Workshops
  pour l'instant, embarque dans l'image `vm-supervisor`).
- Support du sous-ensemble de la spec devcontainer.json a couvrir en premier
  (image simple vs build Dockerfile vs features vs docker-compose multi-service).
- Modele d'autorisation fin cote `mcp-gateway` (quelles demandes de
  l'agent sont auto-approuvees vs necessitent une validation humaine).
- Le `controller` provisionne un service account Kanidm et un role OpenBao
  par Workshop (`crates/controller/src/{kanidm,openbao}.rs`, tous deux
  optionnels), et les nettoie a la suppression du Workshop via un finalizer
  Kubernetes (`atelier.dev/cleanup`) — verifie en conditions reelles
  (creation, suppression, entite/role bien absents ensuite). A travers un
  cycle suspend/resume, ils sont deliberement laisses intacts (seul le pod
  parent est libere/recree), verifie egalement en conditions reelles.
- `identity-proxy` sait s'authentifier aupres d'OpenBao et lister les
  secrets disponibles, mais rien n'ecrit encore de secrets utiles pour un
  Workshop donne (pas de mapping avec `WorkshopSpec.tools`/`egress_allowlist`),
  et il ne fait pas encore office de proxy HTTP(S) qui intercepte et enrichit
  les appels sortants de l'agent — seulement l'authentification et le
  listing, cf. TODO dans `crates/identity-proxy/src/main.rs`.
- Politique d'auto-suspend (delai d'inactivite avant snapshot automatique) et
  compatibilite des snapshots entre versions de kernel/Firecracker (un
  snapshot fige une version precise ; que faire s'il faut mettre a jour le
  kernel invite entre deux reprises ?).
- Stack d'observabilite complet (collector deploye, backend de stockage des
  traces/metriques, **Grafana** + dashboard dedie) — pour l'instant seule
  l'instrumentation applicative (OpenTelemetry) est en place, testee contre
  un collecteur local ad hoc, voir section « Observabilite » ci-dessus.

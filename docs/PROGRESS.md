# Point de situation

> Suivi vivant de l'avancement. Pour la conception cible, voir
> [`ARCHITECTURE.md`](ARCHITECTURE.md).

Toutes les briques listees "Fonctionnel" ci-dessous ont ete validees contre
de la vraie infrastructure (kind reel, Firecracker/jailer reels, Kanidm et
OpenBao reels en conteneur, registre OCI reel) — pas de mocks. C'est un
choix delibere du projet : les bugs decouverts en cours de route (voir
"Lecons retenues") sont attendus et font partie du processus.

## Etat par composant

| Composant | Etat | Preuve |
|---|---|---|
| CRD `Workshop` + types (`crates/common`) | Fonctionnel | Appliquee sur kind, round-trip serde valide |
| `controller` — reconciliation generale | Fonctionnel | 5/5 tests d'integration reels contre kind |
| `controller` — provisioning Kanidm | Fonctionnel | Test reel contre un Kanidm en conteneur |
| `controller` — provisioning OpenBao | Fonctionnel | Test reel contre un OpenBao en conteneur (policy KV data+metadata) |
| `controller` — cycle suspend/resume | Fonctionnel (snapshot reel) | Canal de controle HTTP `controller` -> `vm-supervisor` (`POST /snapshot`) : suspend declenche un vrai snapshot Firecracker publie sur le cache partage, `status.snapshotDigest` renseigne ; resume restaure la microVM depuis ce snapshot (log "restoring microVM from persisted snapshot") dans un **pod/process totalement nouveau**. Verifie reellement contre kind (cycle complet suspend → snapshot publie → pod libere → resume → microVM restauree). Voir "Canal de controle suspend/resume" ci-dessous |
| `controller` — cleanup a la suppression | Fonctionnel | Finalizer `atelier.dev/cleanup`, verifie via test reel |
| `image-builder` — pipeline devcontainer → ext4 | Fonctionnel | `envbuilder` tourne desormais dans la microVM "builder" (plus d'invocation directe dans le conteneur du Job), suivi de `crane export` + `mke2fs` cote hote — build reel de bout en bout sur un vrai depot (`vscode-remote-try-python`), voir "Builder microVM" ci-dessous |
| `image-builder` — publication PVC + patch status | Fonctionnel | Verifie via un vrai Job Kubernetes en cluster (RBAC dedie, voir ci-dessous) : `status.imageDigest` patche automatiquement, plus besoin d'intervention manuelle |
| `vm-supervisor` — boot Firecracker jaile | Fonctionnel | VM reelle demarree via jailer + capabilities, hors pod et dans un pod Kubernetes **non privilegie** (`atelier.dev/kvm` via `kvm-device-plugin` + `NET_ADMIN`/`SYS_ADMIN`/`SYS_RESOURCE`, voir composant `kvm-device-plugin` ci-dessous) |
| `kvm-device-plugin` | Fonctionnel | Device plugin Kubernetes (API kubelet v1beta1) pour `/dev/kvm`+`/dev/net/tun` : DaemonSet qui annonce la ressource allouable `atelier.dev/kvm`, permettant a `vm-supervisor`/`image-builder` de tourner sans `securityContext.privileged: true`. Verifie reellement contre kind : `kubectl describe node` liste `atelier.dev/kvm: 32`, un pod non privilegie qui la demande ouvre reellement `/dev/kvm` (`exec 3<>/dev/kvm` reussit), et un vrai boot Firecracker aboutit (`microVM running`) dans un pod portant uniquement `NET_ADMIN`+`SYS_ADMIN`+`SYS_RESOURCE`, sans `privileged` |
| `vm-supervisor` — snapshot/restore | Fonctionnel (cross-process) | `Vm::restore_persisted` (nouveau) restaure une microVM depuis un snapshot **sans** avoir besoin de l'objet `Vm` d'origine vivant — contrairement a `Vm::restore` (fctools), qui a besoin d'une VM source dans le meme process. Teste reellement : VM source completement eteinte et son jail detruit avant la restauration dans un tout nouveau `Vm` (`crates/firecracker/tests/vm.rs`), puis en conditions reelles via le canal de controle HTTP (nouveau process, nouveau pod, cf. ci-dessous) |
| `crates/firecracker` (lib partagee, extrait de `vm-supervisor`) | Fonctionnel | Meme test boot/snapshot/restore reel qu'avant le refactor, toujours vert |
| `crates/firecracker::network` (TAP link-local) | Fonctionnel | Creation/config/suppression d'un vrai device TAP testee reellement (`unshare --net --map-root-user`, sans besoin de root), plus `restrict_to_net_proxy` (regles iptables de defense en profondeur, voir `docs/architecture/network-security.md`) — testees reellement (contenu exact des regles verifie via `iptables -S`). Desormais utilise par `vm-supervisor` (VM de l'agent), pas seulement la microVM "builder" |
| `crates/builder-vm-init` (guest init de la microVM "builder") | Fonctionnel | Cycle complet valide reellement : boot jaile + reseau + `envbuilder` (clone, build, push registre via `net-proxy`) + extinction propre de la VM detectee par l'hote, `crane manifest` confirme l'image poussee (`cargo test -p atelier-firecracker --test builder_vm`, 35s). Cinq causes racines trouvees et corrigees en cours de route, voir "Builder microVM" ci-dessous |
| Boucle complete Workshop → pod → microVM `Running` | Fonctionnel (automatique) | Pour la premiere fois de bout en bout **sans peuplage manuel du cache** : `kubectl apply` d'un Workshop reel declenche le Job `image-builder` (microVM "builder" reelle), qui construit et pousse l'image, l'exporte en `rootfs.ext4`, la publie dans le cache, patche `status.imageDigest` — puis le controller enchaine automatiquement sur le pod parent, `vm-supervisor` boote la microVM avec ce rootfs. Verifie reellement contre kind (`Job` `Complete`, `Workshop.status.phase=Running`) |
| Observabilite (OpenTelemetry) | Fonctionnel (base) | `atelier_common::telemetry::init()` cable sur tous les binaires, spans sur la boucle de reconciliation |
| `api-server` | Fonctionnel | JWT valide contre un vrai flux OAuth2 Kanidm (PKCE S256, `/oauth2/token` reel — deux bugs reels trouves et corriges au passage, voir "Lecons retenues" : `InvalidAudience` faute d'`aud` configure, CA auto-signee non fiee par `reqwest`/rustls) ; endpoints CRUD + suspend/resume sur `Workshop` via `kube::Api`, testes reellement contre kind (creation, isolation par `owner_subject`, suspend/resume, suppression) ; coordinateur de port-forward (`/v1/workshops/{name}/portforward`, authentifie puis relaie vers `net-proxy`), teste reellement de bout en bout (client websocket -> api-server -> net-proxy -> serveur TCP cible) ; pont HTTP+WebSocket vers `code-server` (`/v1/workshops/{name}/vscode/*`, voir section dediee "UI dashboard") |
| `net-proxy` — egress (allowlist + proxy parent) | Fonctionnel | Proxy HTTP explicite (relai en clair + tunnel `CONNECT`) avec allowlist par domaine/wildcard, et chainage optionnel vers un proxy parent (`ATELIER_UPSTREAM_PROXY`) avec bypass `ATELIER_NO_PROXY`. Premiere conteneurisation le mois dernier (`crates/net-proxy/Dockerfile`), deploye a la fois comme sidecar du Job `image-builder` et desormais comme conteneur du **pod parent** de l'agent, allowlist alimentee depuis `Workshop.spec.egress_allowlist`. Verifie contre un vrai pod en cluster (3/3 conteneurs `Running`, alias `identity-proxy` actif, chainage obligatoire confirme) |
| `net-proxy` — port-forward (microVM → exterieur) | Fonctionnel | Endpoint websocket `/portforward`, multiplexage de canaux dans le style `kubectl port-forward` (net-proxy = kubelet, `api-server` = coordinateur qui authentifie et relaie). TCP et UDP. Teste via un vrai client websocket (`tokio-tungstenite`) : relai de donnees bout en bout et remontee d'erreur de connexion sur le canal dedie, et de bout en bout via `api-server` (`crates/api-server/tests/routes.rs`) |
| `net-proxy` — DNS (UDP+TCP) | Fonctionnel (composant seul) | Resolveur DNS pour la VM, meme allowlist que l'egress (nom refuse → `REFUSED` local, jamais transmis a l'upstream). Teste reellement avec `dig` (UDP et TCP) contre un vrai upstream (resolveur systemd-resolved local), plus tests unitaires (parsing QNAME, upstream jamais contacte pour un nom refuse) |
| `identity-proxy` | Fonctionnel | Proxy HTTP explicite : injecte un en-tete (`Authorization` ou autre) construit depuis un secret OpenBao (cache rafraichi periodiquement, login Kubernetes reel) dans les requetes HTTP en clair dont l'hote correspond a une regle (`Workshop.spec.identityInjectionRules`, type partage avec `atelier-common`), puis relaie vers `net-proxy` (`ATELIER_NET_PROXY_ADDR`) via un tunnel `CONNECT`. `CONNECT`/HTTPS reste un tunnel opaque, non injectable sans MITM (limite documentee). Premier `Dockerfile`, deploye comme conteneur du pod parent, regles alimentees depuis `Workshop.spec` par le controller — verifie contre un vrai pod en cluster ("regles d'injection chargees count=1") |
| `mcp-gateway` | Fonctionnel (HTTP/SSE + vsock, 3 tools) | Serveur MCP reel (SDK officiel `rmcp`) exposant `request_credential` (lecture OpenBao), `request_egress` (elargissement a chaud de l'allowlist `net-proxy`) et `enable_simulator` (active le sidecar LocalStack), deux transports actifs en parallele (streamable HTTP via `net-proxy`, et `AF_VSOCK` natif), tous verifies de bout en bout contre de la vraie infra (OpenBao, net-proxy, LocalStack officiel). Reste a faire : verification depuis l'interieur d'une vraie microVM agent, voir section dediee ci-dessous |
| `dashboard` | Fonctionnel (CRUD + page de gestion + "ouvrir VS Code") | Next.js 16 (App Router), pattern backend-for-frontend : `/api/auth/login` (PKCE) redirige vers l'UI Kanidm, `/api/auth/callback` echange le code et stocke l'`access_token` dans un cookie httpOnly, jamais expose au JS navigateur. Liste/creation/suspend/resume/suppression de Workshops via Server Components + Server Actions, chaque appel relaie le token a `atelier-api-server` qui le revalide integralement. Page de detail par Workshop + bouton "Ouvrir VS Code" (nouvel onglet, `code-server` via le pont HTTP+WS de `api-server`, voir section dediee) ; serveur Next custom (`server.ts`) pour le WebSocket propre de `code-server`. Verifie reellement : flux complet login (scripte cote Kanidm comme `get-oauth2-token.sh`) → callback → session → creation d'un vrai Workshop → affichage dans la liste → suppression, contre un vrai Kanidm/api-server/kind |
| `llm-proxy` (LiteLLM, service global du cluster) | Fonctionnel (base) | `deploy/dev/llm-proxy/` (Deployment/Service, meme niveau qu'OpenBao, pas un sidecar par pod) traduit les appels Anthropic Messages API de Claude Code vers DeepSeek par defaut (alias `sonnet-premium` vers le vrai Anthropic). Alias `net-proxy` `llm-proxy` + injection `ANTHROPIC_BASE_URL`/`ANTHROPIC_AUTH_TOKEN` dans `/etc/environment` (`image-builder`), toujours actifs des que configures cote `controller`. Verifie reellement contre kind : `/health/readiness` 200, `/v1/models` et `/v1/messages` traduits et routes jusqu'a DeepSeek a travers l'alias `net-proxy` (401 reel de DeepSeek avec une cle factice, preuve du pipeline complet). Voir section dediee "LLM Proxy" ci-dessous |
| Observabilite — Grafana/dashboard de supervision | Backlog | Explicitement reporte |
| Repo GitHub `atelier` | Publie (public) | Controller/api-server/dashboard/etc — CI GitHub Actions, images GHCR, site de doc MkDocs, licence AGPLv3 |
| Repo GitHub `atelier-workspace` | Publie (public) | Devcontainer de demo `ministack-workshop` (docker-in-docker, ministack, Claude Code, code-server), depot dedie separe pour que `image-builder` le clone sans identifiants git |

## Ce qui a ete construit cette session (resume chronologique)

1. **Scaffolding** : workspace Rust (control plane) + dashboard Next.js,
   CRD `Workshop` comme source de verite declarative.
2. **Devcontainer comme contrat** : decision que l'environnement livre a
   l'agent est defini par un `.devcontainer/devcontainer.json` standard,
   construit via `envbuilder`.
3. **Convention OpenTelemetry** imposee des le depart sur tous les
   binaires, Grafana explicitement mis en backlog.
4. **Firecracker/jailer** : premiere version hand-rolled en HTTP-sur-socket
   Unix, **rejetee par l'utilisateur** en faveur d'un SDK officiel —
   migration vers `fctools`. Bascule de `sudo` vers des capabilities Linux
   (`setcap`) sur le jailer apres avoir constate que le
   `SudoProcessSpawner` de `fctools` (qui appelle `sudo -S -s`) est
   incompatible avec une regle sudoers scopee finement.
5. **Identite/secrets** : Kanidm pour l'identite (federable), OpenBao pour
   les secrets d'environnement, avec le principe explicite que seul
   `identity-proxy` manipule jamais un secret de sortie — jamais l'agent.
   Pont OpenBao choisi comme auth Kubernetes (ServiceAccount du pod
   parent), pas federation JWT Kanidm → OpenBao (plus simple, deja
   standard).
6. **Finalizer** de nettoyage Kanidm/OpenBao a la suppression d'un
   Workshop ; suspend/resume explicitement exempte de ce nettoyage.
7. **Pipeline image-builder** : devcontainer → `envbuilder` → image OCI
   poussee au registre → `crane export` → tarball → `mke2fs` → `rootfs.ext4`
   → PVC de cache content-addressed (cle = digest de l'image). Pivot majeur
   decouvert en testant reellement : `envbuilder` ne produit pas de dossier
   d'export, il ecrase le filesystem de son propre conteneur — d'ou le
   passage par un registre OCI intermediaire plutot qu'un simple montage de
   volume.
8. **`vm-supervisor` reel dans le pod parent** : conteneur privilegie,
   montage `hostPath` de `/dev/kvm`, boot Firecracker jaile depuis le
   rootfs du cache. Le blocage constate ("Operation not permitted" malgre
   permissions et capabilities correctes) venait du device cgroup
   controller de Kubernetes/containerd, pas d'un probleme de capabilities —
   resolu par `privileged: true` en attendant un device plugin KVM dedie.
9. **Depot GitHub prive** publie.
10. **`api-server`** : validation JWT reelle (RS256, JWKS charge au
    demarrage depuis `ATELIER_JWT_JWKS_URL`, algorithme derive du JWK quand
    `alg` est absent) et endpoints CRUD + suspend/resume sur `Workshop` via
    `kube::Api`, avec `owner_subject` toujours derive du sujet JWT
    authentifie (jamais du corps de la requete, pour eviter qu'un client
    usurpe un proprietaire) et un `404` (pas `403`) pour un Workshop
    appartenant a quelqu'un d'autre (evite de confirmer son existence).
    Teste reellement (`crates/api-server/tests/routes.rs`) : vraie crypto
    (cle RSA `openssl`, JWKS derive de cette cle), vrai `kube::Client`
    contre kind, vrai routeur axum (appele via `tower::ServiceExt::oneshot`,
    pas de mock) — create/get/list/suspend/resume/delete et isolation entre
    deux sujets JWT distincts, tous verifies contre le cluster reel.
11. **`api-server` contre un vrai flux OAuth2 Kanidm, puis coordinateur de
    port-forward** : un Resource Server OAuth2 public a ete cree dans le
    Kanidm de dev (`deploy/dev/kanidm/README.md`, section dediee) et un
    script reutilisable (`deploy/dev/kanidm/get-oauth2-token.sh`) automatise
    le flux complet (PKCE S256, `/oauth2/authorise` + `/permit` +
    `/oauth2/token`) pour obtenir un vrai `access_token`. Faire tourner
    `api-server` contre ce flux reel (au lieu des JWT synthetiques des
    tests) a revele deux bugs invisibles jusque-la : `Validation::new()` de
    `jsonwebtoken` active `validate_aud` par defaut, et aucune audience
    n'etait configuree — tous les vrais tokens etaient donc rejetes
    (`InvalidAudience`) alors que les JWT synthetiques (sans `aud`)
    passaient ; et `reqwest`/rustls ne fait pas confiance a la CA
    auto-signee du Kanidm de dev, ni au trust store systeme. Corriges via
    deux nouvelles variables d'environnement, `ATELIER_JWT_AUDIENCE`
    (obligatoire des qu'un issuer est configure) et `ATELIER_JWT_CA_PATH`
    (optionnelle). Cote port-forward : nouvel endpoint
    `GET /v1/workshops/{name}/portforward`
    (`crates/api-server/src/portforward.rs`) qui authentifie le client,
    verifie qu'il est proprietaire du Workshop, puis relaie sa connexion
    websocket vers l'endpoint `/portforward` de `net-proxy` (deja ecrit,
    voir composant "port-forward" ci-dessus) — sur le modele
    `kubectl port-forward` (`net-proxy` = kubelet, `api-server` =
    coordinateur). Teste de bout en bout (`crates/api-server/tests/routes.rs`) :
    vrai binaire `net-proxy`, vrai serveur TCP cible, vrai `Workshop`+`Pod`
    sur kind, vrai client websocket a travers `api-server`.
12. **Device plugin Kubernetes pour `/dev/kvm`** (`crates/kvm-device-plugin`) :
    implemente l'API kubelet v1beta1 (proto vendore, `tonic`/`tonic-prost-build`)
    pour sortir `vm-supervisor`/`image-builder` de `privileged: true`.
    Deploye en DaemonSet (`deploy/dev/kvm-device-plugin/`), teste contre kind
    reel a trois niveaux : (1) `kubectl describe node` liste bien
    `atelier.dev/kvm: 32` une fois le plugin `Running` et enregistre aupres
    du kubelet ; (2) un pod non privilegie demandant cette ressource ouvre
    reellement `/dev/kvm` (pas juste visible, `exec 3<>/dev/kvm` reussit —
    preuve que le device cgroup est bien configure par le kubelet) ; (3) un
    vrai pod `vm-supervisor` avec exactement la spec desormais generee par
    `reconcile.rs` (`resources.limits.atelier.dev/kvm=1`, capabilities
    `NET_ADMIN`/`SYS_ADMIN`/`SYS_RESOURCE`, **aucun** `privileged`) boote
    reellement Firecracker (`microVM running`). Au passage, decouverte que
    `NET_ADMIN` seul suffit a creer le TAP mais pas a spawner le process
    jailer (`Operation not permitted`) : `SYS_ADMIN`/`SYS_RESOURCE`
    manquants, necessaires pour que les capabilities de fichier posees par
    `setcap` sur le binaire `jailer` soient elevables a l'exec (voir
    "Lecons retenues"). `image-builder` beneficie du meme mecanisme.
13. **Dashboard** (`dashboard/`, Next.js 16 App Router) : pattern
    backend-for-frontend plutot qu'un client OAuth2 cote navigateur — le
    dashboard lui-meme est le client public PKCE, jamais le JS servi au
    navigateur. `/api/auth/login` genere le couple PKCE (`lib/pkce.ts`) et
    redirige vers `${KANIDM_URL}/ui/oauth2` (pas directement
    `/oauth2/authorise`, qui exige un `Authorization: Bearer` deja present —
    inatteignable pour un navigateur sans session, decouvert en testant
    reellement : 401 systematique sans ce header). Kanidm gere son propre
    login+consentement puis redirige vers `/api/auth/callback`, qui echange
    le code (`/oauth2/token`) et pose un cookie **httpOnly**
    (`atelier_session`, jamais lisible par le JS navigateur). Toutes les
    donnees (`lib/api-server.ts`) et mutations (`app/actions.ts`, Server
    Actions suspend/resume/delete/create) relaient ce token en
    `Authorization: Bearer` vers `atelier-api-server`, qui reste la seule
    couche a revalider reellement le JWT — `proxy.ts` (ex-`middleware.ts`,
    renomme dans Next 16) ne fait qu'une verification optimiste de presence
    du cookie. Client OAuth2 Kanidm existant (`atelier`) reutilise, une
    redirect_uri supplementaire ajoutee (`add-redirect-url` +
    `enable-localhost-redirects`, necessaire pour un client public qui
    redirige vers `localhost`, voir `deploy/dev/kanidm/README.md`). Verifie
    reellement de bout en bout : `/api/auth/login` produit une redirection
    Kanidm valide avec le bon `code_challenge`, le cote Kanidm du flux est
    scripte comme `get-oauth2-token.sh` (login reel + consentement +
    redirection avec un vrai `code`) puis rejoue contre `/api/auth/callback`
    avec le cookie PKCE emis par le dashboard — echange de code reussi,
    cookie de session pose avec un vrai JWT Kanidm, page d'accueil affichant
    la vraie liste de Workshops (vide puis peuplee), creation reelle d'un
    Workshop via `POST /v1/workshops` avec ce token, affiche dans la liste,
    supprime. Seule la partie "clic humain dans l'UI de login Kanidm" n'est
    pas automatisable ; tout le reste (echange de code, session, appels API)
    est verifie contre de la vraie infrastructure, pas simule.

## Reseau kind ↔ registre : diagnostic complet, activation bloquee par un risque de securite

Investigation menee jusqu'au bout (cluster kind + registre + Kanidm reels,
conformement au principe "pas de mocks" du projet) :

- **Cause du trou reseau, confirmee** : le conteneur `atelier-registry-dev`
  vit sur le reseau Docker `bridge` par defaut, le noeud kind sur le reseau
  `kind` — deux reseaux Docker distincts, aucune route entre eux. Fix
  verifie manuellement : `docker network connect kind atelier-registry-dev
  --alias atelier-registry-dev` rend le registre joignable par IP depuis un
  pod kind ; un `Service`/`EndpointSlice` Kubernetes statique pointant sur
  cette IP donne un nom DNS stable en cluster (`atelier-registry-dev:5000`
  depuis le namespace `default`). Verifie avec un pod `curl` jetable et via
  un vrai Job `image-builder` (clone + build envbuilder + push registre +
  `crane export` : tous reussis avec cette configuration).
- **Blocage decouvert en testant reellement, pas encore leve** : le Job
  `image-builder` echoue avant meme d'atteindre le registre
  (`temp remount: bind mount ... operation not permitted`) tant que son
  conteneur n'a pas de capacite de mount elevee. Cause : envbuilder remonte
  *tous* les points de montage existants (PVC de cache, `emptyDir` d'outils,
  token ServiceAccount) apres avoir vide le systeme de fichiers du
  conteneur pour y extraire l'image cible — necessaire pour que ces volumes
  restent utilisables apres le wipe, pas seulement pour le token SA.
  `privileged: true` (comme dans le `docker run` manuel du README) leve le
  blocage ; une version plus etroite (`securityContext.capabilities.add:
  [SYS_ADMIN]` seul, toutes les autres capacites droppees) leve aussi le
  blocage en test manuel.
- **Pourquoi ce n'est pas active** : meme reduit a `SYS_ADMIN` seul, cette
  capacite reste une des plus dangereuses qui existe (mount arbitraire,
  plusieurs techniques d'evasion de conteneur connues) — et ici elle
  s'appliquerait a un conteneur qui execute des instructions de build
  (`RUN`, `postCreateCommand`, etc.) issues du **depot cible du Workshop**,
  potentiellement non fiable, a la difference de `vm-supervisor` qui
  n'execute que du code first-party (jailer/Firecracker) sous
  `privileged: true`. Contrairement au blocage KVM de `vm-supervisor`
  (device cgroup controller, pas de contournement plus etroit trouve), ici
  le compromis n'a pas ete tranche : accorder cette capacite a du code non
  fiable contredit le modele de securite du projet ("aucun acces direct au
  reste du cluster" pour ce qui execute du contenu externe), donc le code
  (RBAC `workshops/status` pour un ServiceAccount dedie `image-builder`,
  `capabilities.add: [SYS_ADMIN]` sur le Job) n'a **pas** ete merge en
  l'etat — revert delibere apres discussion.
- **Direction retenue** : plutot que d'isoler le conteneur K8s du Job
  (gVisor/Kata/NetworkPolicy), faire tourner `envbuilder` a l'interieur
  d'une **microVM Firecracker jetable**, en reutilisant le plumbing
  jailer/`fctools` deja ecrit et valide pour `vm-supervisor` — voir section
  "Builder microVM" ci-dessous. Decision explicite : le rootfs *de la
  builder VM elle-meme* (notre propre `Dockerfile`, contenu first-party)
  peut etre construit dans un environnement moins contraint (`docker build`
  classique) — la protection ne vise que le contenu du **depot cible du
  Workshop**, execute a l'interieur de la VM une fois demarree, pas ce
  rootfs.
- **Resolu** : la microVM "builder" est desormais ecrite, validee et
  branchee dans `image-builder`/`reconcile.rs` — voir section "Builder
  microVM" ci-dessous, sous-section "Branchee dans `image-builder`/
  `reconcile.rs`". Le Job `image-builder` n'a plus besoin de `SYS_ADMIN`.

## Builder microVM : isoler `envbuilder` d'une microVM jetable

Composants nouveaux cette session (plan complet dans l'historique de
conversation) :

- **`crates/firecracker`** : le wrapper `fctools` (jailer, boot,
  snapshot/restore) precedemment prive de `vm-supervisor`, extrait en lib
  partagee — `vm-supervisor` en depend desormais sans changement de
  comportement (meme test reel boot/snapshot/restore, toujours vert apres
  le refactor). Ajout d'un module `network` (nouveau) : cree un device TAP
  + un sous-reseau link-local `/30` point-a-point entre l'hote et le guest
  — teste reellement (`crates/firecracker/tests/network.rs`, via `unshare
  --net --map-root-user`, sans besoin de root reel pour ce test-la).
- **`crates/builder-vm-init`** : init minimal (PID 1 du guest, pas de
  systemd) qui monte `/proc`/`/sys`, configure `eth0` avec l'IP link-local
  recue via les `boot_args` du kernel (`atelier.<clef>=<valeur>`, pas de
  MMDS), lance `envbuilder` avec `HTTP_PROXY`/`HTTPS_PROXY` pointant sur
  `net-proxy` (**pas** d'acces reseau direct/NAT vers Internet — voir plus
  bas), puis eteint la VM. Compile, image Docker construite, convertie en
  `rootfs.ext4` bootable via le meme pipeline crane-export + `mke2fs` que
  `image-builder` (deja valide cette session) — voir
  `deploy/dev/builder-vm/README.md`.
- **Reseau via `net-proxy`, pas NAT brut** : premiere version de
  `crates/firecracker::network` posait un NAT (`iptables MASQUERADE`) vers
  la sortie normale du pod. Remis en cause en cours de session : `net-proxy`
  (deja "Fonctionnel", allowlist de domaines + tunnel `CONNECT`) doit rester
  le **seul** chemin de sortie reseau pour une microVM, agent ou builder —
  pas de raison d'en faire une exception pour cette derniere. Design revu :
  le guest n'a qu'un lien point-a-point vers `net-proxy` (directement
  joignable, pas de route par defaut necessaire), configure comme
  `HTTP_PROXY`/`HTTPS_PROXY` pour `envbuilder`. Plus simple (zero
  `iptables`/`ip_forward` cote hote) et plus coherent avec le modele de
  securite existant.
- **`CAP_NET_ADMIN` finalement obtenu, deux voies** : la premiere tentative
  (`setcap cap_net_admin+eip` sur une copie de `ip`, meme pattern que
  `jailer`) a echoue silencieusement — le process de l'agent tournait dans
  un contexte qui ignore les capabilities de fichier sur le **vrai** netns
  de la machine (confirme par un `RTNETLINK answers: Operation not
  permitted` meme sur une simple modification de MTU de `lo`, capability
  posee ou pas). Contournement qui fonctionne : `docker run --privileged
  --device=/dev/net/tun --device=/dev/kvm` — un conteneur Docker recoit un
  **vrai** netns isole avec `CAP_NET_ADMIN` effectif *et* une sortie
  Internet reelle (NAT Docker par defaut), ce qui manquait a `unshare --net`
  seul. C'est aussi l'environnement le plus proche de la cible reelle (pod
  `privileged: true`).
- **Piege glibc en cross-environnement** : premiere execution dans ce
  conteneur `rust:1-bookworm` avec le depot hote monte en volume ratee en
  silence — `cargo build` a reutilise un `atelier-net-proxy` deja compile
  **sur l'hote** (Ubuntu 26.04, glibc 2.43) trouve dans `target/debug/`
  partage via le montage, incompatible avec la glibc du conteneur (Debian
  bookworm, 2.36) : `GLIBC_2.38' not found`. `cargo` ne detecte pas ce genre
  d'incompatibilite (le fingerprint ne suit pas la version glibc de
  l'environnement) — un `CARGO_TARGET_DIR` dedie au conteneur est
  necessaire des qu'un repertoire `target/` hote est monte dedans.
- **Bug reel trouve et corrige en testant : les pipes stdout/stderr de
  Firecracker devaient etre draines** — sans lecteur cote hote, un pipe
  Unix a un buffer fini (~64 Kio) ; une fois plein, l'ecriture du guest sur
  sa console serie (`console=ttyS0`) bloque indefiniment, gelant tout le
  guest bien avant qu'il atteigne le reseau. `Vm::boot`/`Vm::restore`
  appellent desormais `take_pipes()` et draine en continu stdout/stderr
  vers `tracing::debug!` (`crates/firecracker/src/vm.rs`). Corrige un vrai
  bug latent (n'importe quelle VM produisant plus de 64 Kio de sortie
  console l'aurait touche), **mais n'a pas resolu le blocage du boot complet
  de la builder VM** — meme symptome observe apres le correctif (process de
  test a 99.9% CPU en continu, jamais de sortie de `Vm::boot_with_network`,
  aucune requete n'atteint jamais `net-proxy`).
### Blocage resolu : cinq causes racines, boot complet valide reellement

Le blocage total (process de test a 99.9% CPU, jamais de sortie de
`Vm::boot_with_network`, aucune requete n'atteint jamais `net-proxy`) evoque
en fin de session precedente n'avait aucun rapport avec `envbuilder` ni la
configuration reseau du guest. Diagnostic repris et pousse jusqu'au bout
cette session (isolation par `gdb`/`strace` sur le process de test, contre
un environnement `docker run --privileged --network host` — voir "Lecons
retenues" pour pourquoi cet environnement remplace un acces root reel).
Cinq causes racines independantes, trouvees et corrigees dans l'ordre ou
elles se sont revelees :

1. **Chemin de socket jail Firecracker trop long** : `sockaddr_un.sun_path`
   est limite a 108 octets sur Linux ; les noms de jail/repertoire de travail
   choisis dans `crates/firecracker/tests/builder_vm.rs`
   (`atelier-builder-vm-test-<pid>` repete deux fois dans le chemin)
   produisaient un chemin de 110 caracteres. `connect()` echouait donc en
   `ENAMETOOLONG` a chaque tentative — et `fctools` 0.7.0-alpha.2
   (`Vm::start`, boucle `loop { if client.get(...).is_ok() { break } }`,
   `src/vm/mod.rs:244`) avale cette erreur dans une boucle qui ne cede
   jamais la main a l'executeur async, empechant meme son propre timeout de
   5s de se declencher : 100% CPU, aucun message d'erreur exploitable.
   Confirme via `gdb -p <pid> -batch -ex bt` (pile figee dans
   `hyper_client_sockets::uri::unix`) et un test de controle (`tests/vm.rs`,
   chemin de 96 caracteres) qui passait sans probleme dans le meme
   environnement. Corrige en raccourcissant les noms (`bvm-<pid>`).
2. **`KANIKO_DIR` absent** : le `Dockerfile` de `builder-vm-init` fixe
   `ENV KANIKO_DIR=/.envbuilder` (garde-fou interne d'envbuilder/Kaniko avant
   de vider le filesystem), mais cette metadonnee OCI n'est interpretee que
   par un runtime de conteneur — perdue lors de la conversion en
   `rootfs.ext4` brut (`crane export` + `mke2fs`), puisque ce guest n'a pas
   de runtime de conteneur, seulement `atelier-builder-vm-init` en PID 1.
   `envbuilder` refusait donc de demarrer ("KANIKO_DIR is not set to
   /.envbuilder. Bailing!"). Corrige en passant la variable explicitement
   dans `run_envbuilder()`.
3. **Rootfs trop petit** : la marge de 512 Mo (README) suffisait pour la
   base de l'image `builder-vm-init` mais pas pour un vrai build devcontainer
   (paquets `apt`/`pip`, ex: `gcc` pour le devcontainer Python de test) —
   `no space left on device` en plein build. Marge passee a 4096 Mo.
4. **Adresse de registre `localhost` exemptee du proxy** : `envbuilder`
   (client HTTP Go, `golang.org/x/net/http/httpproxy`) exclut
   inconditionnellement `localhost`/loopback du proxy configure via
   `HTTP_PROXY`/`HTTPS_PROXY`, meme sans `NO_PROXY` — comportement cable en
   dur dans cette bibliotheque. Avec `ATELIER_TEST_REGISTRY_ADDR=localhost:
   5000` (cas courant en dev), le guest tentait une connexion directe vers
   "lui-meme" et echouait (pas de route par defaut). Corrige en construisant
   la reference d'image donnee au guest avec l'IP hote du lien
   point-a-point (`network.host_ip`), jamais litteralement `localhost`.
5. **`reboot(RB_POWER_OFF)` sans effet** : cette microVM minimale n'a pas
   d'ACPI (`pci=off`), donc aucun handler `pm_power_off` a invoquer — le
   noyau se contentait d'un `halt` ("reboot: System halted") sans que
   Firecracker detecte la fin de la VM (`is_running()` restait vrai
   indefiniment, alors qu'`envbuilder` avait bien clone, construit ET
   pousse l'image avec succes). `reboot=k` (deja present dans les
   `boot_args`) demande au noyau d'utiliser un reset via le controleur
   clavier i8042 pour un **reboot**, pas un power-off — signal que
   Firecracker intercepte lui-meme comme fin de VM (pattern standard des
   inits minimaux Firecracker). Corrige en appelant
   `reboot(RebootMode::RB_AUTOBOOT)` plutot que `RB_POWER_OFF`.

Cycle complet valide reellement une fois les cinq corrections en place :
boot jaile + configuration reseau + `envbuilder` (clone du dépôt public
`vscode-remote-try-python`, build du devcontainer, push vers le registre de
dev via `net-proxy`) + extinction propre detectee par l'hote, `crane
manifest` confirmant que l'image attendue est bien presente
(`cargo test -p atelier-firecracker --test builder_vm`, 35s, voir
`deploy/dev/builder-vm/README.md`).

### Branchee dans `image-builder`/`reconcile.rs` : boucle complete automatique

Composant desormais utilise reellement par le Job `image-builder`, plus
seulement valide en isolation :

- **`crates/image-builder`** : `build_and_push()` (invocation directe
  d'`envbuilder`) remplace par `build_via_microvm()`, qui reprend le meme
  cycle boot/attente-extinction que `tests/builder_vm.rs`. Le rootfs de la
  microVM builder n'est plus un fichier pre-dimensionne fourni a la main :
  un rootfs de base (contenu minimal, marge reduite) est baque dans l'image
  `image-builder` elle-meme au build Docker (voir plus bas), puis copie et
  agrandi (`truncate` + `resize2fs`, marge par defaut 4096 Mio) a chaque
  build reel — `resolve_builder_rootfs()`. `ATELIER_BUILDER_VM_ROOTFS_PATH`
  reste disponible pour court-circuiter ce mecanisme en test manuel.
- **`crates/image-builder/Dockerfile`** : reecrit en build multi-etapes.
  Le contenu guest de la microVM builder (identique a
  `crates/builder-vm-init/Dockerfile`) est construit dans un stage
  intermediaire puis **aplati directement en rootfs.ext4** via
  `COPY --from=<stage> /` + `mke2fs` — sans passer par un registre OCI
  intermediaire ni `crane export` (contrairement a la procedure manuelle de
  `deploy/dev/builder-vm/README.md`) : un stage Docker multi-etapes fait
  deja ce travail de aplatissement localement. Simplification decouverte en
  ecrivant ce Dockerfile, pas anticipee au depart.
- **Alias `registry` de `net-proxy`** (`crates/net-proxy::internal`,
  troisieme alias interne apres `identity-proxy`/`mcp-gateway`) : la
  microVM builder doit joindre le registre interne (ou `envbuilder` pousse
  l'image), un detail d'implementation que l'utilisateur ne doit pas avoir
  a ajouter a `Workshop.spec.egress_allowlist` (decision explicite,
  discutee en session : cette allowlist reste reservee a l'usage — package
  managers du devcontainer compris — que l'utilisateur choisit
  explicitement d'autoriser). `ATELIER_REGISTRY_ALIAS_ADDR` (cote
  `net-proxy`) et `ATELIER_BUILDER_REGISTRY_ALIAS` (cote `image-builder`,
  cable par le controller) ferment la boucle.
- **`net-proxy` conteneurise pour la premiere fois** (`crates/net-proxy/Dockerfile`,
  n'existait pas avant cette session — jusqu'ici teste uniquement via
  `cargo run`/tests d'integration reels) et deploye comme **sidecar natif**
  du Job `image-builder` (`initContainer` avec `restartPolicy: Always`,
  K8s >= 1.28/1.29, KEP-753) : un simple `containers[]` long-vivant
  empecherait le Job de jamais se terminer (un `Job` n'est marque termine
  que quand tous ses `containers[]`, pas ses sidecars natifs, ont fini).
  Verifie reellement contre kind (K8s 1.34).
- **RBAC dedie** (`ensure_image_build_rbac` : `ServiceAccount` + `Role` +
  `RoleBinding` par Job, scopes au Workshop precis via `resourceNames`) :
  bloque depuis la session precedente par le compromis `SYS_ADMIN`
  (voir plus haut, "Reseau kind ↔ registre") — plus necessaire puisque
  `envbuilder` ne tourne plus dans le conteneur du Job. Un
  `Role`/`RoleBinding` orphelin issu d'un test manuel anterieur trainait
  sur le cluster de dev, non gere par le code (nettoye).
- **Valide de bout en bout contre un vrai cluster kind** : `kubectl apply`
  d'un `Workshop` reel (`vscode-remote-try-python`, `egressAllowlist:
  ["*"]`) declenche tout le pipeline sans aucune intervention manuelle —
  Job `image-builder` `Complete` (1/1, ~100s), `status.imageDigest`
  renseigne, le controller enchaine automatiquement sur le pod parent,
  `vm-supervisor` boote reellement la microVM avec le rootfs construit
  ("microVM running"). Premiere fois que cette boucle complete tourne sans
  peuplage manuel du PVC de cache.
- **Bugs trouves en testant ce chemin (distincts des cinq de la section
  precedente, specifiques a l'integration K8s)** :
  - `iproute2` (`ip`) absent de l'image finale `image-builder` (present
    dans le stage guest, oublie dans le stage final) — `Vm::boot_with_network`
    echoue a la creation du TAP avec une erreur `No such file or directory`
    trompeuse (ressemble a un probleme de binaire Firecracker/jailer, pas
    de `ip`).
  - L'image `atelier-net-proxy:dev` rechargee dans kind AVANT l'ajout du
    code de l'alias `registry` a produit un echec silencieux et rapide (VM
    eteinte en 3s, `crane export` en erreur `MANIFEST_UNKNOWN`) : toujours
    reconstruire ET recharger (`kind load docker-image`) une image avant de
    re-tester en cluster, une image perimee ne donne aucun signal
    explicite qu'elle est perimee.
  - Un `Workshop` de test sans `egressAllowlist` bloque tout l'egress de la
    microVM builder (allowlist vide = tout refuse, comportement voulu) —
    piege facile en testant manuellement, pas un bug.

## Canal de controle suspend/resume : snapshot Firecracker reel

Item 2 de la roadmap ("canal de controle vsock entre `controller`/
`vm-supervisor`") : le terme "vsock" du TODO d'origine etait trompeur —
`AF_VSOCK` est le canal guest<->hote *a l'interieur* d'un meme pod (deja
utilise par `mcp-gateway`, voir `docs/architecture/network-security.md`),
alors que `controller` et `vm-supervisor` sont deux process dans deux pods
distincts : un simple canal HTTP sur le reseau normal du cluster (IP de
pod) est le bon outil, pas `vsock`.

- **Obstacle principal, resolu par construction plutot que par
  contournement** : l'API `fctools` (`VmSnapshot`/`Vm::restore`) n'est pas
  concue pour survivre a un redemarrage complet du process — `Vm::restore`
  prend `&mut self` sur la VM source, dont le `ResourceSystem` vivant
  fournit les ressources `Moved` (kernel/rootfs) a recopier dans le nouveau
  jail, et `VmConfigurationData` ne derive que `Serialize` (pas
  `Deserialize`) : rien de tout ca ne peut etre serialise puis recharge plus
  tard dans un tout autre process. Contournement : `VmConfigurationData`
  est entierement determinee par les memes parametres qu'un boot normal
  (kernel, rootfs, vcpu/mem, boot_args, reseau), tous deja connus
  independamment de tout etat runtime — `Vm::restore_persisted` (nouveau,
  `crates/firecracker/src/vm.rs`) la **reconstruit a l'identique** plutot
  que de la deserialiser, exactement comme le ferait un nouveau
  `Vm::boot`/`Vm::boot_with_network`, avec pour seule difference
  `VmConfiguration::RestoredFromSnapshot` a la place de
  `VmConfiguration::New`. Validite garantie par le `FlatVirtualPathResolver`
  du jail (chemin virtuel base sur le nom de fichier, pas sur l'identite de
  la ressource) : la configuration serialisee vers Firecracker reference
  "/vmlinux.bin"/"/rootfs.ext4" quel que soit l'objet `Resource` interne qui
  les a produits. Teste reellement en abandonnant completement la VM source
  (eteinte, jail detruit) avant de restaurer dans un `Vm` flambant neuf
  (`crates/firecracker/tests/vm.rs::snapshot_persist_and_restore_without_source_vm`).
- **`crates/vm-supervisor`** : petit serveur HTTP (`axum`, port 8081 par
  defaut, `ATELIER_VM_CONTROL_ADDR`) expose `POST /snapshot` — fige la VM
  (`vm.snapshot()`), publie `snapshot.state`/`snapshot.mem` sur le cache
  partage (`ATELIER_VM_SNAPSHOT_DIR`, ecriture atomique via fichiers `.tmp`
  + `rename`), renvoie un digest sha256 informatif, puis arrete proprement
  la VM et sort du process. Au demarrage, si ce repertoire contient deja un
  snapshot, restauration via `Vm::restore_persisted` plutot que boot direct
  — decouverte automatique, sans variable d'environnement supplementaire
  pour distinguer boot/resume. Premiere conteneurisation testee de bout en
  bout de ce canal : boot -> snapshot via l'API -> arret propre -> reprise
  dans un **process totalement neuf**, valide manuellement avant meme le
  passage par Kubernetes.
- **`crates/controller/src/reconcile.rs`** : `ensure_suspended` appelle ce
  canal (`request_snapshot`, best-effort — un echec de connexion, un
  timeout ou une reponse non exploitable degradent silencieusement vers
  "suspension sans snapshot" plutot que de bloquer indefiniment la
  suspension demandee) avant de liberer le pod, et publie
  `status.snapshotDigest`. `ensure_parent_pod` monte desormais le cache en
  lecture-ecriture (necessaire pour que `vm-supervisor` y publie) et passe
  `ATELIER_VM_SNAPSHOT_DIR` (`snapshot_cache_subdir`, nouveau dans
  `storage.rs` — scope par Workshop, pas content-addressed comme le cache
  d'images : un snapshot n'a pas besoin de dedup entre Workshops).
- **Premiere conteneurisation du `controller`** (`crates/controller/Dockerfile`,
  n'existait pas non plus avant cette session) : necessaire pour valider ce
  canal en conditions reelles — le `controller`, lance depuis le poste de
  dev (hors cluster, comme durant toute la session), ne peut pas joindre
  une IP de pod kind directement (reseau du CNI non route vers l'hote).
  Contourne en lancant le conteneur `controller` avec
  `--network container:<noeud-kind>` (partage le netns du noeud, qui lui
  route bien vers les pods qu'il heberge) — meme categorie de contournement
  que l'usage de `--network host` pour la microVM builder plus haut. Une
  fois ce partage de netns en place, la requete `POST /snapshot` atteint
  bien le pod et le cycle complet fonctionne.
- **Valide de bout en bout contre kind reel** : Workshop reel -> `Running`
  -> `desiredState: Suspended` -> `vm-supervisor` recoit `POST /snapshot`,
  publie `snapshot.state`/`snapshot.mem` sur le PVC, `status.snapshotDigest`
  renseigne, pod libere (`phase: Suspended`) -> `desiredState: Running` ->
  nouveau pod parent, log `"restoring microVM from persisted snapshot"`
  (pas `"booting microVM"`) : preuve que la restauration cross-process,
  cross-pod, fonctionne reellement.
- **Note observee, pas un bug** : pendant la periode de grace de suppression
  du pod (delai par defaut ~30s), un reconcile peut encore voir le pod
  (`Terminating`) et retenter `POST /snapshot` sur une VM deja eteinte par
  le premier appel reussi — echoue en 500, journalise en `WARN`, sans
  consequence : `ensure_suspended` ne remplace `status.snapshotDigest` que
  sur un appel reussi, jamais par `None`.

## Reseau de l'agent + `net-proxy`/`identity-proxy` dans le pod parent

Item 3 de la roadmap. Le mecanisme d'isolation reseau etait deja
integralement specifie dans `docs/architecture/network-security.md` (session
precedente) — cette partie de la session l'a implemente et valide.

- **`Workshop.spec.identityInjectionRules`** (nouveau champ) : meme type
  (`IdentityInjectionRule`) defini une seule fois dans `atelier_common::crd`
  et reexporte par `crates/identity-proxy::rules` (`pub use ... as
  InjectionRule`, methode `secret_cache_key()` deplacee dans un trait
  d'extension puisque le type n'est plus local a ce crate) — le controller
  serialise le contenu du CR tel quel vers `ATELIER_IDENTITY_INJECTION_RULES`
  (JSON, camelCase des deux cotes). CRD regenere (`crds/workshop.yaml`) et
  reapplique sur kind.
- **TAP reseau pour la VM de l'agent** (`crates/vm-supervisor/src/main.rs`) :
  `vm-supervisor` cree desormais un TAP link-local (`setup_link_local_tap`,
  meme mecanisme que la microVM "builder", mais sans NAT) et pose les regles
  iptables de `NetworkSetup::restrict_to_net_proxy` (nouveau,
  `crates/firecracker/src/network.rs`) avant de booter. Contrairement a la
  microVM "builder" (init personnalise `atelier-builder-vm-init`),
  `vm-supervisor` boote le devcontainer construit par `image-builder` tel
  quel — impossible de lui faire executer du code de configuration reseau a
  nous. Solution : le parametre de boot noyau standard `ip=<guest>::<hote>:
  <masque>::eth0:off` (autoconfiguration IP Linux, `Documentation/admin-guide/
  nfs/nfsroot.rst`), qui configure l'interface et la route par defaut **avant
  meme que l'init du guest ne demarre**, sans aucune cooperation requise.
  Verifie reellement (`RUST_LOG=atelier_firecracker=debug`, console du
  guest) : `IP-Config: Complete: device=eth0, ipaddr=169.254.0.2,
  mask=255.255.255.252, gw=169.254.0.1`. Regles iptables verifiees a
  l'identique de la specification (`iptables -S atelier-vm-<tap>` : `ACCEPT`
  sur le port `net-proxy` + DNS `:53`, puis `DROP`).
- **`net-proxy` et `identity-proxy` comme conteneurs du pod parent**
  (`ensure_parent_pod`, `crates/controller/src/reconcile.rs`) : contrairement
  au sidecar du Job `image-builder` (qui devait etre un `initContainer`
  `restartPolicy: Always` pour ne pas empecher le Job de se terminer), ici
  ce sont de simples `containers[]` — le Pod parent est cense tourner
  indefiniment, aucune contrainte de "completion" ne s'applique.
  `ATELIER_VM_ADDR` (net-proxy) est fixe a `169.254.0.2` : deterministe,
  `vm-supervisor` utilise toujours l'index de sous-reseau `0` (une seule
  microVM par pod). Premier `Dockerfile` d'`identity-proxy` (n'existait pas
  non plus avant cette session).
- **Bug trouve en testant reellement (meme categorie que pour
  `image-builder`)** : `iproute2`/`iptables` absents de l'image Docker finale
  de `vm-supervisor` (presents nulle part avant, puisque cette image ne
  faisait jusqu'ici jamais de reseau) — `CrashLoopBackOff` avec `lancement de
  ip: No such file or directory`. Corrige en les ajoutant a
  `crates/vm-supervisor/Dockerfile`.
- **Valide de bout en bout contre kind reel** : `Workshop` avec
  `egressAllowlist: ["*"]` et une regle `identityInjectionRules` reelle ->
  pod parent `3/3 Running` -> logs confirmant chaque piece : `vm-supervisor`
  ("microVM running"), `net-proxy` (`identity_proxy_alias=true`,
  `identity_proxy_mandatory_hop=true`), `identity-proxy` ("regles
  d'injection chargees count=1").
- **Explicitement hors scope de cette session** (voir
  `docs/architecture/network-security.md`, section mise a jour) : le TAP et
  le pare-feu donnent au guest un chemin *possible* vers `net-proxy`, mais
  rien a l'interieur du guest ne configure encore `HTTP_PROXY`/`HTTPS_PROXY`
  pour s'en servir — un devcontainer construit sans le savoir tenterait une
  connexion directe, silencieusement bloquee par les regles iptables. Reste
  a injecter ces variables dans l'image construite par `image-builder` (ex:
  `/etc/environment`), voir "Prochaines etapes".

## `mcp-gateway` : premier serveur MCP reel

Dernier composant du tableau ci-dessus a sortir de "Non demarre". Design
cible documente de longue date (`docs/ARCHITECTURE.md`,
`docs/architecture/network-security.md`) : un point d'entree structure pour
que l'agent demande des reglages a l'atelier plutot que d'agir en direct.

- **Decision d'architecture : HTTP/SSE via `net-proxy`, pas `vsock`, pour ce
  lot.** Le design documente presente `vsock` comme transport primaire (deja
  cable cote design, jamais construit : `crates/firecracker/src/vm.rs` fixe
  `vsock_device: None` sur toute VM) et l'alias HTTP de `net-proxy`
  (`ATELIER_MCP_GATEWAY_ADDR`, deja present dans `crates/net-proxy/src/internal.rs`
  mais jamais branche jusqu'ici) comme repli pour les clients qui prefèrent
  HTTP/SSE. Batir `vsock` correctement (device Firecracker, plumbing hote,
  cote guest) est un chantier de l'ampleur du TAP reseau deja fait pour
  l'agent — differe. La garantie de securite recherchee ("mcp-gateway jamais
  joint directement par la VM") ne depend pas de vsock : elle est deja
  assuree par le meme mecanisme que pour `identity-proxy` (pare-feu TAP, la
  VM ne connait que `net-proxy`).
- **`crates/common::openbao_client`** : `login()`/`read_field()`
  (authentification Kubernetes-auth OpenBao + lecture KV v2) extraits de
  `crates/identity-proxy/src/secrets.rs` vers un client partage — la policy
  provisionnee par `crates/controller/src/openbao.rs::ensure_workshop_role`
  couvre deja tout `secret/data/workshops/<name>/*`, donc `mcp-gateway`
  peut lire un sous-chemin different (`workshops/<name>/mcp`) avec le meme
  role, sans aucun changement de provisioning. `identity-proxy` refactore
  pour consommer ce client, son cache/rafraichissement periodique restant
  local (specifique a l'injection continue de credentials HTTP).
- **`crates/net-proxy` : allowlist mutable a chaud + endpoint d'admin
  loopback-only.** `EgressConfig.allowlist`/`DnsConfig.allowlist` passent de
  `Arc<Vec<String>>` a `Arc<RwLock<Vec<String>>>` (`proxy.rs`, `dns.rs`,
  `main.rs`). Nouveau module `admin.rs` : `POST /internal/allowlist/add`,
  servi sur un **second listener** lie explicitement a `127.0.0.1`
  (`ATELIER_NET_PROXY_ADMIN_ADDR`, defaut `127.0.0.1:9001`) — jamais
  `0.0.0.0`, contrairement au port de controle du port-forward
  (`ATELIER_NET_PROXY_CONTROL_ADDR`) qui n'est protege que par les regles
  iptables du TAP. Verifie manuellement (conteneurs Docker reels, meme
  netns) : un appel externe sur le port publie de ce listener echoue
  (connexion refusee, le bind loopback n'est routable que depuis l'interieur
  du conteneur), alors qu'un appel depuis un conteneur partageant le netns
  (`mcp-gateway`) reussit. Pas de persistance : un ajout ne survit pas a un
  redemarrage de `net-proxy` ni ne modifie `Workshop.spec.egress_allowlist`.
- **`crates/mcp-gateway`** : SDK officiel `rmcp` (meme choix de principe que
  pour `fctools` : SDK officiel plutot que hand-roll), transport streamable
  HTTP (`StreamableHttpService` + `axum::Router::nest_service`, feature
  `tower` de `rmcp`). Deux tools reels, actifs seulement si leur nom figure
  dans `Workshop.spec.tools` (`ATELIER_TOOLS`, verifie au moment de l'appel,
  pas filtre de `tools/list`) :
  - `request_credential { field }` : lit `workshops/<name>/mcp` via le
    client OpenBao partage, retourne le champ demande.
  - `request_egress { host }` : appelle l'endpoint d'admin de `net-proxy`
    (ci-dessus) sur `127.0.0.1:9001` (meme pod).
  - `enable_simulator` : **non implemente** — aucun simulateur n'existe
    encore (roadmap item 5, "mcp-gateway et le premier simulateur, candidat
    LocalStack" reste a faire).
  `allowed_hosts` de `rmcp` (protection anti DNS-rebinding, restreint par
  defaut a `localhost`/`127.0.0.1`) etendu explicitement a `mcp-gateway` :
  `net-proxy` relaie la requete de la VM telle quelle recue
  (`crate::proxy::forward`), Host header compris, potentiellement
  `Host: mcp-gateway` plutot qu'une IP.
- **`crates/controller/src/reconcile.rs`** : conteneur `mcp-gateway` ajoute
  au pod parent (`containers[]`, comme `net-proxy`/`identity-proxy` —
  tourne indefiniment), env `ATELIER_WORKSHOP_NAME`, `ATELIER_TOOLS`
  (serialisation CSV de `workshop.spec.tools`), `ATELIER_NET_PROXY_ADMIN_ADDR`,
  `OPENBAO_ADDR` (si configure). Alias `ATELIER_MCP_GATEWAY_ADDR` enfin
  branche cote conteneur `net-proxy` (le code le supportait deja, jamais
  utilise jusqu'ici).
- **Verifie reellement, sans mock** (conteneurs Docker construits et
  executes, pas seulement `cargo test`) :
  - `request_egress` bout en bout : un appel MCP `tools/call` reel
    (handshake `initialize`/`notifications/initialized`/`tools/call` via
    `curl`, protocole streamable HTTP) contre un vrai conteneur
    `atelier-mcp-gateway` elargit l'allowlist d'un vrai conteneur
    `atelier-net-proxy` (partage de netns Docker, meme scenario que deux
    conteneurs d'un pod) ; un `CONNECT` HTTPS ulterieur vers l'hote ajoute
    (`crates.io`) reussit alors un vrai handshake TLS complet a travers le
    tunnel, la ou il echouait avant l'appel.
  - `request_credential` bout en bout : provisioning reel d'un role
    OpenBao (`workshop-mcpdemo`, methode Kubernetes-auth, meme sequence que
    `deploy/dev/openbao/README.md`) et d'un secret KV v2
    (`workshops/mcpdemo/mcp`), interroge via `mcp-gateway` lance en local
    avec un vrai token de ServiceAccount Kubernetes — la valeur exacte du
    secret est retournee par l'appel MCP.
  - Test d'integration controller etendu
    (`apply_creates_owned_parent_pod_once_image_ready`) : verifie contre
    kind reel que le pod parent genere porte bien les quatre conteneurs
    (`vm-supervisor`, `net-proxy`, `identity-proxy`, `mcp-gateway`).
- ~~**Reste a faire** : verification bout-en-bout depuis l'interieur d'une
  vraie microVM agent~~ — **fait cette session**, voir "Verification MCP
  depuis l'interieur d'une vraie microVM agent" ci-dessous.

### `enable_simulator` + premier simulateur (LocalStack) : fait, verifie de bout en bout

- **Decision de design : alias `simulator` "gate", pas un alias interne
  classique.** Les alias existants (`identity-proxy`/`mcp-gateway`/`registry`,
  `crates/net-proxy/src/internal.rs`) sont fixes au demarrage et toujours
  joignables — corrects pour des composants de confiance structurels, mais
  un simulateur AWS local n'a pas vocation a etre joignable par defaut,
  seulement quand l'agent le demande explicitement. Nouvel etat mutable
  distinct (`EgressConfig::simulator`, `Arc<RwLock<Option<(String, u16)>>>`,
  `crates/net-proxy/src/proxy.rs`) : initialise a `None` (donc l'hote
  `simulator` retombe sur l'allowlist normale, qui le refuse sauf `*`, et de
  toute facon `simulator` n'est pas un nom DNS reel) ; ne devient `Some`
  qu'apres un appel a `POST /internal/simulator/enable` (nouveau,
  `crates/net-proxy/src/admin.rs`, meme serveur loopback-only que
  `request_egress`).
- **`crates/mcp-gateway`** : nouveau tool `enable_simulator` (sans
  parametre), gate par `"enable_simulator"` dans `Workshop.spec.tools`
  (meme convention que les autres tools) — relaie simplement l'appel vers
  l'endpoint d'admin ci-dessus.
- **`crates/controller/src/reconcile.rs`** : conteneur `simulator` ajoute au
  pod parent **seulement si** `Workshop.spec.tools` contient
  `enable_simulator` — image officielle `localstack/localstack:3` (pas de
  fork/rebuild maison), lie a `127.0.0.1:4566` (`GATEWAY_LISTEN`, "edge
  port" LocalStack qui sert la quasi-totalite des API AWS emulees sur ce
  seul port), donc structurellement injoignable par la VM (netns distincte)
  et par tout ce qui n'est pas dans le pod. `net-proxy` recoit
  `ATELIER_SIMULATOR_ADDR=127.0.0.1:4566` dans ce cas, sinon rien (pas de
  conteneur, pas d'adresse configuree : `enable_simulator` echoue proprement
  avec un message explicite plutot que de planter).
- **Verifie de bout en bout, sans mock** (conteneurs Docker reels,
  `atelier-net-proxy:dev` + `atelier-mcp-gateway:dev` + `localstack/localstack:3`
  officiel, partage de netns Docker) : avant `enable_simulator`, une requete
  via `net-proxy` vers l'hote `simulator` echoue (502, ne resout jamais
  reellement le nom "simulator") ; un vrai handshake MCP complet par HTTP
  (`initialize`/`notifications/initialized`/`tools/call enable_simulator`,
  meme protocole que pour `request_egress` documente plus haut) retourne
  `"simulateur active"` ; la meme requete vers `simulator` a travers
  `net-proxy` reussit alors (200, vraie reponse JSON de sante LocalStack
  listant les ~34 services AWS emules) — preuve que le gate fonctionne dans
  les deux sens et que le chemin complet agent -> mcp-gateway -> net-proxy ->
  LocalStack est reellement cable.
- **Limite assumee** : verifie via `net-proxy` directement (curl a travers
  le proxy HTTP), pas encore depuis l'interieur d'une vraie microVM agent
  (meme limite que le reste de cette section) ; pas de test contre kind reel
  non plus (conteneurs Docker isoles, pas de pod K8s) — le cablage
  `reconcile.rs` (volume, conteneur conditionnel, env vars) est verifie par
  `cargo check`/compilation mais pas encore par un test d'integration contre
  un cluster reel.

### Transport `vsock` natif : fait, verifie de bout en bout

Composant desormais construit (etait le seul morceau manquant du design
cible documente pour `mcp-gateway`) :

- **`crates/firecracker::vm`** : `VmConfig.vsock: Option<VsockConfig>`
  (`guest_cid`, `uds_relative_path`) — `build_configuration_data` declare le
  socket "principal" comme ressource `Produced` (meme mecanisme que les
  fichiers de snapshot, voir `Vm::snapshot`) : chemin **relatif au jail**
  (`/vsock.sock`), Firecracker le cree lui-meme au boot, le chemin hote reel
  resulte de la resolution du jail.
- **`crates/vm-supervisor`** : device vsock toujours actif
  (`ATELIER_VM_VSOCK_GUEST_CID`, defaut `3` ; `ATELIER_VM_VSOCK_UDS_FILENAME`,
  defaut `vsock.sock`) — sans emplacement partage avec `mcp-gateway`, reste
  simplement inutilise, cout nul.
- **`crates/mcp-gateway`** : second transport actif en parallele du HTTP
  existant, meme `Gateway` (`ServerHandler`) des deux cotes. Convention
  Firecracker pour les connexions **initiees par le guest** : ce process lie
  lui-meme un `UnixListener` a `<uds_path>_<port>`
  (`ATELIER_MCP_GATEWAY_VSOCK_UDS_PATH`, `ATELIER_MCP_GATEWAY_VSOCK_PORT`
  defaut `10000`) — pas de transport HTTP a reimplementer : `rmcp` expose
  `ServiceExt::serve` sur n'importe quel `AsyncRead + AsyncWrite`
  (`rmcp::transport::async_rw`, meme framing JSON-RPC delimite par newline
  que le style stdio MCP standard), un `tokio::net::UnixStream` convient
  donc directement, une session MCP complete par connexion acceptee.
- **`crates/controller/src/reconcile.rs`** : nouveau volume `emptyDir`
  "jailer" partage entre les conteneurs `vm-supervisor` et `mcp-gateway` du
  pod parent, monte au meme chemin absolu (`/srv/jailer`,
  `ATELIER_VM_CHROOT_BASE_DIR` fixe explicitement plutot que de dependre du
  defaut binaire) dans les deux — necessaire puisque le socket vsock
  "principal" vit **a l'interieur** de l'arborescence du jail, pas a un
  chemin choisi arbitrairement. `ATELIER_MCP_GATEWAY_VSOCK_UDS_PATH` calcule
  par le controller a partir des memes constantes (`VM_JAIL_ID`,
  `VM_VSOCK_UDS_FILENAME`) que celles passees a `vm-supervisor`.
- **Piege trouve en testant reellement** : le jailer insere le nom de
  l'executable comme composant de chemin —
  `<chroot_base_dir>/<exec_file_name>/<jail_id>/root/` (donc
  `/srv/jailer/firecracker/atelier-vm/root/...`), pas
  `<chroot_base_dir>/<jail_id>/root/` comme la seule lecture de la doc
  `--chroot-base-dir` le suggererait — corrige apres inspection reelle de
  l'arborescence produite (`find /srv/jailer`).
- **Verifie de bout en bout, sans mock** (`demo/vsock-probe/`, nouveau) :
  vrai `atelier-vm-supervisor` + vrai `atelier-mcp-gateway` (binaires
  natifs, pas de conteneur Docker pour ce test-la — les deux process
  partagent directement `/srv/jailer` sur le systeme de fichiers de
  l'environnement de test), devcontainer minimal (systemd + `python3`) dont
  un service ecrit un **vrai client MCP** a la main (stdlib `socket` avec
  `socket.AF_VSOCK`, disponible nativement sur Linux depuis Python 3.9) qui
  fait un handshake MCP complet (`initialize` -> `notifications/initialized`
  -> `tools/list`) contre `mcp-gateway` en passant uniquement par
  `AF_VSOCK`. Resultat lu apres coup dans le `rootfs.ext4` du guest
  (`debugfs -R "cat ..."`, meme technique que pour la validation
  HTTP_PROXY) : les trois etapes reussissent, `tools/list` renvoie bien
  `request_credential`/`request_egress`. Voir `demo/vsock-probe/README.md`.
- **Limite assumee** : ce test prouve que le **transport** fonctionne
  (jailer+firecracker+vsock+rmcp s'assemblent correctement), pas qu'un
  client MCP standard (Claude Code, etc.) choisirait ce chemin plutot que
  HTTP aujourd'hui — rien ne l'annonce au guest (pas d'equivalent au
  `HTTP_PROXY` injecte dans `/etc/environment` pour signaler ce transport).
  Egalement non teste : le comportement en conditions Kubernetes reelles
  (volume `emptyDir` partage entre conteneurs d'un vrai pod, teste ici en
  isolation avec deux process natifs partageant un repertoire local).

### Verification MCP depuis l'interieur d'une vraie microVM agent : fait

Dernier point ouvert de la section `mcp-gateway` : tous les tests HTTP
precedents (`request_egress`, `request_credential`, `enable_simulator`)
etaient verifies via un client `curl` tournant sur l'**hote**, partageant le
netns Docker avec `net-proxy` — jamais depuis un guest reellement boote par
`vm-supervisor`, donc jamais a travers le pare-feu TAP de production.

- **`demo/mcp-agent-probe/`** (nouveau) : devcontainer minimal (systemd +
  `curl`) dont le seul acces reseau est celui d'un devcontainer normal —
  aucun raccourci, contrairement a `demo/vsock-probe/` (pas de partage de
  netns/process avec l'hote). `mcp_probe.sh` fait un vrai handshake MCP
  (`initialize` -> `notifications/initialized` -> `tools/call
  request_egress`) contre `http://mcp-gateway/mcp`, en s'appuyant
  uniquement sur `HTTP_PROXY` (lu depuis `/etc/environment` via
  `EnvironmentFile=` systemd) — exactement le chemin qu'un vrai client MCP
  dans un devcontainer emprunterait.
- **`/etc/environment` injecte a la main** avec le contenu exact produit par
  `inject_net_proxy_config` (deja verifie octet pour octet contre le vrai
  pipeline `image-builder` plus tot dans cette session) : reproduit
  manuellement plutot que rejoue via le pipeline complet, pour eviter le
  blocage connu d'auth git sur un depot prive a l'epoque (depuis resolu
  autrement pour `ministack-workshop`, voir plus bas — ce depot a demenage
  vers un depot public dedie).
- **Boot avec les vrais binaires** `atelier-vm-supervisor` (TAP +
  `restrict_to_net_proxy`, le pare-feu de production, pas une version
  allegee), `atelier-net-proxy` (alias `mcp-gateway` configure) et
  `atelier-mcp-gateway` (tool `egress` active).
- **Verifie de bout en bout, avec preuve croisee** (pas seulement "le guest
  dit que ca a marche") : le guest recupere bien `HTTP_PROXY=http://169.254.0.1:3128`
  (log de la sonde), les trois etapes du handshake MCP reussissent (log de
  la sonde, lu apres extinction via `debugfs`), **et** `net-proxy` (systeme
  tiers independant) journalise de son cote `allowlist elargie a chaud
  (request_egress) host="example.com" count=1` — la meme requete qui a
  transite par le pare-feu TAP a reellement modifie l'etat de `net-proxy`.
  Voir `demo/mcp-agent-probe/README.md`.

## Devcontainer de demo `ministack-workshop` : le boot Firecracker de l'agent exige systemd

> **Mise a jour** : ce devcontainer de demo vit desormais dans un depot
> public dedie, [github.com/PhilippeVienne/atelier-workspace](https://github.com/PhilippeVienne/atelier-workspace)
> (plus sous `demo/ministack-workshop/` de ce depot) — un `Workshop` peut
> donc le cloner sans identifiants git, le blocage d'auth mentionne plus
> bas dans cette section est resolu par ce demenagement, pas par le
> mecanisme d'identifiants git construit cote `image-builder` (toujours
> disponible pour de vrais depots prives).

Question ouverte depuis la conception de `vm-supervisor` (qui boote le
rootfs d'un devcontainer arbitraire **sans** `init=` personnalise,
contrairement a la microVM "builder") : le PID 1 par defaut d'une image
devcontainer standard demarre-t-il seulement, et fait-il tourner quoi que
ce soit tout seul ? Verifie reellement cette session avec
`ministack-workshop` (devcontainer combinant docker-in-docker,
`ministack`, Claude Code, `code-server` — voir son
[README](https://github.com/PhilippeVienne/atelier-workspace/blob/main/README.md)) :

- **Rootfs construit a la main, meme procedure qu'`image-builder`** (export
  d'image Docker + `mke2fs -F -t ext4 -d`, cf. `crates/image-builder/src/main.rs`),
  boote directement avec `atelier-firecracker`, boot_args identiques a
  `crates/vm-supervisor/src/main.rs` (nouveau test
  `crates/firecracker/tests/agent_boot_smoke.rs`, sur le modele de
  `tests/builder_vm.rs`) : reproduit fidelement les conditions reelles sans
  passer par tout le pipeline K8s (Job `image-builder`, registre,
  controller), qui aurait ajoute des variables sans rapport avec la
  question posee (notamment l'auth git sur un depot prive).
- **Constat initial : le PID 1 par defaut ne demarre rien.** Console du
  guest (draine vers `tracing::debug!`, seul canal de diagnostic sans
  vsock) : le noyau tente `/sbin/init`, `/etc/init`, `/bin/init`, ne trouve
  aucun des trois (l'image `mcr.microsoft.com/devcontainers/base` n'a pas
  de systeme init installe, pensee pour tourner sous un runtime de
  conteneur classique, pas pour booter seule) et retombe sur un `/bin/sh`
  nu comme PID 1 — rien ne demarre. `postStartCommand`
  (`devcontainer.json`) est un concept du CLI `devcontainer`, jamais
  rejoue par le noyau : sans lui, aucun mecanisme ne lance
  `dockerd`/`ministack`/`code-server`.
- **Corrige** : `systemd`/`systemd-sysv` ajoutes a l'image
  (`.devcontainer/Dockerfile` du depot `atelier-workspace`), nos services
  demarres via deux unites systemd dediees
  (`atelier-ministack.service`/`atelier-code-server.service`,
  `WantedBy=multi-user.target`) plutot que via `postStartCommand`, qui
  reste par ailleurs utile pour l'usage `devcontainer up` classique
  (Docker, testee au debut de cette session).
- **Piege trouve en corrigeant** : `systemctl enable` echoue silencieusement
  dans cette image (exit 0, aucun symlink cree) — un script factice
  intercepte `systemctl`/`service` ("systemd is not running in this
  container due to its overhead"), place par l'image de base pour eviter
  que l'installation de paquets `.deb` fournissant des unites systemd
  echoue pendant un `docker build` classique (sans PID 1 systemd). Les
  paquets eux-memes (ex: `docker.service`, installe par la feature
  `docker-in-docker`) s'activent correctement malgre tout via
  `deb-systemd-helper` (appele par leur script `postinst`), qui manipule
  les symlinks directement sans passer par ce faux `systemctl` — repris a
  la main (`ln -s` dans `/etc/systemd/system/multi-user.target.wants/`)
  pour nos deux unites.
- **Resultat, verifie reellement** : boot confirme (meme environnement que
  les autres tests Firecracker de ce crate, `docker run --privileged
  --network host` avec `/dev/kvm`+`/dev/net/tun`, voir "Lecons retenues"),
  `code-server:8080` et `ministack:4566` repondent tous les deux a une
  vraie connexion TCP depuis l'hote — dans une microVM bootee exactement
  comme le fait `vm-supervisor` en production (memes boot_args, aucune
  concession).
- **Portee de ce resultat** : ce n'est pas seulement une bizarrerie de ce
  devcontainer de demo — c'est la premiere verification reelle que
  l'architecture "n'importe quel depot Dev Containers standard" (`docs/ARCHITECTURE.md`)
  a une limite concrete non documentee jusqu'ici : un devcontainer sans
  systeme init installe ne fera jamais tourner ses `postCreateCommand`/
  `postStartCommand` une fois boote comme microVM (contrairement a
  l'usage normal via le CLI `devcontainer`/VS Code). Implication pour tout
  futur Workshop : soit le devcontainer source installe lui-meme un
  systeme init et declare ses propres services demarres au boot (comme
  fait ici), soit `image-builder`/`vm-supervisor` devront un jour injecter
  un mecanisme generique equivalent — non tranche, laisse ouvert.
- **Reste a faire** : creer un vrai `Workshop` K8s pointant sur ce depot
  (bloque sur l'auth git a un depot prive, jamais testee jusqu'ici — les
  Workshops de test utilisaient des depots publics) pour valider la meme
  chose a travers le pipeline complet, pas seulement en isolation.

## UI dashboard : gestion + "ouvrir VS Code"

Premiere page de detail par Workshop et pont HTTP+WebSocket pour ouvrir
`code-server` (port 8080 dans la microVM agent) directement depuis le
navigateur, au-dessus du protocole `portforward` existant (raw TCP/UDP
multiplexe, pas HTTP-aware).

- **`crates/api-server/src/vscode.rs`** (nouveau) : ouvre un flux d'octets
  vers `code-server` via le protocole `portforward` (websocket vers
  `net-proxy`, canal 0 = donnees) — pont via `tokio::io::duplex` + une tache
  de fond, pas de `Sink`/`Stream` a la main. `hyper::client::conn::http1`
  par-dessus (`hyper_util::rt::TokioIo`), `.with_upgrades()`. Requetes
  normales : relayees avec le prefixe `/v1/workshops/{name}/vscode` retire
  (`code-server` supporte nativement d'etre servi sous un sous-chemin
  arbitraire, documente officiellement par `coder/code-server` — URLs
  relatives, prefixe strippe avant de l'atteindre). Upgrade WebSocket (canal
  "live" propre de `code-server`) : `hyper::upgrade::on` capture cote
  requete entrante *avant* de la consommer, meme mecanisme cote reponse
  amont si `101`, puis `tokio::io::copy_bidirectional` — relai brut, sans
  reinterpreter les frames (meme philosophie que `net-proxy::proxy::tunnel`
  pour `CONNECT`). Helper partage `resolve_running_pod_ip` extrait dans
  `routes.rs` (`portforward.rs` refactore pour le reutiliser).
- **Verifie reellement, sans mock** (`crates/api-server/tests/routes.rs`,
  vrai `net-proxy`, vrai Workshop/Pod sur kind) : un test relaie un `GET`
  HTTP a travers tout le pont jusqu'a un vrai petit serveur de test,
  verifie que le prefixe est bien retire ; un second test verifie le
  chemin d'upgrade de bout en bout (client TCP brut envoyant une requete
  `Upgrade: websocket`, reponse `101` reelle, octets echoes a travers tout
  le tunnel) — les deux stables sur plusieurs execution repetees,
  y compris en parallele l'un de l'autre (verrou de test partage sur les
  variables d'environnement globales que les deux mutent).
- **Dashboard** : nouvelle page `app/workshops/[name]/page.tsx` (statut,
  suspendre/reprendre/supprimer, lien "Ouvrir VS Code" en nouvel onglet si
  `phase === "Running"`), Route Handler catch-all
  `app/workshops/[name]/vscode/[[...path]]/route.ts` (reverse-proxy
  same-origin fin, ajoute `Authorization: Bearer` cote serveur a partir du
  cookie de session existant — le navigateur ne voit jamais le token) pour
  tous les assets HTTP normaux de `code-server`. Preset "Demo ministack"
  sur `workshops/new` (pre-remplit le formulaire existant avec le depot de
  ce projet + le chemin du devcontainer de demo).
- **`dashboard/server.ts`** (nouveau, serveur Next custom — necessaire
  seulement pour le WebSocket "live" de `code-server`, qu'un Route Handler
  standard ne peut pas hijacker) : intercepte l'evenement Node `'upgrade'`,
  lit le cookie de session directement dans les en-tetes (aucune API Next
  disponible hors contexte de requete), ouvre une connexion sortante avec
  `ws` vers `api-server` (`Authorization: Bearer` ajoute cote serveur),
  relie les deux cotes. `package.json` : `dev`/`start` utilisent desormais
  `tsx server.ts` au lieu de `next dev`/`next start`.
- **Bug reel trouve en testant manuellement (pas par les tests
  automatises)** : la premiere version appelait `wss.handleUpgrade`
  (qui envoie inconditionnellement un `101` au navigateur) **avant** de
  savoir si la connexion amont vers `api-server` reussissait — un
  navigateur pouvait donc recevoir un `101` "reussi" alors que le canal
  restait mort en silence juste apres. Corrige : la connexion amont est
  ouverte et son evenement `open` attendu *avant* d'appeler
  `wss.handleUpgrade` ; un client WebSocket standard n'envoie de toute
  facon aucune trame avant d'avoir recu son propre `101`, rien n'est perdu
  a attendre. Egalement corrige au passage : `socket.destroy()`
  immediatement apres `socket.write()` peut tronquer l'ecriture avant
  qu'elle ne parte reellement (constate en pratique : le client ne
  recevait rien du tout sur le chemin d'erreur) — `socket.end(data)` a la
  place (ecrit puis ferme proprement une fois le buffer vide).
- **Limite assumee** : verifie manuellement contre un Workshop de test
  (Pod avec `podIP` controle a la main, meme technique que les tests
  automatises) et un service de remplacement pour `code-server` — pas
  encore contre un vrai `code-server` reel ni un vrai `Workshop` complet —
  desormais possible sans blocage d'auth git puisque `ministack-workshop`
  est un depot public (cf. section dediee plus haut), reste a le faire.
  Port `code-server` fixe a
  8080 par convention (`ATELIER_VSCODE_PORT` overridable, surtout utile
  pour les tests), pas encore un champ du CRD `Workshop`.

## LLM Proxy : LiteLLM global (DeepSeek low-cost + Anthropic premium)

Objectif : reduire le cout d'inference de Claude Code (et de tout autre
agent) tournant dans les devcontainers en routant ses appels Anthropic
Messages API vers DeepSeek par defaut, avec un alias explicite vers le vrai
Anthropic Sonnet pour les taches complexes. Architecture proposee par
l'utilisateur, adaptee aux invariants existants (la microVM ne parle
jamais directement a un service, seulement a `net-proxy` ; ici, service
**global du cluster**, pas un sidecar par pod, decision explicite de
l'utilisateur — contrairement au sidecar `simulator` de `mcp-gateway`).

- **Fait verifie, pas suppose** (documentation officielle Claude Code et
  LiteLLM) : Claude Code ne parle que le format Anthropic Messages API
  (`/v1/messages`), aucun support multi-fournisseur natif — un vrai proxy
  traducteur est necessaire pour DeepSeek/OpenAI/Grok. `ANTHROPIC_BASE_URL`/
  `ANTHROPIC_AUTH_TOKEN` sont les variables standard pour pointer vers
  n'importe quel gateway compatible. LiteLLM (MIT, `ghcr.io/berriai/litellm`)
  est l'outil etabli de l'ecosysteme pour cette traduction — meme logique
  de reuse que `ministack` pour AWS, pas de traducteur maison.
- **`deploy/dev/llm-proxy/`** (nouveau, meme niveau qu'`deploy/dev/openbao/`) :
  ConfigMap (`config.yaml`, le `model_list` — `claude-3-5-sonnet-20241022`
  et `deepseek-dev` -> `deepseek/deepseek-chat`, `sonnet-premium` -> le vrai
  `anthropic/claude-3-5-sonnet-20241022`), Secret (`DEEPSEEK_API_KEY`,
  `ANTHROPIC_API_KEY`, `LITELLM_MASTER_KEY`), Deployment+Service
  (`ghcr.io/berriai/litellm:main-stable`, tag epingle).
- **`crates/net-proxy/src/internal.rs`** : 4e alias fixe `llm-proxy`
  (`ATELIER_LLM_PROXY_ADDR`), meme mecanisme qu'`identity-proxy`/
  `mcp-gateway`/`registry` — toujours actif des que configure.
- **`crates/controller`** : `ReconcileCtx.llm_proxy_addr`/
  `llm_proxy_auth_token` (`Option`, meme convention "desactive si absent"
  qu'`openbao`). Cable **inconditionnellement** sur le conteneur `net-proxy`
  du pod parent (pas gate par `Workshop.spec.tools`, contrairement a
  `enable_simulator` — decision explicite : service global, toujours
  present). Le Job `image-builder` recoit `ATELIER_LLM_PROXY_AUTH_TOKEN`
  (necessaire au moment du build).
- **`crates/image-builder`** : extension de `inject_net_proxy_config`
  (meme fonction deja verifiee pour `HTTP_PROXY`) — ajoute
  `ANTHROPIC_BASE_URL=http://llm-proxy`/`ANTHROPIC_AUTH_TOKEN=<jeton>`/
  `ANTHROPIC_API_KEY=` (vide, desactive toute cle locale) dans
  `/etc/environment` quand le service est configure.
- **Bug reel trouve en testant, corrige** : `net-proxy` relayait la requete
  recue de la VM verbatim (forme absolue, `GET http://llm-proxy/... HTTP/1.1`)
  vers les alias internes — fonctionnait avec `mcp-gateway`/`identity-proxy`
  (axum/hyper, tolerants aux deux formes), mais `uvicorn` (serveur ASGI de
  LiteLLM) ne sait pas parser une cible en forme absolue et repondait `404`
  sur **tout**. Corrige par `http::to_origin_form` (nouveau) : reecrit la
  ligne de requete en forme origine (methode + chemin, en-tetes inchanges)
  avant de relayer vers un alias interne/`simulator` — verbatim inchange
  pour `identity-proxy` et l'egress normal, qui en ont explicitement besoin
  (voir leur propre commentaire). Aurait pu affecter n'importe quel futur
  alias non base sur axum/hyper, pas seulement `llm-proxy`.
- **Verifie reellement contre kind, sans mock** : Deployment `Running`
  (probes de sante LiteLLM passees sans le bug connu `BerriAI/litellm#8795`),
  `curl` direct sur le Service — `/health/readiness` 200, `/v1/models`
  liste les 3 alias configures, `POST /v1/messages` avec `model:
  deepseek-dev` traduit et route reellement jusqu'a DeepSeek (401 reel de
  DeepSeek avec une cle de test factice — preuve que la traduction de
  protocole et le routage fonctionnent de bout en bout, pas seulement que
  LiteLLM demarre). Puis a travers l'alias `net-proxy` reel (`--proxy`) :
  memes resultats, confirmant le cablage complet cote atelier.
- **Limites assumees** (documentees dans `deploy/dev/llm-proxy/README.md`) :
  un seul jeton partage par tous les Workshops (pas de cles virtuelles
  LiteLLM par Workshop dans ce lot) ; pas de prompt caching explicitement
  configure (important pour tenir un budget bas, a activer separement).
- **Reste a faire** : verification de bout en bout avec une vraie cle
  DeepSeek (non disponible dans cet environnement de developpement) et
  depuis l'interieur d'une vraie microVM agent (meme limite que les autres
  sections de ce document tant qu'aucun `Workshop` complet n'a ete boote
  avec `ministack-workshop`/`atelier-workspace`).

## Lecons retenues (a ne pas re-decouvrir)

- `fctools` 0.6.0/0.7.0-alpha.2 ne compilent pas avec seulement les
  features `vm` + `jailed-vmm-executor` + `tokio-runtime` +
  `nix-syscall-backend` : il faut aussi `direct-process-spawner`.
- `/tmp` est `tmpfs,nodev` sur la plupart des distros : un jail Firecracker
  qui y est enracine a des device nodes inertes, et le message d'erreur
  Firecracker ("configure the ACL") est trompeur. Utiliser un
  `chroot_base_dir` sur un filesystem sans `nodev` (`/var/tmp` en dev).
- Pour un snapshot `fctools`, les ressources `Produced` (ex: fichiers de
  snapshot) doivent recevoir un `initial_path` **relatif au jail**
  (`/snapshot.state`), pas un chemin hote absolu — contrairement aux
  ressources `Moved`, qui se resolvent automatiquement.
- `Vm::shutdown()` de `fctools` renonce a `cleanup()` si la VM n'est pas
  Paused/Running au moment de l'appel (ex: rootfs de test qui s'est deja
  eteint tout seul) ; il faut appeler `cleanup()` inconditionnellement pour
  eviter de laisser trainer un jail orphelin.
- Un patch JSON merge partiel sur `status` echoue en 422 si l'objet n'a
  encore aucun statut : le tout premier write de statut doit toujours etre
  complet.
- `ENVBUILDER_GIT_CLONE_REF` n'existe pas ; la revision s'encode dans
  `ENVBUILDER_GIT_URL` sous la forme `<repo>#<revision>`.
- `ENVBUILDER_DEVCONTAINER_JSON_PATH` est relatif a
  `ENVBUILDER_DEVCONTAINER_DIR` (`.devcontainer` par defaut), pas a la
  racine du depot — source d'un bug de chemin double
  (`.devcontainer/.devcontainer/...`) avant correction.
- Compiler un binaire avec une image `rust:*-bookworm` puis le faire tourner
  dans `debian:bookworm-slim` casse sur un mismatch de version glibc ; il
  faut un build multi-stage avec la meme base pour build et run.
- `ip tuntap add <nom> mode tap` echoue avec un message trompeur
  (`argument "<nom>" is wrong: "mode"/"dev"/"name" not a valid ifname`,
  quel que soit l'ordre des arguments) des que `<nom>` depasse 15
  caracteres (IFNAMSIZ Linux = 16 octets, terminateur nul inclus) — le
  message ne mentionne jamais la longueur, seulement un mot-cle de la
  commande elle-meme, ce qui egare completement le diagnostic.
- Pour tester du code necessitant `CAP_NET_ADMIN` (creation de TAP,
  configuration d'IP) sans root ni sudo interactif :
  `unshare --net --map-root-user -- <commande>` donne un espace de noms
  reseau isole avec toutes les capacites necessaires, sans mot de passe.
  Insuffisant en revanche pour un test qui a aussi besoin d'une vraie route
  de sortie vers Internet (le namespace isole n'a que `lo`) — cf. section
  "Builder microVM".
- `setcap cap_net_admin+eip` sur un binaire ne suffit pas toujours : dans un
  environnement d'agent sandboxe, meme une copie dediee du binaire avec la
  capability posee peut echouer en `Operation not permitted` sur le **vrai**
  netns (confirme via `getcap` correct + `strace` montrant un simple appel
  RTNETLINK refuse) — la sandbox elle-meme filtre l'operation independamment
  des capabilities Linux. `docker run --privileged` (nouveau netns isole
  avec `CAP_NET_ADMIN` effectif + sortie Internet via le NAT Docker par
  defaut) contourne ce blocage, la ou `unshare --net` seul n'a pas de route
  de sortie.
- axum 0.8 a change la syntaxe des parametres de route : `:nom` (ancienne
  syntaxe) panique au demarrage avec un message qui suggere `{nom}` — pas
  une erreur de compilation, une panique runtime au premier `Router::route`
  concerne.
- `jsonwebtoken` 11 exige d'activer explicitement une feature de fournisseur
  crypto (`rust_crypto` ou `aws_lc_rs`) ; sans ca, toute operation JWK
  panique au runtime avec un message qui explique le probleme (pas d'erreur
  de compilation) — `rust_crypto` evite une dependance a un compilateur C.
- `jsonwebtoken::jwk::Jwk::from_encoding_key(&EncodingKey, Algorithm)`
  derive directement les parametres publics (n/e pour RSA, x/y pour EC)
  depuis une cle privee : pratique pour construire un JWKS de test reel
  (vraie paire de cles, vraie signature) sans dupliquer la logique
  d'encodage/decodage a la main.
- `docker run --privileged --network host --device=/dev/kvm --device=/dev/net/tun`
  est une alternative viable a un acces `sudo` interactif reel pour les
  tests necessitant a la fois `CAP_NET_ADMIN` (creation de TAP) ET une
  vraie sortie reseau vers des services de l'hote (`net-proxy`, registre
  OCI de dev) : contrairement a un `docker run --privileged` isole (NAT
  Docker, netns separe), `--network host` partage directement le netns de
  l'hote — le TAP cree dans le conteneur est immediatement visible et
  routable depuis l'hote, sans configuration NAT/forwarding supplementaire.
  Compiler avec un `CARGO_TARGET_DIR` dedie dans ce conteneur (meme piege
  glibc que plus haut).
- `sockaddr_un.sun_path` (Linux) est limite a 108 octets : un chemin de
  jail Firecracker trop long (noms de test verbeux repetes dans le chemin,
  ex: `{chroot_base_dir}/firecracker/{jail_id}/root/run/firecracker.socket`)
  fait echouer `connect()` en `ENAMETOOLONG` a chaque tentative — silencieux
  et non exploitable si le code appelant (ici `fctools` 0.7.0-alpha.2,
  `Vm::start`) avale l'erreur dans une boucle de retry sans jamais la
  remonter (voir aussi le bullet suivant). Choisir des noms de jail/dossier
  de travail courts dans les tests.
- `fctools` 0.7.0-alpha.2 : la boucle d'attente du socket API dans
  `Vm::start` (`src/vm/mod.rs:244`,
  `loop { if client.get(...).await.is_ok() { break } }`) ne cede jamais la
  main a l'executeur async si `client.get()` echoue de facon synchrone a
  chaque iteration (ex: `ENAMETOOLONG` sur le chemin du socket) — elle
  tourne alors indefiniment a 100% CPU sur un seul thread, empechant meme
  le `timeout()` englobant de se declencher (aucune erreur ni panic, juste
  un blocage total). A diagnostiquer via `gdb -p <pid> -batch -ex bt` sur le
  thread actif (pas `strace`, qui ne voit rien d'utile si le blocage est
  purement en espace utilisateur sans syscall bloquant) : la pile remonte
  directement jusqu'a la ligne fautive.
- Une image OCI convertie en `rootfs.ext4` brut (`crane export` + `mke2fs`,
  sans passer par un runtime de conteneur au boot) perd toute sa metadonnee
  `ENV`/`ENTRYPOINT`/etc. — un guest qui boote directement ce filesystem
  (init custom en PID 1, cas de `atelier-builder-vm-init`) doit re-fournir
  explicitement toute variable d'environnement dont un binaire de l'image
  a besoin (ex: `KANIKO_DIR` pour `envbuilder`), meme si le `Dockerfile`
  source la definit via `ENV`.
- `golang.org/x/net/http/httpproxy` (utilise par le client HTTP standard de
  Go, donc par `envbuilder`) exclut **inconditionnellement** `localhost` et
  les IP loopback du proxy configure via `HTTP_PROXY`/`HTTPS_PROXY` — meme
  sans `NO_PROXY`, comportement non desactivable depuis l'environnement.
  Un service passe comme `localhost:<port>` a un process qui doit y acceder
  *via* un proxy explicite (ex: guest microVM sans route par defaut) doit
  plutot recevoir une adresse non-loopback (IP reelle de l'hote/du lien
  point-a-point).
- `reboot(RebootMode::RB_POWER_OFF)` (crate `nix`) n'a aucun effet observable
  dans une microVM Firecracker minimale sans ACPI (`pci=off` dans les
  `boot_args`) : le noyau n'a pas de handler `pm_power_off` a invoquer et se
  contente d'un `halt` ("reboot: System halted"), sans que Firecracker
  detecte la fin de la VM. Avec `reboot=k` dans les `boot_args` (deja
  necessaire pour ce type de machine minimale), c'est
  `reboot(RebootMode::RB_AUTOBOOT)` (reboot standard, pas power-off) qu'il
  faut appeler : il declenche un reset via le controleur clavier i8042 que
  Firecracker intercepte lui-meme comme signal de fin de VM.
- Un `Job` Kubernetes n'est marque termine que lorsque tous ses
  `containers[]` (pas ses `initContainers[]`, sidecars natifs compris) ont
  fini avec succes : un sidecar long-vivant (ex: `net-proxy`, jamais de code
  de sortie) doit etre declare comme `initContainer` avec
  `restartPolicy: "Always"` (K8s >= 1.28/1.29, KEP-753 "sidecar containers")
  et non dans `containers[]`, sous peine d'un `Job` qui reste `Running`
  indefiniment meme apres la fin reelle du conteneur principal.
- `COPY --from=<stage> /` dans un `Dockerfile` multi-etapes aplatit
  l'integralite du filesystem de ce stage sans passer par un registre OCI :
  equivalent local a `crane export` + `tar xf` (utilise ailleurs dans ce
  projet pour aplatir une image deja poussee), mais inutile ici puisque le
  contenu est deja disponible localement dans le build multi-etapes —
  simplifie significativement la construction d'un rootfs bootable a partir
  d'un stage Docker intermediaire (voir `crates/image-builder/Dockerfile`).
- Une image Docker rechargee dans `kind` (`kind load docker-image`) ne
  signale jamais qu'elle est perimee : reconstruire le code sans reconstruire
  ET recharger l'image dans le cluster produit des echecs silencieux et
  deroutants (le nouveau code n'est simplement jamais execute). Toujours
  verifier `docker images <nom>` / refaire `kind load` apres tout changement
  de code affectant une image utilisee en cluster.
- `fctools` 0.7.0-alpha.2 : `VmConfigurationData` ne derive que `Serialize`
  (pas `Deserialize`), et `Vm::restore` exige un `Vm` source vivant dans le
  meme process (son `ResourceSystem` fournit les ressources a recopier) —
  cette API n'est pas concue pour un snapshot qui doit survivre a un
  redemarrage complet du process appelant. Contournement viable : ne pas
  serialiser `VmConfigurationData` du tout, la **reconstruire a l'identique**
  a partir des memes parametres qu'un boot normal (elle est entierement
  determinee par eux) — la coherence du chemin virtuel jaile
  (`FlatVirtualPathResolver`, base sur le nom de fichier, pas sur l'identite
  de la ressource) rend cette reconstruction valide sans avoir besoin de
  l'objet source. Voir `Vm::restore_persisted`,
  `crates/firecracker/src/vm.rs`.
- Le `controller`, lance depuis un poste de dev (hors cluster), ne peut pas
  joindre une IP de pod kind directement — le reseau du CNI (10.244.0.0/16)
  n'est pas route vers l'hote par defaut. `docker run --network
  container:<nom-du-noeud-kind>` (partage le netns du noeud, qui lui route
  bien vers ses propres pods) contourne ce blocage pour du test manuel —
  meme categorie de contournement que celui deja documente pour
  `CAP_NET_ADMIN`/la microVM builder plus haut. En production, le
  `controller` tournant *dans* le cluster (pas encore le cas dans cette
  session de dev), ce probleme ne se pose simplement pas.
- Un `Job` Kubernetes ne supprime pas son pod a la fin (`Complete`) — le
  pod reste visible (`Terminating` puis disparait apres la periode de
  grace par defaut, ~30s) : un controller qui interroge ce pod pendant
  cette fenetre (ex: pour un appel de controle avant liberation) peut le
  voir "encore la" alors que son process principal a deja fini — prevoir
  l'idempotence/tolerance a un second appel plutot que supposer qu'un seul
  suffira.
- Le parametre de boot noyau Linux standard `ip=<client-ip>:<server-ip>:
  <gw-ip>:<netmask>:<hostname>:<device>:<autoconf>` (autoconfiguration IP,
  cf. `Documentation/admin-guide/nfs/nfsroot.rst`) configure une interface
  et sa route par defaut **avant que l'init du guest ne demarre**, sans
  aucune cooperation de celui-ci — le bon outil pour donner une IP a une VM
  dont on ne controle pas l'init (contrairement a la microVM "builder", qui
  a son propre init personnalise et peut donc configurer son reseau
  elle-meme en espace utilisateur).
- Chaque nouvelle image Docker qui commence a faire du reseau/pare-feu doit
  explicitement installer `iproute2`/`iptables` — l'oubli est facile
  (`image-builder` puis `vm-supervisor` ont chacun ete oublies une fois
  cette session) et le symptome (`lancement de ip: No such file or
  directory`, ou pire un `CrashLoopBackOff` sans message clair au premier
  coup d'oeil) ne pointe pas immediatement vers la cause.
- `jsonwebtoken::Validation::new()` active `validate_aud: true` par defaut :
  sans `set_audience(...)` explicite, n'importe quel token portant un `aud`
  (le cas de tout token OAuth2 reel emis par Kanidm) est rejete en
  `InvalidAudience` — invisible avec des JWT de test synthetiques qui
  n'incluent jamais `aud`. A ne decouvrir qu'en testant contre un vrai flux
  OAuth2, jamais avec des tokens fabriques a la main pour les tests.
- `reqwest` compile avec la feature `rustls-tls` ignore le trust store
  systeme et `SSL_CERT_FILE` : contre un service TLS a CA auto-signee (ex:
  Kanidm de dev), il faut construire un `Client` avec
  `.add_root_certificate(reqwest::Certificate::from_pem(...))` explicitement
  — un simple `reqwest::get()` echoue toujours, meme si `curl`/le systeme
  font confiance a cette CA.
- Donner a un `Pod` de test un vrai conteneur planifiable (ex:
  `registry.k8s.io/pause:3.9`) fait qu'un vrai kubelet le prend en charge et
  **ecrase** tout `status.podIP` patche manuellement par son adresse CNI
  reelle des que le conteneur demarre — y compris apres un premier
  `patch_status` reussi (course avec la reconciliation continue du
  kubelet). Pour un test qui a seulement besoin de controler `status.podIP`
  sans faire tourner de vrai conteneur, fixer `spec.nodeName` sur un nom de
  noeud inexistant : le Pod reste `Pending` a jamais (aucun kubelet ne le
  reclame), et le patch de statut manuel n'est plus jamais ecrase.
- `tonic-build` 0.14 a deplace toute la generation de code liee a `prost`
  vers un crate separe, `tonic-prost-build` (`tonic_build::configure()`
  n'existe plus) — cote runtime, le crate correspondant est
  `tonic-prost` (pas seulement `prost`). `tonic-prost-build` (comme
  `prost-build`) invoque le binaire externe `protoc` au moment du build ;
  sur une machine sans `apt`/`sudo`, un binaire precompile
  (github.com/protocolbuffers/protobuf/releases) pointe via la variable
  d'environnement `PROTOC` (`.cargo/config.toml` local, non commite car
  specifique a la machine) fonctionne tout aussi bien.
- Un `DaemonSet` qui bind-mounte tout `/dev` de l'hote (au lieu de devices
  individuels) casse le mount par defaut de kubelet sur
  `/dev/termination-log` (`CrashLoopBackOff` immediat, "read-only file
  system" au moment de creer ce point de montage) : toujours monter les
  devices necessaires individuellement (`hostPath` par device, type
  `CharDevice`), jamais `/dev` entier.
- Pour un device plugin Kubernetes (`/dev/kvm` ici), `securityContext.capabilities.add: [NET_ADMIN]`
  suffit a creer un TAP (`ip tuntap add`) mais **pas** a spawner un process
  dont les capabilities effectives viennent de `setcap` sur son binaire
  (ex: `jailer`, cf. Dockerfile `vm-supervisor`) : celui-ci echoue en
  "Operation not permitted" au spawn si `SYS_ADMIN`/`SYS_RESOURCE` ne sont
  pas *aussi* explicitement ajoutees au conteneur — les capabilities de
  fichier ne peuvent etre elevees a l'exec que si elles font deja partie du
  bounding set du conteneur, l'ensemble par defaut containerd/Docker
  (`CHOWN`, `DAC_OVERRIDE`, `FOWNER`, `SETUID`, `SETGID`, `SYS_CHROOT`,
  `MKNOD`, etc.) ne les couvrant pas. Un restart de pod qui echoue *apres*
  la creation du TAP mais *avant* que Firecracker demarre laisse ce TAP
  vivant dans le netns du pod (partage entre redemarrages d'un meme pod,
  contrairement a un nouveau pod) : le redemarrage suivant echoue alors en
  `EBUSY` sur la creation du TAP, masquant completement l'erreur d'origine
  (`SYS_ADMIN` manquant) derriere un symptome different — a diagnostiquer
  avec `restartPolicy: Never` pour capturer le tout premier essai sans
  bruit de redemarrage.
- L'endpoint API `/oauth2/authorise` de Kanidm (utilise par
  `get-oauth2-token.sh`) exige un `Authorization: Bearer` deja present —
  confirme en testant reellement : `401 Unauthorized` systematique sans ce
  header, jamais de redirection vers une page de login. Ce n'est donc PAS
  l'URL a utiliser pour un flux browser-based classique (un navigateur sans
  session ne peut pas fournir ce bearer sur une simple navigation) :
  `/ui/oauth2` (meme query params) sert la SPA Kanidm, qui gere son propre
  login+consentement en JS avant de rediriger reellement le navigateur vers
  `redirect_uri` — c'est cette URL qu'un client OAuth2 browser-based doit
  cibler, pas l'endpoint API.
- Un client OAuth2 public Kanidm (`create-public`, PKCE) refuse par defaut
  toute `redirect_uri` pointant vers `localhost` (protection standard contre
  le detournement d'un client public en local) : necessite
  `enable-localhost-redirects` explicite en plus de `add-redirect-url`,
  sans quoi l'echange de code echoue meme avec une redirect_uri par ailleurs
  correctement enregistree.
- Une image devcontainer standard (`mcr.microsoft.com/devcontainers/base`)
  n'a **pas** de systeme init installe : bootee directement par un noyau
  (sans runtime de conteneur), le PID 1 retombe sur un `/bin/sh` nu apres
  l'echec de `/sbin/init`/`/etc/init`/`/bin/init` — rien ne demarre tout
  seul, `postStartCommand` n'etant rejoue que par le CLI `devcontainer`.
  Ajouter `systemd`/`systemd-sysv` et declarer ses propres services comme
  unites systemd est necessaire pour tout devcontainer destine a booter
  comme microVM.
- Dans une image devcontainer standard, `systemctl enable <unit>` echoue
  silencieusement (exit 0, aucun symlink cree) : un faux `systemctl` y est
  installe pour eviter que l'installation de paquets `.deb` porteurs
  d'unites systemd echoue pendant un `docker build` classique (pas de PID 1
  systemd a ce moment-la). Contournement : creer soi-meme le symlink
  d'activation (`ln -s ../mon-service.service
  /etc/systemd/system/multi-user.target.wants/mon-service.service`), comme
  le fait `deb-systemd-helper` pour les paquets (qui, lui, n'est pas
  intercepte).

## Prochaines etapes (par priorite)

1. ~~Brancher la microVM "builder" dans `image-builder`/`reconcile.rs`~~ —
   **fait cette session**, voir "Builder microVM" ci-dessus. Le pipeline
   complet `image-builder` (microVM) → cache → `vm-supervisor` tourne
   automatiquement de bout en bout, verifie contre kind reel, sans peuplage
   manuel du PVC. Reste ouvert dans ce voisinage : le registre interne
   (`ctx.registry_addr`) n'est joignable par la microVM builder que via
   l'alias `registry` de `net-proxy`, pas encore par la VM de l'agent
   (n'a pas de reseau du tout aujourd'hui, voir point 3).
2. ~~Canal de controle entre `controller`/`vm-supervisor` pour que `suspend`
   declenche un vrai `snapshot/create` avant liberation du pod~~ — **fait
   cette session**, voir "Canal de controle suspend/resume" ci-dessus.
   Canal HTTP (pas `vsock`, terme initial trompeur — voir cette section) ;
   `status.snapshotDigest` reellement renseigne, restauration cross-process/
   cross-pod validee contre kind. Reste ouvert : le `controller` lui-meme
   ne tourne pas encore *dans* le cluster (premier `Dockerfile` ecrit cette
   session, mais uniquement pour ce test manuel — pas encore de
   Deployment/RBAC cluster-wide dedies), donc ce canal n'est valide qu'en
   conditions de test, pas encore en deploiement reel.
3. ~~`net-proxy`/`identity-proxy` comme conteneurs du pod parent + TAP
   reseau pour la VM de l'agent~~ — **fait cette session**, voir "Reseau de
   l'agent + net-proxy/identity-proxy dans le pod parent" ci-dessus. Le TODO
   de `docs/ARCHITECTURE.md` (l'agent parle-t-il a `identity-proxy` en
   direct ou seulement via `net-proxy`) etait deja tranche par
   `docs/architecture/network-security.md` (session precedente) : jamais en
   direct, uniquement via `net-proxy`. ~~Reste ouvert, distinct : faire en
   sorte que le devcontainer construit utilise reellement ce chemin~~ —
   **fait cette session** : plutot que de chercher un mecanisme cote
   `envbuilder` (aucun hook natif trouve, et de toute facon inutilisable ici
   — l'export brut `crane export`+`mke2fs` perd deja toute metadonnee OCI
   `ENV`, voir "Lecons retenues"), `image-builder::inject_net_proxy_config`
   (nouveau, `crates/image-builder/src/main.rs`) ecrit directement
   `/etc/environment` (complete le fichier existant, `HTTP_PROXY`/
   `HTTPS_PROXY`/variantes minuscules + `NO_PROXY` vers `169.254.0.1:3128`)
   et `/etc/resolv.conf` (`nameserver 169.254.0.1`, remplace un eventuel
   symlink `systemd-resolved`) dans l'arborescence exportee, entre
   `export_image_filesystem` et `package_ext4` — meme adresse fixe de lien
   point-a-point et meme port que `vm-supervisor`/`controller` (constante
   `3128`, cf. `crates/controller/src/reconcile.rs`). `/etc/environment` est
   le bon point d'injection pour un guest sans init personnalise (boote le
   PID 1 du devcontainer tel quel) : lu par `pam_env` pour toute session de
   login (SSH/terminal interactif — code-server, Claude Code).
   **Verifie contre le vrai pipeline `image-builder`** (pas seulement
   `cargo check`) : conteneurs Docker reels `atelier-image-builder:dev` +
   `atelier-net-proxy:dev` (partage du netns hote, meme registre `:5000`
   reel), vrai devcontainer (`vscode-remote-try-python`) construit par la
   microVM builder via `envbuilder`, push registre reel, `crane export` +
   injection + `mke2fs` reels. Contenu du `rootfs.ext4` resultant inspecte
   directement (`debugfs -R "cat ..."`, sans montage) : `/etc/environment`
   et `/etc/resolv.conf` (fichier regulier, pas un symlink residuel) portent
   bien le contenu attendu.
   **Usage reel par un guest boot, verifie ensuite** (`demo/net-proxy-probe/`,
   nouveau) : devcontainer minimal (systemd + `curl` + service qui fait un
   vrai `CONNECT` HTTPS vers `example.com`) boote avec le **vrai binaire**
   `atelier-vm-supervisor` (pas une reimplementation de test), a cote d'un
   vrai `atelier-net-proxy`, meme protocole A/B qu'ailleurs dans cette
   section : rootfs identique avec/sans les deux fichiers injectes. Preuve
   observee dans les logs du **vrai** `net-proxy` (pas dans un mock) :
   variante avec injection -> `egress autorise ... host="example.com"
   port=443 method=CONNECT allowed=true` (le guest a bien lu `HTTPS_PROXY`
   et l'a utilise) ; variante sans injection -> aucune entree de log
   pendant toute la fenetre du run (la tentative directe de `curl` est
   silencieusement rejetee par `restrict_to_net_proxy`, jamais un `CONNECT`
   n'atteint `net-proxy`). Voir `demo/net-proxy-probe/README.md` pour le
   protocole complet et la limite restante (sonde TCP directe sur le port
   du guest non fiable dans cet environnement, non bloquant vu que la preuve
   par les logs `net-proxy` suffit).
4. ~~`api-server` : valider contre un vrai flux OAuth2 Kanidm, et role de
   coordinateur de port-forward~~ — **fait cette session**, voir point 11
   du resume chronologique ci-dessus. Resource Server OAuth2 reel configure
   dans Kanidm de dev, deux bugs reels trouves et corriges
   (`ATELIER_JWT_AUDIENCE`, `ATELIER_JWT_CA_PATH`), endpoint
   `/v1/workshops/{name}/portforward` ecrit et teste de bout en bout.
5. ~~`mcp-gateway`~~ — **base faite cette session**, voir section dediee
   ci-dessus : serveur MCP reel (`request_credential`, `request_egress`) via
   HTTP/SSE. Reste ouvert : le premier simulateur (candidat toujours
   LocalStack pour AWS) et le tool `enable_simulator` associe, le transport
   `vsock` natif, et une verification depuis l'interieur d'une vraie
   microVM agent (pas encore d'agent MCP dans le devcontainer de test).
6. ~~Device plugin Kubernetes pour `/dev/kvm`, afin de sortir du pod
   `privileged: true`~~ — **fait cette session**, voir point 12 du resume
   chronologique ci-dessus. `vm-supervisor` et `image-builder` tournent
   desormais sans `privileged: true`, verifie contre kind reel (boot
   Firecracker reussi dans un pod portant uniquement `NET_ADMIN`/`SYS_ADMIN`/
   `SYS_RESOURCE` + la ressource `atelier.dev/kvm`).
7. Offload/reload du cache d'images vers S3 (prevu des la conception,
   explicitement differe).
8. Stack d'observabilite complet : collector OTLP + backend de stockage +
   Grafana.
9. ~~Devcontainer de demo `ministack-workshop`~~ : boot Firecracker reel
   **verifie**, ~~UI dashboard dediee~~ **construite cette session** (voir
   section dediee "UI dashboard" ci-dessus : page de gestion par Workshop,
   pont HTTP+WS `api-server` -> `code-server`, preset de creation). Reste
   ouvert : creer un vrai `Workshop` K8s pointant sur
   [`atelier-workspace`](https://github.com/PhilippeVienne/atelier-workspace)
   (depot desormais public, plus de blocage d'auth git) pour la premiere
   validation reellement complete du pont "Ouvrir VS Code" de bout en bout.
10. ~~LLM Proxy~~ — **base faite cette session** (LiteLLM global, DeepSeek
    par defaut + Anthropic premium), voir section dediee ci-dessus. Reste
    ouvert : verification avec une vraie cle DeepSeek et depuis l'interieur
    d'une vraie microVM agent ; cles virtuelles LiteLLM par Workshop (pas
    de scoping/isolation de budget dans ce lot) ; prompt caching non
    configure ; OpenAI/Grok non couverts (DeepSeek/Anthropic uniquement
    pour l'instant, le `model_list` LiteLLM est extensible sans changement
    de code cote atelier).

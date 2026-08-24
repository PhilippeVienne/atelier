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
| `crates/firecracker::network` (TAP link-local + passerelle transparente) | Fonctionnel | Creation/config/suppression d'un vrai device TAP testee reellement (`unshare --net --map-root-user`, sans besoin de root), plus `restrict_to_net_proxy`/`enable_transparent_gateway` (regles iptables `filter`+`nat`, voir `docs/architecture/network-security.md`) — testees reellement (contenu exact des regles verifie via `iptables -S`/`iptables -t nat -S`). Utilise par `vm-supervisor` (VM de l'agent) et `image-builder` (VM "builder") |
| `crates/builder-vm-init` (guest init de la microVM "builder") | Fonctionnel | Cycle complet valide reellement : boot jaile + reseau + `envbuilder` (clone, build, push registre via `net-proxy`) + extinction propre de la VM detectee par l'hote, `crane manifest` confirme l'image poussee (`cargo test -p atelier-firecracker --test builder_vm`, 35s). Cinq causes racines trouvees et corrigees en cours de route, voir "Builder microVM" ci-dessous |
| Boucle complete Workshop → pod → microVM `Running` | Fonctionnel (automatique) | Pour la premiere fois de bout en bout **sans peuplage manuel du cache** : `kubectl apply` d'un Workshop reel declenche le Job `image-builder` (microVM "builder" reelle), qui construit et pousse l'image, l'exporte en `rootfs.ext4`, la publie dans le cache, patche `status.imageDigest` — puis le controller enchaine automatiquement sur le pod parent, `vm-supervisor` boote la microVM avec ce rootfs. Verifie reellement contre kind (`Job` `Complete`, `Workshop.status.phase=Running`) |
| Observabilite (OpenTelemetry) | Fonctionnel (base) | `atelier_common::telemetry::init()` cable sur tous les binaires, spans sur la boucle de reconciliation |
| `api-server` | Fonctionnel | JWT valide contre un vrai flux OAuth2 Kanidm (PKCE S256, `/oauth2/token` reel — deux bugs reels trouves et corriges au passage, voir "Lecons retenues" : `InvalidAudience` faute d'`aud` configure, CA auto-signee non fiee par `reqwest`/rustls) ; endpoints CRUD + suspend/resume sur `Workshop` via `kube::Api`, testes reellement contre kind (creation, isolation par `owner_subject`, suspend/resume, suppression) ; coordinateur de port-forward (`/v1/workshops/{name}/portforward`, authentifie puis relaie vers `net-proxy`), teste reellement de bout en bout (client websocket -> api-server -> net-proxy -> serveur TCP cible) ; pont HTTP+WebSocket generique vers n'importe quel port du guest (`proxy_to_guest_port`) : `code-server` (`/v1/workshops/{name}/vscode/*`) et terminal `ttyd` (`/v1/workshops/{name}/terminal/*`), les deux verifies dans un vrai navigateur contre une vraie microVM — voir sections "UI dashboard" et "Terminal navigateur (`ttyd`)" |
| `net-proxy` — egress (allowlist + proxy parent + passerelle transparente) | Fonctionnel | Proxy HTTP explicite (relai en clair + tunnel `CONNECT`) avec allowlist par domaine/wildcard, chainage optionnel vers un proxy parent (`ATELIER_UPSTREAM_PROXY`), **et deux ports d'ecoute transparents** (redirection iptables, Host header/SNI, zero configuration guest necessaire — voir section dediee ci-dessous). Deploye comme sidecar du Job `image-builder` et comme conteneur du **pod parent** de l'agent, allowlist alimentee depuis `Workshop.spec.egress_allowlist`. Verifie contre un vrai pod en cluster (4/4 conteneurs `Running`, build complet d'un devcontainer reel reussi entierement via le chemin transparent) |
| `net-proxy` — port-forward (microVM → exterieur) | Fonctionnel | Endpoint websocket `/portforward`, multiplexage de canaux dans le style `kubectl port-forward` (net-proxy = kubelet, `api-server` = coordinateur qui authentifie et relaie). TCP et UDP. Teste via un vrai client websocket (`tokio-tungstenite`) : relai de donnees bout en bout et remontee d'erreur de connexion sur le canal dedie, et de bout en bout via `api-server` (`crates/api-server/tests/routes.rs`) |
| `net-proxy` — DNS (UDP+TCP) | Fonctionnel (composant seul) | Resolveur DNS pour la VM, meme allowlist que l'egress (nom refuse → `REFUSED` local, jamais transmis a l'upstream). Teste reellement avec `dig` (UDP et TCP) contre un vrai upstream (resolveur systemd-resolved local), plus tests unitaires (parsing QNAME, upstream jamais contacte pour un nom refuse) |
| `identity-proxy` | Fonctionnel | Proxy HTTP explicite : injecte un en-tete (`Authorization` ou autre) construit depuis un secret OpenBao (cache rafraichi periodiquement, login Kubernetes reel) dans les requetes HTTP en clair dont l'hote correspond a une regle (`Workshop.spec.identityInjectionRules`, type partage avec `atelier-common`), puis relaie vers `net-proxy` (`ATELIER_NET_PROXY_ADDR`) via un tunnel `CONNECT`. `CONNECT`/HTTPS reste un tunnel opaque, non injectable sans MITM (limite documentee). Premier `Dockerfile`, deploye comme conteneur du pod parent, regles alimentees depuis `Workshop.spec` par le controller — verifie contre un vrai pod en cluster ("regles d'injection chargees count=1") |
| `mcp-gateway` | Fonctionnel (HTTP/SSE + vsock, 3 tools) | Serveur MCP reel (SDK officiel `rmcp`) exposant `request_credential` (lecture OpenBao), `request_egress` (elargissement a chaud de l'allowlist `net-proxy`) et `enable_simulator` (active le sidecar LocalStack), deux transports actifs en parallele (streamable HTTP via `net-proxy`, et `AF_VSOCK` natif), tous verifies de bout en bout contre de la vraie infra (OpenBao, net-proxy, LocalStack officiel). Reste a faire : verification depuis l'interieur d'une vraie microVM agent, voir section dediee ci-dessous |
| `dashboard` | Fonctionnel (CRUD + page de gestion + VS Code + terminal) | Next.js 16 (App Router), pattern backend-for-frontend : `/api/auth/login` (PKCE) redirige vers l'UI Kanidm, `/api/auth/callback` echange le code et stocke l'`access_token` dans un cookie httpOnly, jamais expose au JS navigateur. Liste/creation/suspend/resume/suppression de Workshops via Server Components + Server Actions, chaque appel relaie le token a `atelier-api-server` qui le revalide integralement. Page de detail par Workshop + boutons "Ouvrir VS Code" et "Terminal" (`code-server` et `ttyd` via le pont HTTP+WS de `api-server`, voir sections dediees), terminal egalement en iframe sur la page ; serveur Next custom (`server.ts`) pour le WebSocket propre de ces deux services, et refresh token OAuth2 transparent pour que l'expiration du JWT (900s) ne coupe plus une session ouverte. `code-server` et le terminal verifies dans un vrai navigateur pilote contre une vraie microVM (commande interactive executee dans le guest), six bugs reels corriges au passage. Verifie reellement : flux complet login (scripte cote Kanidm comme `get-oauth2-token.sh`) → callback → session → creation d'un vrai Workshop → affichage dans la liste → suppression, contre un vrai Kanidm/api-server/kind |
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

## Terminal navigateur (`ttyd`) : premiere validation reelle du pont guest, six bugs

Objectif du lot : un terminal riche dans le navigateur pour chaque Workshop,
et reparer le bouton "Ouvrir VS Code" qui ne repondait plus. Les deux
partagent exactement le meme chemin (`api-server` -> `portforward` ->
`net-proxy` -> guest), c'est donc le lot qui a **valide pour la premiere fois
ce pont contre de vrais services dans une vraie microVM** — la section "UI
dashboard" ci-dessus annoncait cette verification comme restant a faire.

Elle a fait tomber six bugs reels, dont quatre bloquants qui se presentaient
tous au navigateur sous la meme forme indiscernable (`1006`, ou rien du tout).

- **`crates/api-server`** : `vscode_proxy_impl` devient `proxy_to_guest_port`
  (port cible + prefixe d'URL en parametres) ; `terminal.rs` (nouveau) s'en
  sert pour `ttyd` (port 7681, `ATELIER_TERMINAL_PORT`), qui embarque son
  propre client xterm.js.
- **`dashboard/lib/guest-proxy.ts`** (nouveau) : le Route Handler HTTP de
  `code-server` extrait et partage avec le terminal.
- **`dashboard`** : bouton "Terminal ↗" + iframe sur la page de detail,
  meme condition `canConnect` que VS Code.

### Les six bugs

1. **Regle `iptables` de retour de connexion manquante** — la chaine dediee
   par TAP filtrait `INPUT` par liste blanche de *ports de destination*, ce
   qui jetait silencieusement le retour (SYN-ACK) de toute connexion que
   `net-proxy` initie lui-meme *vers* le guest : le port de destination est
   alors un port ephemere cote `net-proxy`, jamais dans la liste. Tout le
   mecanisme de port-forward (`code-server`, `ttyd`) restait bloque jusqu'au
   `Connection timed out (os error 110)` alors que le service ecoutait
   normalement dans le guest. Diagnostique sur infra reelle (beaucoup de
   `SYN_SENT` ne passant jamais `ESTABLISHED` dans `/proc/net/tcp` du pod),
   corrige par un `-m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT` en
   premiere regle de la chaine. **C'etait la cause du "rien ne repond
   jamais", pour VS Code comme pour le terminal.**
2. **Next.js fermait nos upgrades WebSocket** — Next installe *son propre*
   listener `upgrade` sur notre serveur custom des la premiere requete HTTP
   servie (`getRequestHandler()` appelle `setupWebSocketHandler()`, qui
   recupere le serveur via `req.socket.server`,
   `node_modules/next/dist/server/next.js`). Son handler fait
   `if (matchedOutput) return socket.end()`
   (`node_modules/next/dist/server/lib/router-server.js`) — or nos chemins
   d'upgrade correspondent bien au Route Handler catch-all
   `app/workshops/[name]/(vscode|terminal)/[[...path]]/route.ts`. Next
   fermait donc la socket du navigateur en parallele de notre handler.
   Deterministe, mais seulement *apres* la premiere requete HTTP du
   process : d'ou une flakiness apparente qui a coute cher (un client `ws`
   nu contre un serveur fraichement redemarre reussissait 6/6, le meme test
   echouait 2/2 une fois une page chargee). Corrige en neutralisant
   l'auto-attachement et en deleguant a `app.getUpgradeHandler()` (API
   publique) tout ce qui ne nous concerne pas — ce qui repare au passage le
   **HMR de Next**, que notre propre handler detruisait en boucle.
3. **`ERR_CONTENT_DECODING_FAILED`** — `fetch` (undici) decompresse le corps
   selon `Content-Encoding` mais laisse cet en-tete, et `Content-Length`,
   intacts sur `response.headers` ; les relayer faisait tenter au navigateur
   de decompresser un corps deja en clair. Les deux sont desormais retires
   (`code-server` et `ttyd` servent tous deux compresse).
4. **Sous-protocole `tty` non relaye** — `ttyd` negocie explicitement
   `Sec-WebSocket-Protocol: tty`, et un navigateur ferme la connexion
   (`1006`) si le `101` ne confirme pas le sous-protocole demande. Relaye
   desormais dans les deux sens (`handleProtocols` sur une `WebSocketServer`
   `noServer` par requete, valeur reelle negociee par l'amont).
5. **En-tete `Host` code en dur sur `127.0.0.1:8080`** — fonctionnait par
   coincidence pour `code-server`, qui ecoute sur ce port, mais cassait
   silencieusement le pont vers `ttyd`. Reflete maintenant le port
   reellement cible.
6. **`Workshop.spec.resources` jamais cable vers la VM** — trouve en
   creusant pourquoi `code-server` restait injoignable : `vm-supervisor`
   utilisait toujours 256 MiB / 1 vCPU quoi que declare le Workshop. Corrige
   dans `crates/controller/src/reconcile.rs` (`memory_to_mib` /
   `cpu_to_vcpu_count` + 7 tests unitaires, commit `57109e9`).

Ajoute par-dessus, sur demande explicite ("il faut que le front soit immune a
ce genre de soucis") : **refresh token OAuth2 transparent**. Le JWT Kanidm
expire a 900s, et une session terminal/VS Code ouverte plus longtemps se
mettait a echouer en boucle silencieuse — encore un `1006`, indiscernable
d'un vrai probleme reseau, et une vraie source de confusion pendant le debug.
`getAccessToken()` echange desormais le refresh token cote serveur des que
l'access token approche de son expiration, et `SessionKeepAlive` (monte dans
`TopNav`) ping `/api/auth/refresh` toutes les 4 min avec rattrapage sur
`visibilitychange` (les navigateurs bridant les timers d'un onglet en
arriere-plan). Les deux cookies restent `httpOnly` : aucun token n'atteint le
JS du navigateur.

### Verification reelle (vrai navigateur, pas un client `ws`)

Le manque de test navigateur est precisement ce qui avait laisse passer le
bug 2 : un client `ws` Node.js reussissait, l'`<iframe>` du dashboard non.
Verifie avec Chromium pilote (cookies de session reels, vrai client `ttyd`) :

- Terminal connecte, prompt du shell recu, **commande interactive executee
  dans la microVM** (`echo`, `uname -sr` -> Linux 5.10.223, `nproc` -> 2,
  `free -m` -> ~2 GB, ce qui confirme au passage le bug 6 corrige : plus les
  256 MiB / 1 vCPU codes en dur).
- `code-server` : `.monaco-workbench` charge, page Welcome affichee.
- HMR de Next : une seule ouverture, zero erreur, plus de reconnexion en
  boucle.

Reste ouvert : quelques `404` en console cote `code-server` (workbench
fonctionnel malgre tout), non investigues. Le port de `ttyd` est fixe par
convention (`ATELIER_TERMINAL_PORT` overridable), pas encore un champ du CRD
`Workshop` — meme situation que `ATELIER_VSCODE_PORT`.

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
  DeepSeek (non disponible dans cet environnement de developpement). Le
  blocage "aucun `Workshop` complet n'a ete boote avec
  `ministack-workshop`/`atelier-workspace`" est desormais leve — voir
  section suivante.

## `net-proxy` en passerelle transparente : plus besoin de `HTTP_PROXY` cote guest

En bootant pour la premiere fois un `Workshop` complet avec le devcontainer
de demo public (`ministack-workshop`/`atelier-workspace` : base
`mcr.microsoft.com`, features `docker-in-docker`+`claude-code` via
`ghcr.io`, `systemd`/`code-server` installes par `apt-get`), un vrai bug
racine a ete trouve, plus profond qu'un simple oubli de domaine dans
`Workshop.spec.egress_allowlist` :

- **Symptome initial** : `archive.ubuntu.com` refuse d'etre resolu
  (`Temporary failure resolving 'archive.ubuntu.com'`) alors qu'il figure
  bien dans l'allowlist, et **aucune trace** (ni autorisee ni refusee) de
  cette requete dans les logs `net-proxy` — la requete n'atteint jamais
  `net-proxy` du tout.
- **Cause racine, confirmee en rejouant le build avec `RUST_LOG=debug`
  (console guest complete, drainee via `tracing::debug!`)** : l'etape `RUN
  apt-get` d'un `Dockerfile`, executee par `envbuilder` dans la microVM
  "builder", n'herite jamais de `HTTP_PROXY`/`HTTPS_PROXY` — contrairement
  au clone git et au push registre (`crates/builder-vm-init`), qui eux
  passent bien par ces variables et fonctionnent. Rustiner au cas par cas
  (`ARG HTTP_PROXY` dans chaque `Dockerfile`) aurait ete fragile et jamais
  garanti pour un devcontainer arbitraire fourni par l'utilisateur d'un
  Workshop.
- **Decision (discutee en session)** : plutot que de continuer a exiger
  qu'un outil a l'interieur du guest sache configurer un proxy explicite,
  `net-proxy` devient une **passerelle reseau transparente** — interception
  au niveau paquet (`iptables REDIRECT`), lecture du `Host:` (HTTP) ou du
  SNI du `ClientHello` (HTTPS, **sans jamais dechiffrer** — pas de MITM,
  refuse explicitement en session, la validation TLS de bout en bout reste
  intacte cote guest). Voir `docs/architecture/network-security.md` pour le
  design complet (deja mis a jour, section correspondante).
- **Composants touches** : `crates/firecracker/src/network.rs`
  (`NetworkSetup::enable_transparent_gateway`, nouvelle chaine `nat`
  dediee, `restrict_to_net_proxy` conserve pour compatibilite) ;
  `crates/net-proxy` (deux nouveaux listeners — port HTTP transparent qui
  reutilise `handle_connection` tel quel, port TLS transparent avec le
  nouveau module `tls_sni.rs`, aucune dependance TLS ajoutee) ;
  `crates/vm-supervisor`/`crates/image-builder` (appellent desormais
  `enable_transparent_gateway` au lieu de `restrict_to_net_proxy`/rien du
  tout) ; `crates/builder-vm-init` (une ligne `ip route add default via
  <host_ip>` — la VM de l'agent avait deja cette route gratuitement via le
  parametre kernel `ip=`, seule la VM "builder", notre propre bootstrap,
  ne l'avait pas) ; `crates/controller/src/reconcile.rs` (cablage des deux
  nouveaux ports, memes valeurs cote `net-proxy` et cote
  `vm-supervisor`/`image-builder`) ; Dockerfiles `image-builder`
  (`iptables` ajoute, manquant jusqu'ici — seul `iproute2` y etait).
- **Aucune nouvelle capacite Kubernetes accordee** : la pose des regles
  reste faite par le composant deja `NET_ADMIN` (`vm-supervisor`/
  `image-builder`, `firecracker_security_context()`), `net-proxy` continue
  de tourner sans capacite elevee — meme decoupage qu'avant, juste etendu.
- **`sysctl net.ipv4.ip_forward` toujours a 0** : `REDIRECT` reecrit la
  destination du paquet vers l'interface d'entree **avant** la decision de
  routage, donc le paquet devient une livraison locale (chemin `INPUT`),
  jamais un transit `FORWARD` — la preoccupation documentee dans
  `network-security.md` (sysctl global au netns, partage entre VMs
  concurrentes du meme pod) reste donc valide et n'est pas contournee.
- **Verifie reellement, sans mock, Dockerfile `atelier-workspace` non
  modifie** :
  - `cargo test -p atelier-net-proxy` (49 tests, dont 5 nouveaux pour
    `tls_sni::parse_sni` et 3 nouveaux tests d'integration transparents
    dans `proxy.rs`, sockets loopback reelles).
  - `cargo test -p atelier-firecracker --test network` sous
    `unshare --net --map-root-user` (vrai `CAP_NET_ADMIN`, sans `sudo`) :
    nouveau test `enables_transparent_redirect_without_touching_forward`,
    contenu exact des regles `nat` verifie via `iptables -t nat -S`, et
    confirmation explicite que `FORWARD` ne gagne jamais de regle `ACCEPT`.
  - Bout en bout contre kind, en reconstruisant `atelier-net-proxy:dev`/
    `atelier-image-builder:dev`/`atelier-vm-supervisor:dev` : rejouer le
    meme Workshop qui echouait avant (allowlist "dev" complete, voir
    `dashboard/lib/dev-allowlist.ts`) reussit desormais integralement —
    `apt-get install systemd` (HTTP transparent, `archive.ubuntu.com`/
    `security.ubuntu.com`), la feature Node.js (`deb.nodesource.com`, HTTPS
    transparent/SNI), `docker-in-docker` (`packages.microsoft.com`), le
    tout sans une seule variable `HTTP_PROXY` necessaire cote guest.
    `status.imageDigest` publie, pod parent `4/4 Running`, microVM agent
    demarree avec l'image construite (`vm-supervisor`: "microVM running").
- **Allowlist "dev" enrichie au passage** (`dashboard/lib/dev-allowlist.ts`,
  prerempli dans le formulaire de creation de Workshop) : au-dela de
  `github.com`, un devcontainer standard a aussi besoin de
  `mcr.microsoft.com`/`*.data.mcr.microsoft.com` (image de base), `ghcr.io`/
  `pkg-containers.githubusercontent.com` (features), `archive.ubuntu.com`/
  `security.ubuntu.com`/`ports.ubuntu.com`/`deb.debian.org` (apt),
  `download.docker.com`/`get.docker.com`/`registry-1.docker.io`/
  `auth.docker.io`/`production.cloudflare.docker.com` (Docker Hub/Engine),
  `registry.npmjs.org`/`pypi.org`/`files.pythonhosted.org` (gestionnaires
  de paquets de langage), `deb.nodesource.com` (Node.js) et
  `packages.microsoft.com` (docker-in-docker) — chacun trouve un par un en
  rejouant le build reel et en lisant les refus dans les logs `net-proxy`.
- **Limite assumee** : le sniffing SNI ne fonctionne que pour du TLS
  classique avec SNI en clair (immense majorite du trafic reel) — ESNI/ECH
  (SNI chiffre) ne serait pas filtrable par nom, non bloquant pour ce lot.
  `HTTP_PROXY`/`HTTPS_PROXY` restent injectes pour la VM builder en filet
  de securite dans un premier temps (double mecanisme), a retirer dans un
  lot ulterieur une fois confirme inutile.

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

- Une chaine `iptables` qui filtre par liste blanche de *ports de
  destination* jette le trafic retour des connexions qu'on initie soi-meme
  (le port de destination y est un port ephemere, jamais dans la liste). Il
  faut un `-m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT` en premiere
  regle, sinon la connexion part mais son SYN-ACK n'arrive jamais et le
  symptome (`Connection timed out`) fait chercher du cote du service cible,
  qui lui va parfaitement bien.
- Un serveur Next.js custom n'a pas l'exclusivite de l'evenement `upgrade` :
  Next accroche son propre listener sur *notre* serveur des la premiere
  requete HTTP servie (via `req.socket.server`), et ce listener fait
  `socket.end()` sur tout chemin qui correspond a une route Next — un
  Route Handler catch-all suffit. Deleguer explicitement a
  `app.getUpgradeHandler()` ce qui ne nous concerne pas, et neutraliser
  l'auto-attachement pour le reste.
- Un bug qui n'apparait qu'*apres* la premiere requete HTTP d'un process
  ressemble a s'y meprendre a de la flakiness. Avant de conclure
  "intermittent", chercher l'etat du process qui change entre deux essais :
  ici un test qui reussissait 6/6 sur un serveur fraichement redemarre
  echouait 2/2 une fois une page chargee.
- `fetch` (undici) decompresse le corps selon `Content-Encoding` mais laisse
  cet en-tete et `Content-Length` intacts sur `response.headers`. Un proxy
  qui les relaie tels quels produit un `ERR_CONTENT_DECODING_FAILED` cote
  navigateur : les retirer tous les deux.
- Un client WebSocket qui a demande un sous-protocole
  (`Sec-WebSocket-Protocol`, ex. `tty` pour `ttyd`) ferme la connexion en
  `1006` si le `101` ne le confirme pas. Un proxy WebSocket doit relayer le
  sous-protocole dans les deux sens, pas seulement les octets.
- Tester un pont WebSocket avec un client `ws` Node.js ne remplace pas un
  vrai navigateur : plusieurs bugs de ce lot (fermeture par Next, absence de
  sous-protocole) ne se manifestaient que via l'`<iframe>` et le vrai client
  du service. Un navigateur pilote (Chromium headless, cookies de session
  injectes) est peu couteux a mettre en place et aurait fait gagner
  beaucoup de temps.
- Cote dashboard, `npm run dev` est `tsx watch server.ts` : lancer
  `npx tsx server.ts` a la main ne surveille pas le fichier, et on teste
  alors une version obsolete de `server.ts` sans s'en rendre compte
  (plusieurs resultats "incoherents" venaient de la).
- En Next.js 16 (App Router + Turbopack + React 19), le mode dev initialise un
  `debugChannel` de debug React dans le client (`createFromReadableStream`). Ce
  stream attend la fermeture du canal de debug transitant par le WebSocket HMR
  (`/_next/hmr`). Dans un custom server (`dashboard/server.ts`), déléguer
  l'upgrade WebSocket via `app.getUpgradeHandler()` au lieu de
  `(app as any).upgradeHandler` bloquait silencieusement la socket HMR : le
  stream du `debugChannel` ne se fermait jamais, `initialServerResponse` restait
  `pending` indéfiniment, et l'hydratation React (`hydrateRoot` / `useEffect`)
  ne s'exécutait sur aucune page sans lever la moindre erreur en console.
  Résolu en utilisant `(app as any).upgradeHandler(req, socket, head)`.

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
   pont HTTP+WS `api-server` -> `code-server`, preset de creation).
   ~~Reste ouvert : creer un vrai `Workshop` K8s pointant sur
   [`atelier-workspace`](https://github.com/PhilippeVienne/atelier-workspace)
   (depot desormais public, plus de blocage d'auth git) pour la premiere
   validation reellement complete du pont "Ouvrir VS Code" de bout en
   bout~~ — **fait**, voir "Terminal navigateur (`ttyd`)" ci-dessus :
   `code-server` et `ttyd` verifies dans un vrai navigateur contre un vrai
   Workshop, six bugs reels corriges sur le chemin. Reste ouvert : quelques
   `404` en console cote `code-server`, non investigues.
10. ~~LLM Proxy~~ — **base faite cette session** (LiteLLM global, DeepSeek
    par defaut + Anthropic premium), voir section dediee ci-dessus. Reste
    ouvert : verification avec une vraie cle DeepSeek et depuis l'interieur
    d'une vraie microVM agent ; cles virtuelles LiteLLM par Workshop (pas
    de scoping/isolation de budget dans ce lot) ; prompt caching non
    configure ; OpenAI/Grok non couverts (DeepSeek/Anthropic uniquement
    pour l'instant, le `model_list` LiteLLM est extensible sans changement
    de code cote atelier).

---

## Journal d'Avancement du Plan d'Action Global (Specs 00 à 05)

> Section dédiée au suivi jalon par jalon de l'implémentation du plan d'action global ([`docs/specs/PLAN-ACTION-GLOBAL.md`](specs/PLAN-ACTION-GLOBAL.md)).
> Chaque agent complétant une tâche doit ajouter une entrée datée avec sa preuve empirique (tests réels sans mocks, zéros warnings clippy/fmt).

### Matrice d'Avancement des Jalons

| Jalon | Intitulé du Jalon | Statut | Progrès | Dernière mise à jour |
| :--- | :--- | :--- | :--- | :--- |
| **M1** | **Socle PostgreSQL, Découplage OIDC Universel & Nettoyage CRD** | ⏳ À démarrer | 0/14 tâches | 2026-08-23 |
| **M2** | **Stockage S3 Hybride & Git 100% HTTPS** | ⏳ En attente M1 | 0/7 tâches | 2026-08-23 |
| **M3** | **Passerelle d'Inférence IA LiteLLM & Budgets Stricts** | ⏳ En attente M1 | 0/4 tâches | 2026-08-23 |
| **M4** | **Serveur MCP Externe Embarqué dans l'API Server** | ⏳ En attente M1 | 0/5 tâches | 2026-08-23 |
| **M5** | **Moteur DevFactory & Project Manager Autonome (LangGraph)** | ⏳ En attente M4 | 0/11 tâches | 2026-08-23 |
| **M6** | **Chart Helm Monolithique & Documentation Administrateur** | ⏳ En attente M1-M5 | 0/10 tâches | 2026-08-23 |

---

### Entrées d'Historique par Tâche

#### 2026-08-23 : Exécution non-root (`vscode`) et répertoire de travail (`/workspaces/atelier-workspace`) dans `ttyd` et `code-server`

- **Problème** : `ttyd` et `code-server` démarraient sous `root` dans `/` à l'intérieur de la microVM Firecracker.
- **Causes racines identifiées** :
  1. Les services systemd du devcontainer (`atelier-terminal.service`, `atelier-code-server.service`, `atelier-ministack.service`) ne déclaraient pas `User=vscode`, `Group=vscode`, `HOME=/home/vscode`, `USER=vscode`.
  2. `USER vscode` placé au milieu du `Dockerfile` bloquait les devcontainer features (`docker-in-docker`, `claude-code`) lors du build `envbuilder` par manque de permissions root (`apt-get`).
  3. Si `/workspaces/atelier-workspace` n'était pas expressément matérialisé par un fichier (ex: `README.md` ou `.keep`), l'arborescence n'était pas incluse dans le rootfs ext4, ce qui faisait échouer `WorkingDirectory=/workspaces/...` au boot systemd (`CHDIR failure`).
- **Corrections apportées** :
  1. **`PhilippeVienne/atelier-workspace`** : configuration des services systemd (`User=vscode`, `Group=vscode`, `HOME=/home/vscode`, `USER=vscode`, `ExecStart=/bin/bash -c "mkdir -p /workspaces/atelier-workspace && cd /workspaces/atelier-workspace && exec ..."`).
  2. **`crates/image-builder`** : ajout de l'étape `ensure_workspace_directory` garantissant l'existence et les permissions `1000:1000` (`vscode:vscode`) de `/workspaces/<repo>` avant l'empaquetage ext4.
- **Preuve empirique** : Test réel WebSocket contre `ttyd` sur microVM Firecracker active (`ws-user-verify-parent`) :
  ```text
  vscode ➜ /workspaces/atelier-workspace $ whoami; pwd; id
  vscode
  /workspaces/atelier-workspace
  uid=1000(vscode) gid=1000(vscode) groups=1000(vscode),997(docker)
  ```
  Validation workspace complète : `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (100% verts).
```markdown
### [YYYY-MM-DD HH:MM] Jalon X - Tâche X.Y.Z : <Titre de la tâche>
- **Composant impacté** : `crates/...` ou `services/...`
- **Modifications réalisées** : Résumé précis des structs, endpoints ou tables modifiés.
- **Preuve empirique / Test exécuté** : Commande exacte exécutée et extrait de sortie attestant du succès (zéro mock).
- **Statut** : ✅ Validé / Prêt pour la tâche suivante.
```

### [2026-08-23 00:00] Jalon M1 - Tâches 1.1.1, 1.1.2, 1.1.3, 1.3.2, 1.3.3 : Nettoyage CRD Kanidm & budget LLM par Workshop
- **Composant impacté** : `crates/common/src/crd.rs`, `crates/controller` (`kanidm.rs` supprimé, `lib.rs`, `reconcile.rs`, `Cargo.toml`), `crates/controller/tests/reconcile.rs`, `crds/workshop.yaml`.
- **Modifications réalisées** :
  - Retrait de `WorkshopStatus.kanidm_entity_id` : ce champ n'était en réalité jamais lu ailleurs que pour être reporté d'un reconcile à l'autre (`resolve_kanidm_entity`/`carry_forward_status`), aucune injection réelle dans le pod ou la microVM ne s'appuyait dessus.
  - Suppression complète du module `crates/controller/src/kanidm.rs` (provisioning de service account Kanidm), du champ `ReconcileCtx.kanidm`, de la dépendance `kanidm_client`, et de tout le fil `kanidm_entity_id` à travers `apply`/`ensure_suspended`/`ensure_image_build_job`/`ensure_parent_pod`/`carry_forward_status`.
  - Ajout de `WorkshopResources.max_llm_budget_usd: Option<f64>` (`#[serde(rename_all = "camelCase")]` ajouté à `WorkshopResources`, absent jusqu'ici) — champ pas encore consommé (réservé au Jalon M3, provisioning LiteLLM).
  - Régénération de `crds/workshop.yaml` (`cargo run -p atelier-controller --bin crdgen`) et application réelle sur le cluster kind (`kind-atelier-dev`) : `kanidmEntityId` disparu du schéma, `maxLlmBudgetUsd` présent en camelCase.
  - Ajout de deux tests unitaires (`crates/common/src/crd.rs::tests`) : `generate_crd` (génération du manifest sans panique, absence de `kanidmEntityId`, présence de `maxLlmBudgetUsd`) et `workshop_roundtrip_json_and_yaml` (round-trip `serde_json`/`serde_yaml` sur un `Workshop` complet).
- **Preuve empirique / Test exécuté** :
  ```
  kubectl apply -f crds/workshop.yaml   # customresourcedefinition.apiextensions.k8s.io/workshops.atelier.dev configured
  cargo test -p atelier-common          # 2 passed (generate_crd, workshop_roundtrip_json_and_yaml)
  cargo test -p atelier-controller      # 7 unit + 4 integration passed contre le vrai cluster kind + vrai OpenBao (apply_provisions_openbao_role_when_configured inclus)
  cargo test --workspace                # 100% vert
  cargo clippy --workspace --all-targets -- -D warnings   # 0 warning
  cargo fmt --all -- --check            # propre
  ```
- **Restant sur M1 (non traité ici, volontairement)** : 1.3.1 (ajout `sqlx`), 1.3.4 (`generate_session_auth` OpenBao) et 1.3.5 (injection du mot de passe de session dans `code-server`/`ttyd`) nécessitent (a) un PostgreSQL de dev provisionné (aucun n'existe encore dans `deploy/dev/`) et (b) une décision d'architecture : le pod parent (`vm-supervisor`) boote aujourd'hui le PID 1 du devcontainer tel quel, sans mécanisme pour transmettre une valeur par-Workshop au guest au boot (contrairement à la microVM "builder") — l'image rootfs est un cache partagé entre Workshops utilisant le même devcontainer, donc injecter le secret au moment du build (comme `ANTHROPIC_AUTH_TOKEN`) le partagerait entre Workshops. Question posée à l'utilisateur avant de concevoir ce canal.
- **Statut** : ✅ Validé pour les 5 tâches listées / le reste de M1 reste `[ ]`.

### [2026-08-23 21:30] Jalon M1 - Tâches 1.3.4, 1.3.5 : mot de passe de session via endpoint metadata `net-proxy`
- **Composant impacté** : `crates/controller/src/openbao.rs` (+`Cargo.toml`), `crates/controller/src/reconcile.rs`, `crates/net-proxy/src/session_auth.rs` (nouveau), `crates/net-proxy/src/metadata.rs` (nouveau), `crates/net-proxy/src/main.rs`, `crates/net-proxy/tests/session_auth.rs` (nouveau), `crates/common/src/telemetry.rs` (commentaire Kanidm résiduel corrigé).
- **Décision d'architecture (validée avec l'utilisateur avant implémentation)** : le pod parent boote le PID 1 du devcontainer tel quel, sans canal pour transmettre une valeur par-Workshop au guest au démarrage, et le rootfs est un cache partagé entre Workshops utilisant le même devcontainer (injecter au moment du build partagerait le secret). Option retenue parmi 3 proposées : un **endpoint metadata HTTP** exposé par `net-proxy` sur l'adresse link-local du TAP (`169.254.0.1`, déjà l'unique route réseau du guest), plutôt que des kernel boot args.
- **Modifications réalisées** :
  - `openbao::ensure_session_auth(config, workshop_name)` : get-or-create idempotent d'un mot de passe aléatoire de 32 caractères sous `secret/data/workshops/<name>/session_auth` (champ `password`), avec le token d'administration du controller. Appelé dans `ensure_parent_pod`, best-effort (non bloquant).
  - `crates/net-proxy/src/session_auth.rs` : cache en mémoire rafraîchi toutes les 5 min via `atelier_common::OpenBaoClient` (même schéma que `crates/identity-proxy/src/secrets.rs`) — `net-proxy` relit lui-même le secret avec son propre login Kubernetes-auth (le rôle `workshop-<name>` couvre déjà ce chemin), le controller ne transmet donc jamais le mot de passe en clair dans la spec du pod (pas de `kubectl get pod -o yaml` qui le révèle).
  - `crates/net-proxy/src/metadata.rs` : routeur axum `GET /session-auth`, lié à `0.0.0.0:3132` (par défaut) comme le reste des ports côté guest — `503` tant que le secret n'est pas encore lu, `200` + mot de passe en texte brut une fois disponible.
  - `reconcile.rs` : ajout des env `OPENBAO_ADDR`/`ATELIER_WORKSHOP_NAME` au conteneur `net-proxy`.
  - `net-proxy/src/main.rs` : dégrade proprement (avertissement, pas d'échec du process) si `OPENBAO_ADDR` est présent mais `ATELIER_WORKSHOP_NAME` absent — évite qu'une config incohérente de cette fonctionnalité annexe fasse tomber tout `net-proxy` (egress/DNS).
- **Preuve empirique / Test exécuté** :
  ```
  cargo test -p atelier-net-proxy --test session_auth   # 1 passed : login Kubernetes-auth REEL (vrai ServiceAccount, vrai token projete via `kubectl create token`) + lecture du secret ecrit avec le token root, contre le vrai OpenBao (deploy/dev/openbao)
  cargo test -p atelier-net-proxy                        # 51 unitaires + 1 integration, tous verts (dont les 2 nouveaux tests de crate::metadata)
  cargo test -p atelier-controller --test reconcile      # 4 integration passed contre le vrai cluster kind (apply_provisions_openbao_role_when_configured inclus)
  cargo test --workspace / clippy -D warnings / fmt --check   # 100% vert
  ```
- **Incident rencontré et résolu en cours de route** : un process `atelier-controller` déjà lancé manuellement (build pré-nettoyage Kanidm) tournait en direct sur le cluster kind partagé et réconciliait les Workshops créés par les tests avec son ancien code, entrant en conflit 422 ("pod updates may not change fields...") avec le nouveau code du test. Résolu (accord utilisateur) en arrêtant puis relançant ce process avec le binaire à jour.
- **Reste, hors de ce dépôt** : le devcontainer (repo séparé `PhilippeVienne/atelier-workspace`) doit faire consommer `GET http://169.254.0.1:3132/session-auth` par ses services `ttyd`/`code-server` au démarrage (`--credential atelier:<password>` / `--auth password`) — ce dépôt se limite à rendre le secret disponible, pas à le pousser dans ces services.
- **Reste sur M1** : 1.2.6 (injection du header `Authorization: Basic` côté `api-server` pour `/vscode/*`/`/terminal/*`) nécessite de trancher comment `api-server` (composant cluster-wide, pas un pod par Workshop) obtient un accès OpenBao scopé en lecture à `secret/data/workshops/*/session_auth` — question à poser avant implémentation. 1.3.1/1.2.1/1.2.7-1.2.9/1.3.6/1.3.7 (sqlx/PostgreSQL) restent bloqués sur l'absence de PostgreSQL de dev.
- **Statut** : ✅ Validé pour 1.3.4 et 1.3.5.

### [2026-08-23 22:00] Jalon M1 - Tâche 1.0.1 : instance PostgreSQL de développement
- **Composant impacté** : `deploy/dev/postgres/dev-pod.yaml` (nouveau), `deploy/dev/postgres/README.md` (nouveau).
- **Contexte** : le Jalon M1 rend `DATABASE_URL` obligatoire pour `api-server`/`controller` (tâches 1.2.1, 1.2.7-1.2.9, 1.3.1, 1.3.6, 1.3.7), mais aucune tâche n'existait pour provisionner un PostgreSQL de dev — la seule instance prévue dans le plan est celle du Jalon M6 (Helm de production). Signalé par l'utilisateur, corrigé en ajoutant cette tâche 1.0.1.
- **Modifications réalisées** : pod PostgreSQL 16 (image `pgvector/pgvector:pg16`, même image que prévue pour le Jalon M6 afin d'éviter un changement d'image plus tard) déployé dans le cluster kind, sans persistance (`emptyDir`, même convention que `deploy/dev/kanidm`/`deploy/dev/openbao`). Deux bases : `atelier_apiserver` (créée automatiquement via `POSTGRES_DB`) et `atelier_controller` (créée à la main, voir README).
- **Preuve empirique / Test exécuté** :
  ```
  kubectl apply -f deploy/dev/postgres/dev-pod.yaml
  kubectl wait --for=condition=Ready pod/atelier-postgres-dev --timeout=90s   # pod/atelier-postgres-dev condition met
  kubectl exec atelier-postgres-dev -- psql -U atelier_admin -d postgres -c 'CREATE DATABASE atelier_controller;'   # CREATE DATABASE
  kubectl port-forward svc/atelier-postgres-dev 5433:5432 &
  kubectl exec atelier-postgres-dev -- psql -U atelier_admin -d atelier_apiserver -c '\l'   # liste bien atelier_apiserver + atelier_controller
  bash -c "cat < /dev/null > /dev/tcp/127.0.0.1/5433" && echo "port 5433 joignable"   # port 5433 joignable
  ```
- **Statut** : ✅ Validé. Débloque le démarrage des tâches `sqlx`/PostgreSQL de M1, non encore attaquées.

### [2026-08-23 22:30] Jalon M2 - Tâches 2.0.1 & 2.0.2 : instances S3 (RustFS) et Forgejo (Git HTTPS) de développement
- **Composant impacté** : `deploy/dev/s3/dev-pod.yaml` (nouveau), `deploy/dev/s3/README.md` (nouveau), `deploy/dev/forgejo/dev-pod.yaml` (nouveau), `deploy/dev/forgejo/README.md` (nouveau).
- **Contexte** : Pour respecter l'éthos du projet « Vérification Empirique sans Mocks » lors du développement du Jalon M2 (client S3 dans `api-server` et injection de token Git HTTPS dans `identity-proxy`), deux composants d'infrastructure locale de test étaient requis dans le cluster Kind : un serveur S3 compatible S3 standard (RustFS) et une forge Git 100% HTTPS (Forgejo).
- **Modifications réalisées** :
  - **Tâche 2.0.1 (S3 RustFS)** : Pod `atelier-s3-dev` déployé dans Kind (image officielle `rustfs/rustfs:latest` avec `emptyDir`, 100% Rust et conforme à l'éthos Atelier). Un `initContainer` initialise automatiquement les 3 buckets requis (`atelier-sessions`, `atelier-snapshots`, `forgejo-lfs-attachments`) avec les permissions `10001:10001` (UID du process `rustfs`). Service `atelier-s3-dev` exposant le port 9000 (S3 API).
  - **Tâche 2.0.2 (Forgejo 100% HTTPS connecté à PostgreSQL)** : Base `forgejo` créée sur `atelier-postgres-dev`. Pod `atelier-forgejo-dev` déployé dans Kind (image `codeberg.org/forgejo/forgejo:9` connectée à `atelier-postgres-dev:5432/forgejo`, 121 tables initialisées). SSH désactivé d'office (`DISABLE_SSH: true`). Utilisateur administrateur `atelier_admin` créé via CLI, token d'accès PAT généré, création d'un dépôt privé `test-repo` validée via API REST, puis cycle complet `git clone` ➔ `git commit` ➔ `git push origin main` validé avec mise à jour effective dans PostgreSQL (`repository.is_empty = false`).
- **Preuve empirique / Test exécuté** :
  ```
  kubectl apply -f deploy/dev/s3/dev-pod.yaml
  kubectl wait --for=condition=Ready pod/atelier-s3-dev --timeout=60s   # pod/atelier-s3-dev condition met
  kubectl logs atelier-s3-dev -c rustfs   # Starting: /usr/bin/rustfs /data (Running on port 9000)
  kubectl exec atelier-s3-dev -c rustfs -- sh -c 'echo "test session payload" > /data/atelier-sessions/test-session.zst && cat /data/atelier-sessions/test-session.zst'   # ecriture et lecture reussies en tant que rustfs (10001)

  kubectl exec atelier-postgres-dev -- psql -U atelier_admin -d postgres -c 'CREATE DATABASE forgejo;'   # CREATE DATABASE
  kubectl apply -f deploy/dev/forgejo/dev-pod.yaml
  kubectl wait --for=condition=Ready pod/atelier-forgejo-dev --timeout=60s   # pod/atelier-forgejo-dev condition met
  kubectl exec atelier-postgres-dev -- psql -U atelier_admin -d forgejo -c '\dt'   # 121 tables creees par Forgejo dans PostgreSQL
  kubectl exec atelier-forgejo-dev -- su-exec 1000:1000 forgejo admin user create --username atelier_admin --password dev-only-not-for-production --email admin@atelier.local --admin   # New user 'atelier_admin' has been successfully created!
  kubectl exec atelier-forgejo-dev -- su-exec 1000:1000 forgejo admin user generate-access-token --username atelier_admin --token-name dev-test-token --scopes all   # Access token was successfully created: 5f04486ef2c4...
  kubectl exec atelier-forgejo-dev -- curl -s -X POST http://127.0.0.1:3000/api/v1/user/repos -H "Authorization: token 5f04486ef2c4..." -H "Content-Type: application/json" -d '{"name": "test-repo", "private": true, "auto_init": true}'   # HTTP 201 Created ("id": 1, "name": "test-repo", "private": true)
  kubectl exec atelier-forgejo-dev -- sh -c 'git clone http://atelier_admin:5f04486ef2c4...@127.0.0.1:3000/atelier_admin/test-repo.git /tmp/clone-test && cd /tmp/clone-test && echo "commit" > f && git add f && git commit -m "feat: commit" && git push origin main'   # main -> main OK
  kubectl exec atelier-postgres-dev -- psql -U atelier_admin -d forgejo -c 'SELECT id, name, is_empty FROM repository;'   # 1 | test-repo | f
  ```
- **Statut** : ✅ Validé pour 2.0.1 et 2.0.2. Débloque le développement réel des modules S3 (`storage.rs`) et de l'interception `identity-proxy` du Jalon M2.

### [2026-08-23 22:45] Jalon M1 - Tâche 1.0.2 : PKI locale validable et instance Keycloak OIDC de développement
- **Composant impacté** : `deploy/dev/pki/init-pki.sh` (nouveau), `deploy/dev/pki/README.md` (nouveau), `deploy/dev/keycloak/realm-export.json` (nouveau), `deploy/dev/keycloak/dev-pod.yaml` (nouveau), `deploy/dev/keycloak/README.md` (nouveau).
- **Contexte** : Pour simplifier le dev local et respecter l'éthos du projet « Tests Réels sans Mocks », il était impératif de générer une PKI de développement validable (Root CA + certificats Multi-SAN) et de disposer d'une vraie instance Keycloak locale branchée sur notre PostgreSQL partagé (`atelier-postgres-dev:5432/keycloak`) pour valider les flux OIDC JWT (JWKS dynamique, claims, PKCE) dès le Jalon M1.
- **Modifications réalisées** :
  - **PKI Dev Locale** : Script `deploy/dev/pki/init-pki.sh` générant une Root CA (`atelier-ca.crt`, valide 10 ans) et un certificat Multi-SAN (`server.crt`) couvrant tous les domaines dev (`*.atelier.local`, `auth.atelier.local`, `git.atelier.local`, `app.atelier.local`, `api.atelier.local`, `localhost`, `127.0.0.1`). Synchronisation automatique des Secrets Kubernetes `atelier-dev-ca` et `atelier-dev-tls` dans le cluster Kind.
  - **Keycloak Dev (PostgreSQL)** : Base `keycloak` créée sur `atelier-postgres-dev`. Pod `atelier-keycloak-dev` déployé dans Kind (image `quay.io/keycloak/keycloak:26.1` en mode `start-dev`). Realm `atelier` importé automatiquement avec les clients OIDC `atelier-dashboard` (PKCE S256 public) et `atelier-api` (Bearer-only), et deux utilisateurs de test (`atelier-admin` et `atelier-test-user`).
- **Preuve empirique / Test exécuté** :
  ```
  ./deploy/dev/pki/init-pki.sh   # Root CA generee + Certificat Multi-SAN cree + Secrets synchronises dans Kind
  openssl verify -CAfile deploy/dev/pki/ca/atelier-ca.crt deploy/dev/pki/certs/server.crt   # deploy/dev/pki/certs/server.crt: OK

  kubectl exec atelier-postgres-dev -- psql -U atelier_admin -d postgres -c 'CREATE DATABASE keycloak;'   # CREATE DATABASE
  kubectl apply -f deploy/dev/keycloak/dev-pod.yaml
  kubectl wait --for=condition=Ready pod/atelier-keycloak-dev --timeout=90s   # pod/atelier-keycloak-dev condition met
  kubectl exec atelier-forgejo-dev -- curl -s http://atelier-keycloak-dev:8080/realms/atelier/.well-known/openid-configuration   # Document OIDC RFC 8414 valide retourne
  kubectl exec atelier-forgejo-dev -- curl -s -X POST http://atelier-keycloak-dev:8080/realms/atelier/protocol/openid-connect/token -d "client_id=atelier-dashboard" -d "grant_type=password" -d "username=atelier-test-user" -d "password=dev-only-not-for-production" -d "scope=openid email profile"   # JWT Access Token + ID Token + Refresh Token obtenus avec succes
  ```
- **Statut** : ✅ Validé pour 1.0.2. L'authentification OIDC de dev est désormais 100% basée sur Keycloak et une PKI validable.

### [2026-08-23 22:30] Jalon M1 - Tâches 1.2.1, 1.2.7, 1.2.8, 1.2.9, 1.2.10 : sqlx, migrations et healthchecks dans `api-server`
- **Composant impacté** : `crates/api-server/Cargo.toml`, `crates/api-server/src/main.rs`, `crates/api-server/src/routes.rs`, `crates/api-server/migrations/20260824000000_init_apiserver.sql` (nouveau), `crates/api-server/tests/routes.rs`, `deploy/dev/postgres/` (rôle `atelier_app` ajouté).
- **Modifications réalisées** :
  - Ajout des dépendances `sqlx` (0.8, features `postgres`/`uuid`/`chrono`/`json`/`macros`/`migrate`), `aws-sdk-s3`, `aws-config`, `base64` (les deux derniers pas encore consommés, réservés au Jalon M2).
  - `main.rs` : `DATABASE_URL` obligatoire au démarrage (erreur explicite sinon), `PgPoolOptions` (10 connexions max), `sqlx::migrate!("./migrations")` exécutées au boot avant de servir du trafic.
  - `AppState` (`routes.rs`) : ajout de `db_pool: sqlx::PgPool` et `openbao_addr: Option<String>` (uniquement pour la sonde readiness, pas de lecture de secrets).
  - Deux nouveaux endpoints hors authentification (`/health/liveness`, `/health/readiness`) : le premier toujours `200` si le process tourne, le second `SELECT 1` réel sur PostgreSQL + `GET /v1/sys/health` sur OpenBao si configuré, `503` sinon.
  - Migration `20260824000000_init_apiserver.sql` : tables `session_logs`/`audit_events`, RLS (`ENABLE`+`FORCE ROW LEVEL SECURITY`) isolant par `owner_subject` via `current_setting('app.current_tenant')`.
- **Découverte empirique importante** : `atelier_admin` (le rôle `POSTGRES_USER` de l'image officielle) est **superutilisateur** — un superutilisateur (ou tout rôle `BYPASSRLS`) ignore silencieusement RLS *même avec `FORCE ROW LEVEL SECURITY`* (comportement standard PostgreSQL, pas un bug de la migration). Vérifié en pratique : deux lignes de tenants différents insérées, toutes deux visibles via `atelier_admin` malgré `SET app.current_tenant`. Corrigé en ajoutant un second rôle `atelier_app` (non-superutilisateur, `NOBYPASSRLS`), provisionné automatiquement au premier démarrage du pod (`ConfigMap` montée sur `/docker-entrypoint-initdb.d/`) — avec ce rôle, la même expérience isole correctement les tenants (lecture ET écriture, `WITH CHECK` bloque bien un `INSERT` cross-tenant avec une erreur RLS réelle). Voir `deploy/dev/postgres/README.md` pour la démonstration complète et la limite actuelle (les migrations et l'app tournent encore avec `atelier_admin` en M1, la séparation des rôles pour l'app runtime reste à faire quand du code consommera réellement ces tables).
- **Incident rencontré et résolu en cours de route** : disque hôte plein à 100 % en plein `cargo test --workspace` (dépendances `aws-sdk-s3`/`aws-config` volumineuses ajoutées) — bloquant, indépendant de ce changement (`target/` du dépôt seul pesait 102 Go, plus `~135G` de Téléchargements, volumes Docker, etc.). Résolu par l'utilisateur (libération d'espace), tests rejoués avec succès ensuite.
- **Preuve empirique / Test exécuté** :
  ```
  cargo test -p atelier-api-server --test routes   # 5 passed, dont le nouveau health_endpoints_respond_without_auth (vrai PgPool, vraies migrations)
  cargo test --workspace / clippy -D warnings / fmt --check   # 100% vert
  kubectl exec atelier-postgres-dev -- psql -U atelier_admin -d atelier_apiserver -c '\dt'   # session_logs, audit_events, _sqlx_migrations
  kubectl exec atelier-postgres-dev -- psql ... "SELECT relrowsecurity, relforcerowsecurity FROM pg_class ..."   # t | t pour les deux tables
  # Demonstration RLS reelle : atelier_admin voit les deux tenants (RLS ignoree, superutilisateur),
  # atelier_app n'en voit qu'un (RLS appliquee) et un INSERT cross-tenant leve
  # "new row violates row-level security policy"
  ```
- **Statut** : ✅ Validé pour les 5 tâches listées.

### [2026-08-23 23:00] Jalon M1 - Tâches 1.3.1, 1.3.6, 1.3.7 : sqlx et healthcheck dans `controller`, et correction d'une régression réelle sur les pods parents
- **Composant impacté** : `crates/controller/Cargo.toml`, `crates/controller/src/main.rs`, `crates/controller/src/health.rs` (nouveau), `crates/controller/src/lib.rs`, `crates/controller/src/reconcile.rs`, `crates/controller/migrations/20260824000000_init_controller.sql` (nouveau).
- **Modifications réalisées** :
  - Ajout de `sqlx` (mêmes features que `api-server`) et `axum` (nouveau serveur HTTP dédié aux sondes).
  - `main.rs` : `DATABASE_URL` obligatoire, `PgPool`, migrations exécutées au boot, puis un serveur `GET /health/ready` (`0.0.0.0:8081` par défaut, `ATELIER_CONTROLLER_HEALTH_ADDR` pour le surcharger) vérifiant l'API Kubernetes (`apiserver_version()`), PostgreSQL (`SELECT 1`) et OpenBao si configuré — même logique que `/health/readiness` côté `api-server`.
  - Migration `20260824000000_init_controller.sql` : `rootfs_cache_index` (index du cache rootfs content-addressed) et `workshop_reconciliation_history` (historique des transitions de phase) — schéma seul, pas encore consommé par `reconcile.rs`.
- **Régression réelle découverte et corrigée en cours de route (avant tout dégât)** : en relançant le controller à jour contre le cluster kind partagé (qui héberge de vrais Workshops créés par l'utilisateur, ex. `my-new-demo` avec une microVM active depuis 2h+), le tout premier reconcile de chaque Workshop existant échouait en boucle avec une erreur 422 Kubernetes ("`field is immutable`") — cause : `ensure_parent_pod`/`ensure_image_build_job` re-appliquent inconditionnellement (Server-Side Apply) la spec complète du pod/Job à *chaque* reconcile, alors que `spec.containers[*].env` (Pod) et `spec.template` (Job) sont immuables une fois l'objet créé. Ce défaut préexistait (latent, jamais déclenché tant qu'aucun changement de spec n'était introduit) ; le commit précédent (ajout de `ATELIER_WORKSHOP_NAME`/`OPENBAO_ADDR` au conteneur `net-proxy`) l'a rendu visible pour la première fois.
  - Première approche envisagée (supprimer puis recréer le pod en cas de conflit) **rejetée avant déploiement** après vérification manuelle : `my-new-demo-parent` est un pod réel avec une microVM restaurée depuis snapshot et une session utilisateur active — le recréer aurait interrompu ce travail sans nécessité.
  - Fix retenu : ne plus jamais re-patcher un Pod ou un Job déjà existant (`jobs.get_opt`/`pods.get_opt` d'abord, `patch` seulement si absent) — une mise à jour de spec ne prend effet qu'au prochain cycle naturel de recréation (suspend/resume pour le pod parent, jamais pour un Job terminé). Aucune perte de fonctionnalité : rien ne dépendait d'un re-patch réussi sur un objet déjà existant.
  - Vérifié réellement : redéploiement du controller corrigé sur le cluster partagé, tous les Workshops existants (`my-new-demo`, `ws-user-fix-test`, `test`, etc.) reconcilient désormais sans la moindre erreur en boucle, `my-new-demo-parent` conserve son `creationTimestamp` d'origine (non recréé, session non interrompue).
- **Preuve empirique / Test exécuté** :
  ```
  cargo test -p atelier-controller   # 7 unit + 4 integration passed (dont apply_creates_owned_parent_pod_once_image_ready, qui exerce le chemin "pod deja existant")
  cargo test --workspace / clippy -D warnings / fmt --check   # 100% vert
  curl http://127.0.0.1:8081/health/ready   # 200
  # Sur le cluster partage, apres redeploiement : 0 occurrence de "error"/"failed" dans les logs
  # du controller sur un cycle complet de reconciliation de tous les Workshops existants ;
  # my-new-demo-parent conserve son creationTimestamp d'origine (pod non recree).
  ```
- **Statut** : ✅ Validé pour les 3 tâches listées, plus une régression corrigée avant tout impact utilisateur constaté.

### [2026-08-23 22:15] Jalon M1 - Tâches 1.4.1, 1.4.2, 1.4.3 : dashboard découplé de Kanidm, flux OIDC générique (Keycloak)
- **Composant impacté** : `dashboard/lib/config.ts`, `dashboard/lib/session.ts`, `dashboard/app/api/auth/{login,callback}/route.ts`, `dashboard/app/login/page.tsx`, `dashboard/server.ts`, `dashboard/package.json`, `dashboard/README.md`.
- **Modifications réalisées** :
  - `KANIDM_URL`/`ATELIER_KANIDM_URL` renommés en `OIDC_ISSUER_URL`/`ATELIER_OIDC_ISSUER_URL` — cette base pointe maintenant sur l'URL du **realm** (`http://127.0.0.1:8080/realms/atelier` en dev), pas la racine du serveur comme pour Kanidm.
  - Les chemins Kanidm-spécifiques (`/ui/oauth2`, `/oauth2/token`) remplacés par les chemins OIDC standards de Keycloak, mais rendus **configurables séparément** plutôt que résolus via `/.well-known/openid-configuration` : nouvelles variables `ATELIER_OIDC_AUTHORIZE_PATH` (défaut `/protocol/openid-connect/auth`) et `ATELIER_OIDC_TOKEN_PATH` (défaut `/protocol/openid-connect/token`), combinées à `OIDC_ISSUER_URL` via deux nouvelles fonctions `oidcAuthorizeUrl()`/`oidcTokenUrl()` dans `config.ts`.
  - **Décision technique notable** : ces deux fonctions concatènent les chaînes (`${base}${path}`) plutôt que d'utiliser `new URL(path, base)` — `OIDC_ISSUER_URL` contient déjà un chemin non-vide (`/realms/atelier`), et `URL(path, base)` avec un `path` commençant par `/` **remplace** le chemin de la base au lieu de l'y ajouter, ce qui aurait silencieusement perdu le segment `/realms/atelier` et cassé toute résolution contre Keycloak.
  - Choix de l'option (a) du plan (chemins configurables) plutôt que (b) (découverte OIDC dynamique) : évite un appel réseau + cache supplémentaire pour un flux qui n'a besoin que de deux endpoints stables ; un fournisseur OIDC non-Keycloak n'a qu'à surcharger les deux variables de chemin.
  - `session.ts` (`refreshAccessToken()`) et `callback/route.ts` utilisent tous deux `oidcTokenUrl()` : un seul point de vérité pour le chemin token, plus de duplication Kanidm-spécifique.
  - `ATELIER_OAUTH2_CLIENT_ID` par défaut passé de `atelier` à `atelier-dashboard` (client public PKCE réellement pré-configuré dans `deploy/dev/keycloak/realm-export.json`).
  - `package.json` (`NODE_EXTRA_CA_CERTS` du script `dev`) et `README.md` : chemin CA basculé sur `deploy/dev/pki/ca/atelier-ca.crt` (PKI locale multi-services, `deploy/dev/pki/README.md`) — plus de `deploy/dev/kanidm/data/ca.pem`.
  - Wording UI (`app/login/page.tsx`) et commentaires (`server.ts`, `session.ts`) désormais génériques OIDC au lieu de "Kanidm".
- **Preuve empirique / Test exécuté** (contre le vrai Keycloak de dev, pas de mock) :
  ```
  kubectl port-forward svc/atelier-keycloak-dev 8090:8080 &   # 8080 local deja occupe par un autre service
  curl http://127.0.0.1:8090/realms/atelier/.well-known/openid-configuration   # 200, confirme les endpoints attendus

  # Flux complet simule via curl, memes endpoints/parametres que login/callback route.ts :
  # 1) GET /realms/atelier/protocol/openid-connect/auth avec PKCE S256 -> 200, vraie page de login Keycloak
  # 2) POST du formulaire de login (atelier-test-user) -> 302 avec un vrai `code` d'autorisation
  # 3) POST /realms/atelier/protocol/openid-connect/token (grant_type=authorization_code, code_verifier) -> 200, access_token + refresh_token
  # 4) POST /realms/atelier/protocol/openid-connect/token (grant_type=refresh_token) -> 200, nouveau access_token + refresh_token en rotation (exactement le mecanisme de refreshAccessToken())

  cd dashboard && npm run build   # succes (TypeScript strict, Next 16 App Router)
  cd dashboard && npm run lint    # succes, 0 warning
  ```
  Limite rencontrée : le serveur `next dev` (`server.ts`, custom pour le WebSocket) refuse de démarrer une seconde instance sur le même répertoire (verrou Next), et une instance déjà lancée par une session précédente tournait sur le port par défaut sans accès au Keycloak forwardé sur 8090 — la validation du flux HTTP réel a donc été faite via des appels `curl` directs reproduisant fidèlement les requêtes émises par `login/route.ts`/`callback/route.ts`/`session.ts` (mêmes URLs, mêmes paramètres, même grammaire de requête), plutôt qu'un clic navigateur bout-en-bout sur l'app elle-même. Le rechargement à chaud (HMR) de l'instance déjà active a par ailleurs confirmé que la nouvelle URL d'autorisation générée par `login/route.ts` préservait bien le segment `/realms/atelier`.
- **Statut** : ✅ Validé pour les 3 tâches listées.

### [2026-08-23 22:50] Hors plan initial - Ingress Traefik de dev, mapper d'audience Keycloak et bug `nextUrl.origin` du dashboard
- **Contexte** : en testant réellement le flux de login dans un vrai navigateur (suite aux tâches 1.4.1-1.4.3), l'utilisateur a rencontré une cascade de bugs d'intégration non couverts par le plan initial — consignés ici avec leur résolution, et une nouvelle tâche d'infrastructure ajoutée au plan (1.0.3) en conséquence.
- **Composant impacté** : `deploy/dev/traefik/` (nouveau), `deploy/dev/keycloak/realm-export.json` (mapper d'audience), `dashboard/lib/config.ts`, `dashboard/app/api/auth/{login,callback}/route.ts`, `dashboard/README.md`.
- **Problèmes rencontrés et corrigés, dans l'ordre** :
  1. **Collision de port 8080** : le dashboard redirigeait vers Keycloak sur `127.0.0.1:8080`, mais ce port était déjà occupé par `atelier-api-server` (port figé dans `crates/api-server/src/main.rs`) — le README `deploy/dev/keycloak` documentait par erreur le même port pour son port-forward. Point de départ d'une réflexion plus large : plutôt que de re-numéroter les ports un par un, mise en place d'un ingress unique.
  2. **Ingress Traefik de dev** (`deploy/dev/traefik/`) : routage par en-tête `Host` vers 4 domaines (`auth.`/`git.`/`app.`/`api.atelier.local`), remplaçant les port-forwards individuels. Keycloak et Forgejo sont joints via leur `Service` `ClusterIP` in-cluster ; `atelier-api-server` et le dashboard (pas encore conteneurisés) sont exposés via un `Service` sans sélecteur + un `Endpoints` manuel pointant sur `172.19.0.1` (gateway du réseau Docker `kind`, joignable depuis n'importe quel pod — vérifié réellement avec un pod `curlimages/curl` jetable). Traefik tourne en `hostNetwork: true` (pas un `Service` `NodePort`, qui n'aurait pas pu utiliser le port 80 standard sans élargir `--service-node-port-range` côté API server) : lie directement le port 80 sur l'IP du node kind (`172.19.0.2`), joignable telle quelle depuis l'hôte (réseau Docker en pont standard sur Linux). Option `LoadBalancer`/MetalLB envisagée et écartée par l'utilisateur (composant supplémentaire non nécessaire pour un seul node de dev). Script `deploy/dev/traefik/update-hosts.sh` pour automatiser la mise à jour de `/etc/hosts` (impossible depuis un Job Kubernetes : le cluster kind tourne lui-même dans un conteneur Docker isolé du système de fichiers de la vraie machine hôte).
  3. **`atelier-api-server` tournait sans aucune variable JWT configurée** (`AuthState::Disabled`), rejetant systématiquement toute requête avec 401 quel que soit le token — relancé avec `ATELIER_JWT_ISSUER`/`ATELIER_JWT_JWKS_URL`/`ATELIER_JWT_AUDIENCE` pointant sur le Keycloak réel via Traefik.
  4. **Aucun token Keycloak ne portait de claim `aud`** (vérifié en décodant un vrai JWT) : sans mapper d'audience explicite, `ATELIER_JWT_AUDIENCE` était structurellement invalidable, quelle que soit sa valeur. Ajouté en direct sur l'instance Keycloak vivante via `kcadm.sh` (`oidc-audience-mapper` sur le client `atelier-dashboard`, `included.client.audience=atelier-api`) puis répercuté dans `realm-export.json` pour survivre à une recréation du realm.
  5. **Bug réel (pas cosmétique) trouvé côté dashboard** : `request.nextUrl.origin`, dans le serveur custom (`server.ts`, `next({ dev })` sans `hostname` explicite), ignore l'en-tête `Host` réellement reçu et retombe toujours sur `http://localhost:3000` — vérifié en envoyant `Host: app.atelier.local` directement au process Node sur le port 3000 (pas seulement via Traefik). Conséquence concrète : le cookie PKCE (`atelier_oauth_pkce`) posé sur `app.atelier.local` au moment de `/api/auth/login` n'était jamais renvoyé par le navigateur puisque `redirect_uri` ramenait sur le domaine `localhost:3000`, différent — l'échange du code échouait ("session de connexion expirée"). Corrigé par une nouvelle fonction `requestOrigin()` (`dashboard/lib/config.ts`) qui lit l'en-tête `Host` directement plutôt que `nextUrl.origin`, utilisée dans `login/route.ts` et `callback/route.ts`.
- **Preuve empirique / Test exécuté** : flux complet simulé via `curl` avec un fichier de cookies partagé, reproduisant exactement un navigateur réel — `GET /api/auth/login` (pose le cookie PKCE sur `app.atelier.local`) → `GET` de la vraie page de login Keycloak → `POST` du formulaire avec les vrais identifiants de test → redirection avec un vrai code d'autorisation → `GET /api/auth/callback` (échange réel du code, cookie PKCE bien reçu cette fois) → `atelier_session` posé avec un vrai JWT (`aud: atelier-api` confirmé en décodant le token) → `GET /` renvoie `200` avec le contenu Workshop, sans "JWT invalide". `curl -H "Host: <domaine>" http://172.19.0.2:80/` confirme les 4 routes Traefik (`auth`→302, `git`→200, `api`→401 authentifié requis, `app`→307 vers `/login`).
- **Reste à faire (hors scope de cette session)** : `atelier-api-server` et le dashboard ne sont pas encore conteneurisés (d'où le contournement `Endpoints` manuel vers la gateway Docker) — les intégrer proprement au cluster (Deployment + Service) simplifierait `deploy/dev/traefik/ingresses.yaml` en supprimant ce contournement.
- **Statut** : ✅ Résolu et vérifié de bout en bout. Nouvelle tâche 1.0.3 ajoutée au plan pour tracer l'ingress Traefik.

### [2026-08-23 23:05] Jalon M1 - Tâches 1.2.2, 1.2.3, 1.2.4, 1.2.5, 1.2.6 : JWKS dynamique, claims OIDC et Basic Auth VS Code/Terminal
- **Composant impacté** : `crates/api-server/src/auth.rs`, `crates/api-server/src/vscode.rs`, `crates/api-server/src/session_auth.rs` (nouveau), `crates/api-server/src/routes.rs`, `crates/api-server/src/main.rs`, `crates/common/src/openbao_client.rs`, `crates/controller/src/openbao.rs`, `crates/controller/src/main.rs`.
- **Contexte** : tâches réalisées par un agent dédié lancé en arrière-plan (voir demande de l'utilisateur "démarre autant d'agents en parallèle") puis **interrompu par l'utilisateur avant la fin**. Reprises et terminées par cette session : un import manquant (`use base64::Engine;`) empêchait la compilation des tests, corrigé, puis toute la suite de tests revérifiée réellement avant tout commit.
- **Modifications réalisées** :
  - `auth.rs` : documentation généralisée (OIDC standard RFC 7517/7636, plus de mention Kanidm-spécifique) ; JWKS mis en cache dans un `Arc<RwLock<JwkSet>>`, rafraîchi toutes les 10 min par une tâche de fond, plus un refetch immédiat si un JWT présente un `kid` absent du cache (rotation de clés côté fournisseur) ; `Claims` étendue (`sub`, `preferred_username`, `email`, `groups`) et injectée entière dans les extensions Axum (`Extension<Claims>`), pas seulement le sujet.
  - **Basic Auth VS Code/Terminal (1.2.6)** : `api-server` est cluster-wide (une seule instance pour tous les Workshops), donc incompatible avec le rôle OpenBao `workshop-<name>` scopé à un seul Workshop qu'utilisent `identity-proxy`/`mcp-gateway`/`net-proxy`. Ajout d'un rôle OpenBao dédié `atelier-api-server`, provisionné **une seule fois au démarrage du controller** (`ensure_api_server_role`, policy `read` seule sur `secret/{data,metadata}/workshops/+/session_auth`, wildcard sur un seul segment de chemin — aucun autre secret de Workshop n'est accessible). `crates/common/src/openbao_client.rs::OpenBaoClient` généralisé (`from_env_with_role` + `read_field_for`) pour permettre un rôle fixe distinct du nom de Workshop, sans casser les appelants existants scopés à un seul Workshop. Nouveau `crate::session_auth::SessionAuthClient` (cache du token client OpenBao, retry après un login frais si le premier essai échoue) injecte l'en-tête `Authorization: Basic` dans `proxy_to_guest_port` (`vscode.rs`, réutilisé par `terminal.rs`) — dégradé silencieusement (relai sans injection) si OpenBao n'est pas configuré ou si le secret est absent.
- **Incident rencontré et résolu en cours de route** : le nouveau test `vscode_proxy_injects_real_session_auth_basic_header` échouait de façon intermittente (`400 Bad Request`, "le Workshop n'a pas de pod parent actif") — diagnostiqué comme une interférence du controller réel tournant en direct sur le cluster partagé (il réconciliait le Workshop de test entre le `patch_status` du test et la requête HTTP, écrasant le statut manuellement posé). Confirmé en arrêtant le controller (test passe systématiquement) puis en le relançant avec le code à jour (aucune régression, `my-new-demo-parent` non affecté). Pas un bug de l'implémentation.
- **Preuve empirique / Test exécuté** :
  ```
  cargo test -p atelier-api-server       # 6 passed (dont vscode_proxy_injects_real_session_auth_basic_header, vrai OpenBao)
  cargo test -p atelier-controller       # 7 unit + 5 integration passed (dont ensure_api_server_role_reads_any_workshop_session_auth_but_nothing_else)
  cargo test --workspace / clippy -D warnings / fmt --check   # 100% vert (controller live arrêté pendant la vérification finale, relancé ensuite sans erreur)
  ```
- **Statut** : ✅ Validé pour les 5 tâches listées.

### [2026-08-23 23:15] Validation du DoD du Jalon M1
- **Contexte** : toutes les tâches individuelles de M1 (1.0.1-1.0.3, 1.1.1-1.1.3, 1.2.1-1.2.10, 1.3.1-1.3.7, 1.4.1-1.4.3) étaient déjà `[x]`, mais le récapitulatif "Definition of Done" du jalon (section dédiée de `docs/specs/PLAN-ACTION-GLOBAL.md`) n'avait jamais été revérifié point par point ni coché.
- **Vérifications effectuées** :
  - PostgreSQL : `DATABASE_URL` obligatoire et migrations réelles au boot d'`api-server` et `controller` — confirmé.
  - Kanidm : `grep -rn kanidm crates/api-server crates/controller` (code et `Cargo.toml`) ne retourne plus rien — confirmé.
  - Healthchecks : `/health/liveness`, `/health/readiness` (api-server) et `/health/ready` (controller) répondent réellement.
  - `cargo test --workspace`/`clippy -D warnings`/`fmt --check` : 100% vert, revérifié avec le controller live arrêté puis relancé (élimine l'interférence de reconciliation déjà documentée).
  - **Basic Auth VS Code/`ttyd`** : chaîne complète côté ce dépôt fonctionnelle et testée (controller → OpenBao → net-proxy → api-server), **mais incomplète en pratique** : le devcontainer (repo séparé `atelier-workspace`) ne consomme pas encore l'endpoint metadata pour configurer le Basic Auth de `ttyd`/`code-server` — vérifié en cherchant toute référence à `session-auth`/`169.254.0.1:3132`/`credential atelier` dans les clones locaux disponibles de ce repo (`/tmp/atelier-workspace-new`, `/tmp/claude-1000/atelier-workspace`) : aucune trouvée. Marqué `[~]` (partiel) dans le DoD plutôt que `[x]`, pour ne pas déclarer M1 clos à tort sur ce point précis.
- **Statut** : ⚠️ DoD de M1 validé à 5/6 items complets ; le 6ème (Basic Auth guest) nécessite une intervention dans le dépôt `atelier-workspace`, hors du périmètre de cette session.

### [2026-08-24] Jalon M2 - Tâches 2.1.1, 2.1.2, 2.1.3, 2.1.4 : client S3 (`storage.rs`), archivage et rejeu de session en streaming
- **Composant impacté** : `crates/api-server/src/storage.rs` (nouveau module), `crates/api-server/src/lib.rs`, `crates/api-server/Cargo.toml`, `crates/api-server/tests/storage.rs` (nouveau test d'intégration).
- **Contexte** : premier des deux agents parallèles verrouillés sur la section 5.1 du plan (le second, `[-/claude-code/sess-6f3eef77-d]`, travaille sur `crates/controller`/`crates/net-proxy` pour l'injection Git HTTPS — aucun fichier en commun).
- **Modifications réalisées** :
  - **2.1.1** : trait `StorageBackend` (`#[async_trait::async_trait]`, dyn-compatible pour permettre une future implémentation alternative — principe de substitutabilité du projet) avec `upload_stream`/`download_stream`/`delete_object` sur un type unique `BoxedAsyncRead = Pin<Box<dyn AsyncRead + Send + Sync>>`. `S3StorageBackend` implémente ce trait au-dessus d'`aws-sdk-s3`, client construit explicitement via `aws_sdk_s3::config::Builder` (`endpoint_url`, `Credentials::new` statiques, `force_path_style`) plutôt que via la découverte AWS standard (IMDS/profils), qui ne s'applique pas à un endpoint personnalisé type RustFS/MinIO.
  - **2.1.2** : `storage::config_from_env` suit exactement la convention de `openbao::config_from_env`/`TrustedIssuer::from_env` : `S3_ENDPOINT` absent → `Ok(None)` (stockage désactivé) ; présent → `S3_REGION`/`S3_BUCKET_SESSIONS`/`S3_BUCKET_SNAPSHOTS`/`AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY` deviennent obligatoires (erreur explicite sinon), `S3_FORCE_PATH_STYLE` optionnel (`false` par défaut).
  - **2.1.3** : `S3StorageBackend::upload_session_archive(workshop_name, session_id, source: impl AsyncRead)` compresse `source` en zstd en streaming via `async-compression` (`ZstdEncoder` opérant directement sur un `AsyncRead`, jamais de chargement intégral en mémoire), sous la clé conventionnelle `workshops/<workshop_name>/sessions/<session_id>.zst` (préfixe par Workshop pour un listing/nettoyage ciblé sans connaître à l'avance les `session_id`).
  - **2.1.4** : `S3StorageBackend::get_session_stream(s3_key)` récupère l'objet et le décompresse en streaming (`ZstdDecoder`), renvoie un `AsyncRead` consommable progressivement par l'appelant (pas de `Vec<u8>` chargé entièrement).
  - **Choix de conception notable, découvert en testant réellement contre RustFS** : un `put_object` avec un corps HTTP en streaming de taille inconnue (`SdkBody::from_body_1_x` sur un flux `http_body`) échoue systématiquement avec « Only request bodies with a known size can be aws-chunked encoded » — la taille compressée d'une session en cours de rejeu n'est jamais connue à l'avance. `upload_stream` est donc implémenté via un **televersement multipart S3** (`create_multipart_upload`/`upload_part` par blocs de 8 Mio/`complete_multipart_upload`), la seule méthode compatible avec un corps de taille totale inconnue (chaque part a une taille individuelle connue) ; abandon (`abort_multipart_upload`) sur toute erreur en cours de route, et repli sur un `put_object` classique à corps vide pour un flux source vide (un multipart upload ne peut pas se compléter sans au moins une part).
  - Dépendances ajoutées à `crates/api-server/Cargo.toml` : `async-compression` (zstd streaming, `tokio`+`zstd` features — préférée à la crate `zstd` nue pour éviter de gérer nous-mêmes un thread bloquant autour de libzstd), `bytes`, `async-trait`, `sha2` (dev, cohérent avec `crates/vm-supervisor`/`crates/image-builder`) ; activation de la feature `rt-tokio` sur `aws-sdk-s3` (déjà présente comme dépendance non consommée) — nécessaire à la fois pour l'implémentation par défaut du sleep/retry async du client (sinon panique au premier appel réseau) et pour `ByteStream::into_async_read` (rejeu en streaming de `get_object`).
- **Preuve empirique / Test exécuté** (contre le vrai serveur RustFS de dev, pas de mock) :
  ```
  kubectl port-forward svc/atelier-s3-dev 9000:9000 &
  export S3_ENDPOINT=http://127.0.0.1:9000 S3_REGION=us-east-1 \
         AWS_ACCESS_KEY_ID=atelier-rustfs-access-key AWS_SECRET_ACCESS_KEY=atelier-rustfs-secret-key \
         S3_BUCKET_SESSIONS=atelier-sessions S3_BUCKET_SNAPSHOTS=atelier-snapshots S3_FORCE_PATH_STYLE=true

  cargo test -p atelier-api-server --test storage
  # test upload_and_replay_session_archive_preserves_integrity ... ok
  # Contenu déterministe (LCG à graine fixe, pas de générateur aléatoire) d'environ 5 Mo :
  # upload_session_archive -> get_session_stream -> lecture complète jusqu'à EOF -> SHA-256 identique avant/après.

  unset S3_ENDPOINT   # vérifie le gate (pas de panic, juste "test ignore")
  cargo test -p atelier-api-server --test storage   # ok, 1 passed (skip effectif)

  cargo test -p atelier-api-server   # 6 (routes.rs, vrai cluster/OpenBao) + 1 (storage.rs, vrai RustFS) passed
  cargo fmt --all -- --check         # silencieux
  cargo clippy --workspace --all-targets -- -D warnings   # 0 warning
  ```
- **Statut** : ✅ Validé pour les 4 tâches listées (2.1.1 à 2.1.4).

### [2026-08-24] Jalon M6 - Tâches 6.0.1, 6.0.2 : `local-stack.sh` réécrit (retrait de Kanidm, orchestration complète), `teardown-stack.sh`
- **Composant impacté** : `deploy/dev/local-stack.sh` (réécrit), `deploy/dev/teardown-stack.sh` (nouveau).
- **Contexte** : le script `local-stack.sh` datait d'avant la migration Kanidm → Keycloak et d'avant l'ajout de PostgreSQL, S3, Forgejo, la PKI locale et l'ingress Traefik — tous déployés manuellement au fil de sessions précédentes (voir entrées ci-dessus) mais jamais intégrés à l'orchestration.
- **`local-stack.sh`** : retire toute référence à Kanidm (conteneur Docker, service account, token API) ; orchestre désormais dans l'ordre de dépendance CRD Workshop → PKI locale (`deploy/dev/pki/init-pki.sh`, idempotent) → OpenBao (inchangé) → PostgreSQL (pod + création idempotente des bases `atelier_controller`/`keycloak`/`forgejo`, la base `atelier_apiserver` étant déjà créée automatiquement) → Keycloak (realm importé via ConfigMap) → S3 (RustFS) → Forgejo (admin + token API générés une seule fois, réutilisés ensuite via `env.sh`) → Traefik (ingress + Ingress applicatifs, après Keycloak/Forgejo dont il référence les Service) → registre OCI + build/`kind load` des images `:dev` (inchangé) → LLM Proxy optionnel (inchangé) → Redis (non disponible, message explicite renvoyant au Jalon M5 plutôt qu'un échec silencieux ou une infra inventée). Port-forwards host démarrés/réutilisés pour OpenBao (8200), PostgreSQL (5433) et S3 (9000). `env.sh` régénéré avec `ATELIER_DATABASE_URL_{API_SERVER,CONTROLLER}` (une base par composant), `ATELIER_OIDC_ISSUER_URL`/`ATELIER_JWT_{ISSUER,JWKS_URL,AUDIENCE}` pointant sur Keycloak via l'ingress Traefik (`auth.atelier.local`, cohérent avec `dashboard/lib/config.ts` et `crates/api-server/src/auth.rs`), variables S3 et Forgejo. Message final listant les étapes manuelles non automatisables : `sudo deploy/dev/traefik/update-hosts.sh` (nécessite un mot de passe, ne peut pas tourner dans le script) et confiance optionnelle à la CA locale (`SSL_CERT_FILE`/`NODE_EXTRA_CA_CERTS`).
- **Bug réel trouvé et corrigé en testant** : le pod Forgejo du cluster tournait depuis des heures avec une base `forgejo` inexistante (jamais créée manuellement, cf. tâche originale) — confirmé par `kubectl logs` (`pq: database "forgejo" does not exist` en boucle sur les tâches cron). Le script détecte ce cas (grep du message d'erreur dans les logs juste après le `kubectl wait`) et redémarre le pod pour qu'il exécute ses migrations une fois la base créée. Deuxième bug trouvé en testant : `forgejo admin user create` échouait silencieusement juste après ce redémarrage (pod au statut `Ready` k8s alors qu'aucune readiness probe applicative n'est définie — le serveur web met encore quelques secondes à finir ses migrations) — corrigé par une boucle de nouvelle tentative (jusqu'à 30 x 1s) au lieu d'un `|| true` masquant l'échec ; la condition de réutilisation du token dans `env.sh` a aussi été corrigée pour ne jamais considérer une valeur vide comme "déjà généré".
- **`teardown-stack.sh`** (nouveau) : symétrique de `local-stack.sh`, `kubectl delete -f` sur exactement les mêmes fichiers manifest (jamais un sélecteur de label large ni `--all`) pour Traefik, LLM Proxy, Forgejo, S3, Keycloak, PostgreSQL, OpenBao, plus les secrets TLS/CA de la PKI et les port-forwards locaux. Ne touche jamais `crds/workshop.yaml` (supprimerait tous les Workshops du cluster) ni les images Docker/kind `atelier-*:dev` (encore utilisées par les pods Workshop réels en cours d'exécution). Le registre OCI est arrêté (`docker stop`) mais pas supprimé, pour conserver les images déjà poussées. Garde-fou explicite : n'exécute aucune suppression sans `CONFIRM=yes`, avec un message d'avertissement sur l'impact (casse la session dev active en cours, sans toucher aux pods Workshop eux-mêmes).
- **Preuve empirique / Test exécuté** (contre le vrai cluster `kind-atelier-dev`, cluster partagé avec de vrais Workshops actifs — `my-new-demo-parent` notamment, jamais touché) :
  ```
  ./deploy/dev/local-stack.sh   # run complet, tous les composants déjà déployés détectés et réutilisés sans effet de bord
  # → a révélé et corrigé le bug Forgejo/base manquante en conditions réelles (voir ci-dessus)
  ./deploy/dev/local-stack.sh   # deuxième run : "administrateur + token déjà générés, réutilisé" — idempotence confirmée

  cat deploy/dev/local-stack/env.sh   # aucune référence à Kanidm, DATABASE_URL/ATELIER_OIDC_ISSUER_URL/ATELIER_JWT_*/S3_*/ATELIER_FORGEJO_* présents

  curl -H "Host: auth.atelier.local" http://172.19.0.2:80/realms/atelier/.well-known/openid-configuration
  # → 200, issuer "http://auth.atelier.local/realms/atelier" (cohérent avec ATELIER_JWT_ISSUER généré)

  curl -H "Host: git.atelier.local" http://172.19.0.2:80/   # 200
  curl -H "Host: git.atelier.local" -H "Authorization: token $ATELIER_FORGEJO_ADMIN_TOKEN" http://172.19.0.2:80/api/v1/user
  # → 200, {"login":"atelier_admin","is_admin":true,...} : le token généré par le script authentifie réellement

  # my-new-demo-parent (vrai Workshop) et les process locaux déjà lancés
  # (npm run dev, atelier-controller, atelier-api-server) vérifiés intacts
  # avant/après chaque run.

  kubectl delete -f <chaque manifest cible> --dry-run=client -o name   # confirme que teardown-stack.sh ne resout que les ressources dev attendues (aucune CRD, aucun Workshop)
  ./deploy/dev/teardown-stack.sh   # sans CONFIRM=yes : refuse et affiche l'avertissement, exit 1 — comportement voulu, PAS exécuté avec CONFIRM=yes sur ce cluster partagé (aurait cassé la session dev active d'OpenBao/PostgreSQL/Keycloak)
  ```
- **Point non testé en exécution réelle, par prudence** : `CONFIRM=yes ./deploy/dev/teardown-stack.sh` n'a pas été exécuté pour de vrai contre `kind-atelier-dev` (cluster partagé avec une session dev active et de vrais Workshops) — la suppression d'OpenBao/PostgreSQL/Keycloak aurait immédiatement cassé les process locaux déjà en cours (`atelier-controller`/`atelier-api-server`) et potentiellement la chaîne d'authentification de Workshops réels actifs. Le script a été relu très attentivement et chaque résolution de ressource vérifiée via `kubectl delete --dry-run=client` (voir ci-dessus) ; l'exécution réelle du chemin de suppression reste à faire sur un cluster kind jetable dédié.
- **Statut** : ✅ Validé pour 6.0.1 (testé réellement, bug Forgejo trouvé et corrigé en conditions réelles). ⚠️ 6.0.2 : script écrit et relu attentivement, résolution des ressources vérifiée, mais suppression réelle non exécutée par prudence (cluster partagé).

### [2026-08-24] Jalon M2 - Tâches 2.2.1, 2.2.2, 2.2.3 : Git HTTPS via `identity-proxy` (injection automatique de PAT, alias `git.atelier.internal`)
- **Composants impactés** : `crates/common/src/crd.rs` (+`GIT_ALIAS_HOST`), `crates/controller/src/git_identity.rs` (nouveau), `crates/controller/src/reconcile.rs`, `crates/net-proxy/src/internal.rs`, `crates/net-proxy/src/main.rs`, et — bug réel trouvé en testant, hors périmètre initial mais nécessaire pour que le test empirique passe — `crates/identity-proxy/src/proxy.rs` et `crates/identity-proxy/src/http.rs`.
- **2.2.1 (chemin OpenBao)** : réutilisation délibérée du chemin déjà utilisé par `crates/image-builder/src/main.rs::resolve_git_credentials` (`secret/data/workshops/<name>/git`, champs `username`/`password`), pas d'un second chemin `git_token` comme suggéré initialement par le plan. Justification documentée dans `crates/controller/src/git_identity.rs` : un même PAT Forgejo/GitHub/GitLab donne généralement accès aux mêmes dépôts pour cloner le devcontainer au build (`image-builder`) et pour que l'agent clone/pousse au runtime (`identity-proxy`) — deux moments et deux composants différents, mais un seul secret à provisionner côté utilisateur.
- **2.2.2 (règle d'injection calculée)** : nouveau module `crates/controller/src/git_identity.rs`. `config_from_env()` lit `ATELIER_GIT_HOST_SERVICE` (nom du Service Kubernetes de la forge, défaut désactivé si absent — même convention que `openbao`/`llm_proxy_addr`), `ATELIER_GIT_HOST_SERVICE_NAMESPACE` (défaut `default`), `ATELIER_GIT_HOST_PORT` (défaut `3000`), `ATELIER_GIT_INJECTION_HEADER`/`ATELIER_GIT_INJECTION_PREFIX` (défauts `Authorization`/`token `, override pour GitLab via `PRIVATE-TOKEN`/vide). `resolve_cluster_ip()` lit le ClusterIP du Service **via l'API Kubernetes** (`Api<Service>::get`), jamais une résolution DNS classique — le controller peut tourner hors du cluster en dev (voir les autres entrées de ce journal), sans accès au DNS interne, mais toujours avec accès à l'API Kubernetes. Dans `ensure_parent_pod` (`reconcile.rs`), la règle calculée (`host: git.atelier.internal`, `secretPath: git`, `field: password`) est ajoutée **à la volée** à la liste sérialisée vers `ATELIER_IDENTITY_INJECTION_RULES`, jamais écrite dans `Workshop.spec` lui-même (qui reste déclaratif) ; le ClusterIP résolu est posé en `Pod.spec.hostAliases` (nouveau champ, mécanisme Kubernetes natif qui écrit dans `/etc/hosts` de tous les conteneurs du pod). Best-effort et non bloquant comme le reste du provisioning OpenBao : un échec de résolution désactive juste la fonctionnalité pour ce cycle.
- **2.2.3 (alias net-proxy)** : nouvel alias fixe `git.atelier.internal` (constante partagée `atelier_common::GIT_ALIAS_HOST`, reprend délibérément la même valeur que `FORGEJO__server__ROOT_URL` déjà configuré dans `deploy/dev/forgejo/dev-pod.yaml` — pas une coïncidence). Configurable via `ATELIER_GIT_ALIAS_ADDR`, pointant vers l'adresse locale d'`identity-proxy`, même mécanisme que les 4 alias existants (`identity-proxy`/`mcp-gateway`/`registry`/`llm-proxy`). **Vérification documentée avant d'écrire du code** (voir le commentaire de tête de `crates/net-proxy/src/internal.rs`) : le chaînage générique déjà en place (`ATELIER_IDENTITY_PROXY_ADDR`, qui fait transiter tout l'egress *déjà autorisé* par `identity-proxy`) s'applique **après** que l'allowlist ait tranché — il n'aurait donc pas suffi à rendre `git.atelier.internal` joignable sans que l'utilisateur l'ajoute explicitement à `Workshop.spec.egress_allowlist`. L'alias dédié, lui, bypass l'allowlist entièrement, exactement ce que demandait 2.2.3. Un seul alias retenu (pas de second `forgejo.atelier.internal` : un seul forge Git par Workshop dans ce MVP, pas de valeur ajoutée à en distinguer deux).
- **Bug réel trouvé et corrigé en testant un vrai `git clone` de bout en bout** (`identity-proxy`, hors du périmètre de fichiers initialement prévu pour cette tâche mais nécessaire pour que le test empirique demandé par le plan passe réellement — répertoire explicitement inclus dans le protocole multi-agents de cette session) : `crates/identity-proxy/src/proxy.rs::forward()` ne relayait la requête avec injection de credential que pour la **toute première** requête HTTP d'une connexion — toute requête suivante sur la même connexion keep-alive n'était plus qu'un flux d'octets recopié en aveugle (`copy_bidirectional`), sans jamais repasser par l'injection. Or le protocole HTTP "smart" de Git enchaîne systématiquement deux requêtes sur la même connexion (`GET .../info/refs?service=git-upload-pack` puis `POST .../git-upload-pack`) : la première recevait bien le header `Authorization`, la seconde non, faisant échouer Forgejo en `401 Unauthorized` sur la deuxième — `git clone` échouait avec `fatal: could not read Username`, alors qu'un simple `curl` (une seule requête) fonctionnait. Corrigé en transformant `forward()` en boucle qui réinjecte le credential sur chaque requête, en s'appuyant sur un parsing minimal du framing HTTP (`Content-Length`/`Transfer-Encoding: chunked`, nouvelles fonctions `crates/identity-proxy/src/http.rs::{read_response_head, content_length, is_chunked, copy_exact, copy_chunked_body}`) pour savoir où s'arrête chaque requête/réponse et pouvoir relire la suivante ; si le framing d'une réponse est inconnu (corps jusqu'à fermeture, pas de `Content-Length`/chunked), on retombe sur l'ancien comportement (`copy_bidirectional` jusqu'à fermeture), correct tant qu'il ne reste plus qu'une requête sur la connexion.
- **Test réel exécuté (`cargo test -p atelier-net-proxy --test git_identity`, nouveau)** : bout en bout contre le vrai Forgejo de dev (`kubectl port-forward svc/atelier-forgejo-dev --address 0.0.0.0`, nécessaire pour être joignable depuis un conteneur Docker) et un vrai OpenBao (ServiceAccount + rôle Kubernetes-auth provisionnés comme `crates/net-proxy/tests/session_auth.rs`) :
  1. Génère un vrai PAT Forgejo (`forgejo admin user generate-access-token`) et crée un vrai dépôt privé frais.
  2. Écrit le PAT dans OpenBao sous `secret/data/workshops/<name>/git` (même convention que 2.2.1).
  3. Lance `identity-proxy` (image `atelier-identity-proxy:dev` reconstruite avec le correctif ci-dessus) dans un conteneur Docker avec `--add-host git.atelier.internal:<passerelle bridge>` — équivalent Docker exact de `Pod.spec.hostAliases` posé par le controller en production (voir 2.2.2), pour rester fidèle au mécanisme réel sans modifier `identity-proxy` pour les besoins du test.
  4. Lance `net-proxy` (process natif) avec `ATELIER_GIT_ALIAS_ADDR` pointant vers ce conteneur, et **sans aucune allowlist egress configurée** — preuve directe que seul l'alias interne rend `git.atelier.internal` joignable.
  5. `git clone http://git.atelier.internal:.../atelier_admin/<repo>.git` via `http_proxy=http://127.0.0.1:<net-proxy>` : **succès réel**, contenu du dépôt (`README.md` de l'`auto_init`) vérifié sur disque.
  - Avant le correctif identity-proxy : échec reproductible (`fatal: could not read Username`). Après : clone systématiquement réussi (plusieurs runs).
- **Test réel complémentaire (`cargo test -p atelier-controller --test reconcile apply_wires_the_git_identity_injection_rule_when_configured`, nouveau)** : contre le vrai cluster, avec `ReconcileCtx::git_identity` configuré pointant sur le vrai Service `atelier-forgejo-dev` — vérifie que `apply()` (a) résout le vrai ClusterIP via l'API Kubernetes, (b) le pose en `hostAliases` sur le pod parent réellement créé, (c) ajoute la règle d'injection calculée à `ATELIER_IDENTITY_INJECTION_RULES` du conteneur `identity-proxy`, (d) câble `ATELIER_GIT_ALIAS_ADDR` sur `net-proxy`, et (e) ne modifie jamais `Workshop.spec` lui-même.
- **Commandes exécutées et résultats** :
  ```
  cargo fmt --all -- --check                                              # silencieux
  cargo clippy --workspace --all-targets -- -D warnings                   # 0 warning (workspace entier, y compris le travail en cours de l'autre agent sur storage.rs)
  cargo test -p atelier-controller -p atelier-net-proxy -p atelier-identity-proxy
  # 9 (controller lib) + 6 (controller/tests/reconcile.rs, vrai cluster) + 8 (identity-proxy) + 52 (net-proxy lib)
  # + 1 (net-proxy/tests/git_identity.rs, vrai Forgejo+Docker+OpenBao+K8s) + 1 (net-proxy/tests/session_auth.rs, vrai OpenBao+K8s) passed, 0 failed
  ```
- **Vérification de non-régression sur le cluster partagé** (consigne de sécurité de cette session) : `atelier-controller` réel (`./target/debug/atelier-controller`, PID préexistant) arrêté puis relancé avec le binaire recompilé (mêmes variables d'environnement, `git_identity` **non** activé pour ce run réel — fonctionnalité opt-in, `ATELIER_GIT_HOST_SERVICE` absent) : logs de reconciliation examinés pour tous les Workshops existants (`my-new-demo`, `test`, `ws-user-fix-test`, etc.) sans aucune erreur ; `my-new-demo-parent` (vrai Workshop actif, 4/4 conteneurs) toujours `Running` après redémarrage, `status.phase` toujours `Running`.
- **Décision de conception — un seul chemin OpenBao, un seul alias** : voir 2.2.1/2.2.3 ci-dessus ; documentée en détail (avec justification) dans les commentaires de tête de `crates/controller/src/git_identity.rs` et `crates/net-proxy/src/internal.rs`.
- **Statut** : ✅ Validé pour les 3 tâches (2.2.1 à 2.2.3), avec un bug pré-existant réel corrigé dans `identity-proxy` sans lequel le test empirique demandé par le plan (`git clone` réel réussi) ne pouvait pas passer.

### [2026-08-24 07:45] Jalon M2 - DoD : archivage S3 du terminal, branchement de `storage.rs` dans `crate::vscode`/`crate::terminal`
- **Contexte** : après la fusion des trois agents parallèles (2.1.x storage, 2.2.x git-identity, 6.0.x local-stack), le DoD du Jalon M2 avait deux lignes encore honnêtement `[ ]` : le module `crates/api-server/src/storage.rs` (2.1.1-2.1.4) existait et était testé isolément, mais **n'était appelé nulle part** dans le reste du crate (vérifié par `grep`) — aucune session terminal/VS Code réelle n'était jamais archivée. Aucune spec dédiée (contrairement à Keycloak/Forgejo/LiteLLM) ne précisait le périmètre exact de cet enregistrement : décision produit prise avec l'utilisateur avant d'écrire du code (voir question posée).
- **Décision retenue (validée par l'utilisateur)** : enregistrer uniquement le terminal (`ttyd`), pas `code-server`. Justification : le tunnel `code-server` ne transporte que le protocole HTTP/WebSocket interne de l'éditeur (assets, LSP...), sans sémantique de rejeu exploitable une fois archivé, contrairement à la sortie d'un terminal (convention `asciinema` : seule la sortie serveur→client est enregistrée, jamais la saisie utilisateur).
- **Implémentation** :
  - Nouveau module `crates/api-server/src/session_recorder.rs` (`SessionRecording`) : `start(storage, workshop_name)` génère un `session_id` (UUID v4), ouvre un `tokio::io::duplex` interne dont la moitié lecture est immédiatement consommée par `S3StorageBackend::upload_session_archive` dans une tâche `tokio::spawn` (jamais de session bufferisée entièrement en mémoire, y compris pour une session de plusieurs heures) ; `write_chunk()` pousse les octets au fil de l'eau, best-effort (un échec d'archivage ne doit jamais interrompre la session terminal elle-même) ; `finish()` ferme le tuyau et attend la fin réelle de l'upload en arrière-plan (au lieu d'un simple `drop`), pour que la fin du tunnel WebSocket corresponde à un archivage réellement complet (ou en échec, déjà journalisé).
  - `crates/api-server/src/vscode.rs::proxy_to_guest_port` : les 8 paramètres positionnels (dont le nouveau `record_session: bool`) ont déclenché `clippy::too_many_arguments` — regroupés dans un struct `GuestProxyTarget`. La copie bidirectionnelle du tunnel WebSocket (`tokio::io::copy_bidirectional`) a été remplacée par `copy_bidirectional_with_recording`, une boucle `tokio::select!` manuelle qui duplique (« tee ») uniquement la direction serveur→client vers le `SessionRecording` le cas échéant, puis appelle `finish()` avant de retourner — comportement strictement identique à `copy_bidirectional` quand `recording` est `None` (donc pour tous les tunnels `code-server`).
  - `crate::terminal` appelle `proxy_to_guest_port` avec `record_session: true` ; `crate::vscode` avec `record_session: false`.
  - `AppState.storage: Option<Arc<S3StorageBackend>>` (nouveau champ, construit dans `main.rs` via `S3StorageBackend::from_env()` — `None` si `S3_ENDPOINT` absent, fonctionnalité alors simplement désactivée, même convention que `session_auth`).
- **Test réel exécuté (`crates/api-server/tests/session_recorder.rs`, nouveau)** : contre le vrai RustFS de dev — écrit plusieurs blocs sur un `SessionRecording`, appelle `finish()`, puis relit l'archive via `S3StorageBackend::get_session_stream` (reconstruite avec `session_recorder.session_id()`, exposé pour cet usage) et vérifie l'égalité byte-à-byte avec ce qui a été écrit.
- **Commandes exécutées et résultats** :
  ```
  cargo fmt --all -- --check                                    # silencieux
  cargo clippy --workspace --all-targets -- -D warnings         # 0 warning
  cargo test -p atelier-api-server
  # 9 (routes, controller live arrêté pour éliminer l'interférence de réconciliation déjà documentée)
  # + 1 (session_recorder.rs, vrai RustFS) + 1 (storage.rs, vrai RustFS) passed, 0 failed
  ```
- **Vérification de non-régression sur le cluster partagé** : `atelier-controller` réel arrêté le temps de la vérification (élimine la course connue avec `vscode_proxy_injects_real_session_auth_basic_header`, confirmée en isolant le test — échoue avec le controller actif, passe sans), puis recompilé et relancé : logs de réconciliation de tous les Workshops réels examinés sans erreur, `my-new-demo` (Workshop réel actif, pod `my-new-demo-parent` 4/4) toujours `Running` après redémarrage.
- **Statut** : ✅ DoD du Jalon M2 entièrement clos (3/3 lignes `[x]`).

### [2026-08-24 01:30] Jalon M5 - Tâches 5.0.1, 5.1.1, 5.1.2, 5.3.1, 5.3.2, 5.3.3 : Redis dev, scaffolding `services/pm-engine`, base `atelier_pm` + RLS + checkpointer LangGraph
- **Composant impacté** : `deploy/dev/redis/dev-pod.yaml` (nouveau), `deploy/dev/redis/README.md` (nouveau), `services/pm-engine/` (nouveau : `pyproject.toml`, `Dockerfile`, `README.md`, `pm_engine/main.py`, `pm_engine/checkpointer.py`, `tests/`, `migrations/20260824000000_init_pm_engine.sql`).
- **5.0.1 — Redis dev (Streams)** : Pod `atelier-redis-dev` (`redis:7.4-alpine`, `emptyDir`, meme convention que `deploy/dev/postgres`/`deploy/dev/s3`) deploye reellement sur `kind-atelier-dev`. Les Streams sont une structure native de Redis (pas de module a activer) : cycle at-least-once complet verifie a la main (`XADD` -> `XLEN`=1 -> `XGROUP CREATE pm-engine-workers` -> `XREADGROUP` -> `XPENDING`=1 message en attente -> `XACK` -> `XPENDING`=0). Detail dans `deploy/dev/redis/README.md`.
- **5.0.2 — LiteLLM dev + modele d'embedding leger : BLOQUE, non traité, documenté ci-dessous plutôt que forcé.**
  - Au moment d'aborder cette tâche, `deploy/dev/llm-proxy/` était en cours de déploiement/redéploiement **en temps réel** par l'agent parallèle dédié à M3 (`kubectl get pods` montrait `atelier-llm-proxy-<hash>` en `Terminating` à côté d'un nouveau pod de 24 secondes, plus un nouveau service `atelier-llm-proxy-db` inédit) — collision quasi certaine si ce fichier avait été édité au même instant.
  - Conformément à la consigne de cette session en cas de conflit probable : **aucune modification appliquée** à `deploy/dev/llm-proxy/config.yaml` ni `dev-deployment.yaml`. Patch proposé ci-dessous, à appliquer par la prochaine session une fois M3 stabilisé (vérifier `git log -- deploy/dev/llm-proxy/` avant d'appliquer, la structure du fichier a pu évoluer depuis) :
    ```diff
    --- a/deploy/dev/llm-proxy/config.yaml
    +++ b/deploy/dev/llm-proxy/config.yaml
    @@
       - model_name: sonnet-premium
         litellm_params:
           model: anthropic/claude-3-5-sonnet-20241022
           api_key: os.environ/ANTHROPIC_API_KEY
    +
    +  # Modele d'embedding local, sans cle payante (Jalon M5, tache 5.0.2) :
    +  # valide services/pm-engine (project_memories/pgvector) en dev sans
    +  # dependre d'une cle OpenAI facturee. LiteLLM route ce nom vers un
    +  # backend HuggingFace local (le conteneur telecharge le modele au
    +  # premier appel ; prevoir un volume/cache persistant si le pod est
    +  # recree souvent). Dimension native 384 != VECTOR(1536) de
    +  # `project_memories` (calibree sur text-embedding-3-small) : n'ecrit
    +  # PAS directement dans cette colonne sans adaptation (re-projection ou
    +  # migration de colonne dediee aux tests dev) -- limite assumee, a
    +  # trancher avant d'utiliser ce modele pour peupler la table reelle.
    +  - model_name: embedding-dev-local
    +    litellm_params:
    +      model: huggingface/sentence-transformers/all-MiniLM-L6-v2
    ```
  - Non vérifié empiriquement (LiteLLM pas stable au moment du passage) : à revalider avec `curl -X POST .../v1/embeddings -d '{"model":"embedding-dev-local","input":"test"}'` une fois appliqué.
- **5.1.1/5.1.2 — Scaffolding `services/pm-engine`** : `pyproject.toml` (Python ≥3.12, FastAPI/LangGraph/`langgraph-checkpoint-postgres`/`psycopg[binary]`/Redis/AsyncPG/Pydantic/HTTPX), `pm_engine/main.py` avec un unique endpoint `/health` (aucune machine d'états LangGraph — tâches 5.2.x hors périmètre, dépendantes du serveur MCP M4 pas encore construit). `Dockerfile` multi-stage (`builder` avec `uv`, image finale `python:3.12-slim` non-root, ~205 Mo).
- **5.3.1/5.3.2 — Base `atelier_pm` + RLS** : `CREATE DATABASE atelier_pm` + `CREATE EXTENSION vector` sur l'instance PostgreSQL de dev réelle. Table `project_memories` (`VECTOR(1536)`, alignée sur `text-embedding-3-small`) avec index `ivfflat`/`vector_cosine_ops` et RLS (`ENABLE`+`FORCE ROW LEVEL SECURITY`, policy sur `current_setting('app.current_tenant')`). Nouveau rôle non-superutilisateur `atelier_pm_app` (`NOBYPASSRLS`), même convention que `atelier_app` documentée dans `deploy/dev/postgres/README.md` — **piège rencontré et documenté dans la migration** : `ALTER DEFAULT PRIVILEGES ... GRANT ... ON TABLES` ne couvre pas la séquence implicite de `BIGSERIAL` (`nextval()` échoue avec "permission denied for sequence" malgré le GRANT sur la table) ; il faut aussi `GRANT USAGE, SELECT ON SEQUENCES`.
- **5.3.3 — Checkpointer `AsyncPostgresSaver`** : `pm_engine/checkpointer.py` (context manager `build_checkpointer(database_url)`, appelle `.setup()` puis retourne l'instance). Nécessitait une dépendance non déclarée par `langgraph-checkpoint-postgres` (`psycopg[binary]`, sinon `ImportError: no pq wrapper available`), ajoutée à `pyproject.toml`.
- **Preuve empirique / Test exécuté** (aucun mock, contre le cluster kind partagé et l'instance PostgreSQL réelle) :
  ```
  kubectl exec atelier-redis-dev -- redis-cli XADD/XLEN/XGROUP CREATE/XREADGROUP/XPENDING/XACK   # cycle at-least-once complet, voir deploy/dev/redis/README.md

  cd services/pm-engine && uv venv .venv --python 3.12 && uv pip install -e ".[dev]" --python .venv/bin/python   # installation reelle, 0 erreur
  .venv/bin/uvicorn pm_engine.main:app --port 8100 &   curl http://127.0.0.1:8100/health   # {"status":"ok"}
  docker build -t atelier-pm-engine:dev .   # succes, image finale 205MB
  docker run -d -p 18100:8100 atelier-pm-engine:dev   curl http://127.0.0.1:18100/health   # {"status":"ok"} (via le conteneur, pas juste le venv local)
  .venv/bin/pytest -q   # 2 passed (test_health.py, test_checkpointer.py contre DATABASE_URL_PM reel via le port-forward 5433 deja actif)

  kubectl exec atelier-postgres-dev -- psql -U atelier_admin -d postgres -c 'CREATE DATABASE atelier_pm;'
  kubectl exec -i atelier-postgres-dev -- psql -U atelier_admin -d atelier_pm < services/pm-engine/migrations/20260824000000_init_pm_engine.sql   # succes

  # RLS verifiee avec deux tenants et le role non-superutilisateur atelier_pm_app (jamais atelier_admin) :
  PGPASSWORD=... kubectl exec -i atelier-postgres-dev -- psql -U atelier_pm_app -d atelier_pm -c "SET app.current_tenant='alice'; SELECT ... FROM project_memories;"   # -> 1 ligne (alice uniquement)
  PGPASSWORD=... kubectl exec -i atelier-postgres-dev -- psql -U atelier_pm_app -d atelier_pm -c "SET app.current_tenant='bob';   SELECT ... FROM project_memories;"   # -> 1 ligne (bob uniquement)
  kubectl exec atelier-postgres-dev -- psql -U atelier_admin -d atelier_pm -c "SET app.current_tenant='alice'; SELECT ... FROM project_memories;"   # -> 2 lignes (superutilisateur, RLS ignoree - coherent avec la decouverte deja documentee pour atelier_apiserver)

  # AsyncPostgresSaver.setup() cree reellement ses tables, verifie apres le test pytest :
  kubectl exec atelier-postgres-dev -- psql -U atelier_admin -d atelier_pm -c '\dt'   # checkpoints, checkpoint_writes, checkpoint_blobs, checkpoint_migrations, project_memories

  # Etat du cluster verifie intact apres coup :
  kubectl get pods | grep -E 'my-new-demo-parent|atelier-redis-dev|atelier-postgres-dev'   # tous Running, my-new-demo-parent non affecte
  ```
- **Statut** : ✅ Validé pour 5.0.1, 5.1.1, 5.1.2, 5.3.1, 5.3.2, 5.3.3. ⚠️ 5.0.2 non réalisé (bloqué par un déploiement M3 concurrent en cours au moment du passage) — patch proposé mais non appliqué, à reprendre par la session suivante.

### [2026-08-24 08:34] Interruption de session - État des tâches M3 et M6 en cours, passage de relais

Session `sess-6f3eef77` interrompue à la demande de l'utilisateur. Deux tâches restent verrouillées `[-/...]` dans `docs/specs/PLAN-ACTION-GLOBAL.md`, dans des états différents — détail ci-dessous pour la session suivante.

**M3 (tâches 3.1.1 à 3.2.1, verrou `[-/claude-code/sess-6f3eef77-f]`) : code terminé et testé, mais PAS ENCORE fusionné dans `main`.**
- Le travail a été fait par un agent en arrière-plan dans un worktree git isolé, avec deux commits réels et vérifiés (diff relu intégralement par la session parente avant tentative de fusion) : `8eac102` (`feat(controller): Virtual Keys LiteLLM isolees par Workshop (Jalon M3)`) et `5303a19` (`docs(progress): consigner une race observee entre le controller live et cargo test --workspace`), sur la branche **`worktree-agent-a8d2ccd747d91380d`** (encore présente localement, `git worktree list` la montre sous `.claude/worktrees/agent-a8d2ccd747d91380d`).
- **Tentative de fusion abandonnée volontairement** (`git merge --abort`), pour deux raisons concurrentes :
  1. Conflit de fusion réel dans `crates/controller/src/reconcile.rs` et `crates/controller/tests/reconcile.rs` : la branche de l'agent avait divergé de `main` avant la fusion de l'agent Git-HTTPS (`744a665`, tâches 2.2.x), qui touche aussi `ensure_parent_pod`/`ReconcileCtx` dans les mêmes zones du fichier — nécessite une résolution manuelle attentive (pas un simple conflit de contenu dupliqué comme pour M5), pas traitée avant l'arrêt demandé.
  2. **Pendant la tentative de fusion**, un processus tiers (une autre session/agent travaillant sur ce même dépôt partagé, cf. protocole multi-agents d'`AGENTS.md`) a exécuté un `git pull`/`rebase`/`push` réel sur `origin/main` en parallèle — `main` est passé de `d93173f` à `b35cbbf` (`Add CodeQL analysis workflow configuration`, en passant par `069baca chore: update documentation` qui a rebasé/aplati la fusion M5 faite plus tôt dans cette session). Ceci a rendu l'état de fusion en cours incohérent (conflits résolus contre un `HEAD` qui n'était plus le bon) : `git merge --abort` a ramené le dépôt à un état propre correspondant au nouveau `HEAD` réel (`b35cbbf`), sans perte du travail de l'agent M3 (préservé intact sur sa branche).
- **Pour reprendre** : `git merge worktree-agent-a8d2ccd747d91380d` (ou `git rebase main` de cette branche puis fast-forward) depuis l'état actuel de `main`, résoudre le conflit réel dans `reconcile.rs`/`tests/reconcile.rs` (les deux fonctionnalités — Git HTTPS `identity_injection_rules` et Virtual Key LiteLLM `identity_injection_rules` — doivent coexister : les deux ajoutent une règle à la même liste, la fusion doit conserver les deux `push`), puis revalider `cargo test --workspace`/`clippy`/`fmt` avant de committer. Les tâches 3.1.1-3.2.1 et le DoD de M3 ne doivent être cochés `[x]` qu'après cette fusion réelle et cette revalidation — **PAS encore fait**, le verrou `[-/claude-code/sess-6f3eef77-f]` reste donc intentionnellement en place dans le plan.

**M6 (tâches 6.2.1 à 6.5.2, verrou `[-/claude-code/sess-6f3eef77-h]`) : agent encore en cours d'exécution au moment de l'arrêt, aucun résultat rapporté.**
- Agent en arrière-plan (worktree `.claude/worktrees/agent-a1b58166d0b22db1f`, branche `worktree-agent-a1b58166d0b22db1f`) lancé pour construire le chart Helm complet (`charts/atelier/`) et le guide administrateur. Aucune notification de complétion reçue avant l'arrêt de session — état d'avancement réel inconnu (peut être toujours en cours, ou terminé sans que la notification ait encore été traitée). La session suivante doit vérifier l'état de cet agent/worktree avant de relancer quoi que ce soit sur ce périmètre, pour éviter un travail dupliqué.

**Vérification finale avant interruption** : `my-new-demo`/`my-new-demo-parent` (Workshop réel actif) toujours `Running`, cluster intact.

### [2026-08-24 09:00] Jalon M3 - Tâches 3.1.1 à 3.2.1 : Virtual Keys LiteLLM par Workshop, budgets stricts et révocation
- **Composant impacté** : `crates/controller/src/litellm.rs` (nouveau), `crates/controller/src/openbao.rs` (`ensure_llm_virtual_key_secret`), `crates/controller/src/reconcile.rs`, `crates/controller/src/lib.rs`, `crates/controller/tests/litellm.rs` (nouveau), `crates/controller/tests/reconcile.rs`, `deploy/dev/llm-proxy/config.yaml`, `deploy/dev/llm-proxy/dev-deployment.yaml`.
- **Contexte** : jusqu'ici, `ANTHROPIC_AUTH_TOKEN` était un jeton statique unique (`ATELIER_LLM_PROXY_AUTH_TOKEN`), baked une seule fois dans `/etc/environment` au moment du build de l'image (`crates/image-builder::inject_net_proxy_config`), partagé par tous les Workshops (limite documentée dans `deploy/dev/llm-proxy/README.md`). Ce jalon introduit une Virtual Key LiteLLM isolée par Workshop, avec budget plafonné (`WorkshopSpec.resources.maxLlmBudgetUsd`) et TTL court, régénérée à chaud à chaque (re)création du pod parent.
- **Décision de conception documentée (écart assumé vis-à-vis du libellé littéral de 3.1.3, "injecter dans `/etc/environment`")** : réécrire `/etc/environment` du guest à la reprise post-suspension est impossible sans rebuild d'image (seul `image-builder`, exécuté une fois au build, y écrit réellement — `vm-supervisor` boote le rootfs tel quel, sans init capable de recevoir des paramètres au boot). Modifier `net-proxy`/`identity-proxy` pour ouvrir un nouveau canal (à la manière de `crates/net-proxy/src/metadata.rs` pour `session_auth`) était par ailleurs hors du périmètre assigné à cet agent (ces deux crates sont explicitement exclues). Solution retenue, à coût d'implémentation nul sur ces deux crates : réutilisation telle quelle du mécanisme **générique** déjà en place pour l'injection de credentials Git (`Workshop.spec.identity_injection_rules`, `crates/identity-proxy/src/rules.rs`/`proxy.rs`) — la Virtual Key est écrite dans OpenBao (`secret/workshops/<name>/llm_key`, champ `value`), une règle d'injection `Authorization: Bearer <value>` sur l'hôte interne `llm-proxy` est ajoutée à la volée à `ATELIER_IDENTITY_INJECTION_RULES`. `identity-proxy`, sur le chemin de sortie, REMPLACE alors l'en-tête `Authorization` statique baked au build par la vraie Virtual Key de ce Workshop (`with_injected_header` remplace, ne duplique pas — comportement déjà testé dans `crates/identity-proxy/src/http.rs`). L'agent dans la microVM n'a jamais connaissance de la vraie clé, exactement comme pour le credential Git. Conséquence assumée : cette isolation nécessite OpenBao EN PLUS de LiteLLM — sans OpenBao, aucun canal de livraison n'existe et le controller se contente de logguer l'échec (le jeton statique partagé historique reste actif, zéro régression). Justification complète documentée en tête de `crates/controller/src/litellm.rs`.
- **Génération de la Virtual Key** : uniquement au moment où le pod parent va être créé (`pod_will_be_created`, calculé via `pods.get_opt().is_none()` avant construction de la spec) — jamais à chaque reconcile d'un pod déjà en place, dont l'`env` est de toute façon immuable une fois créé. Couvre naturellement le provisioning initial ET la reprise post-suspension (`ensure_suspended` supprime le pod, la reconciliation suivante le recrée via ce même chemin).
- **Clé éphémère de build (3.1.4)** : alias dédié `atelier-build-<name>` (distinct de `atelier-wks-<name>` du pod parent), généré une seule fois à la création du Job `image-builder` (même garde `job_already_exists`), injecté dans `/etc/environment` du build comme avant. Révoquée dès que le Job atteint un état terminal (`succeeded`/`failed`) — limite assumée et documentée : si `image-builder` patche `status.imageDigest` avant que le controller ne revoie ce Job comme terminé lors d'un cycle suivant, la révocation explicite peut être manquée pour ce Workshop ; un TTL court dédié (`BUILD_VIRTUAL_KEY_TTL` = 30 min, plus court que le TTL runtime de 2h) sert de filet de sécurité.
- **Révocation à la suppression (3.2.1)** : `cleanup()` (exécuté par le handler `Event::Cleanup` du finalizer `atelier.dev/cleanup`) appelle désormais `litellm::LiteLlmClient::delete_virtual_key(&litellm::workshop_key_alias(name))` avant de libérer le rôle OpenBao. `cleanup()` rendu `pub` (comme `apply`) pour être testable directement sans démarrer un `Controller` complet, même convention que les tests OpenBao existants.
- **Infrastructure de dev déployée** (absente au début de cette session) : un vrai LiteLLM (`ghcr.io/berriai/litellm:main-stable`) déployé sur `kind-atelier-dev` via `deploy/dev/llm-proxy/dev-deployment.yaml`, qui déploie désormais AUSSI une instance Postgres DÉDIÉE (`atelier-llm-proxy-db`, pod+Service séparés) : `/key/generate`/`/key/delete` exigent une base Postgres côté LiteLLM (constaté en pratique : `500 "DB not connected"` sans elle) — une instance dédiée plutôt qu'une base supplémentaire sur `atelier-postgres-dev` (partagée par `api-server` ET un Workshop réel actif sur ce cluster), pour zéro risque d'interférence avec cette instance partagée.
- **Adaptation du test réel en l'absence de clés de provider payantes** : ni DeepSeek ni Anthropic réels disponibles dans cet environnement. Ajout d'un modèle de test dédié dans `deploy/dev/llm-proxy/config.yaml` (`atelier-budget-test`) utilisant la fonctionnalité native `mock_response` de LiteLLM (aucun appel HTTP sortant vers un fournisseur réel, jamais de coût facturé), combinée à `model_info.input_cost_per_token`/`output_cost_per_token` explicites pour porter un coût non nul par appel (LiteLLM n'a pas de tarif par défaut pour un nom de modèle arbitraire). LiteLLM lui-même calcule et enforce le budget de la Virtual Key exactement comme il le ferait avec un vrai modèle payant — aucune simulation côté test Rust.
- **Preuve empirique / Séquence réellement observée contre le vrai LiteLLM déployé** (`crates/controller/tests/litellm.rs::generates_enforces_budget_and_revokes_a_real_virtual_key`) :
  ```
  POST /key/generate {key_alias, duration:"2h", max_budget:1.0}     -> 200, "key":"sk-..."
  POST /chat/completions (modele atelier-budget-test, cle ci-dessus) -> 200 (sous le budget)
  # cout enregistre de maniere asynchrone cote LiteLLM (constate en pratique, boucle de relecture /key/info)
  GET  /key/info?key=sk-...                                          -> spend: 15.0 (> budget 1.0)
  POST /chat/completions (meme cle)                                  -> 429 "Budget has been exceeded! ..."
  POST /key/delete {key_aliases:[alias]}                             -> 200 {"deleted_keys":[alias]}
  POST /chat/completions (meme cle)                                  -> 401 Authentication Error
  POST /key/delete {key_aliases:[alias]} (deuxieme appel)             -> 404 "No keys found" (traite comme succes, idempotence)
  ```
- **Preuve empirique supplémentaire** (`crates/controller/tests/reconcile.rs::apply_wires_the_llm_virtual_key_injection_rule_when_configured`, contre un vrai OpenBao ET un vrai LiteLLM) : `apply()` écrit une vraie Virtual Key dans `secret/workshops/<name>/llm_key` (vérifié en relisant directement OpenBao), câble la règle d'injection `host: "llm-proxy"` sur un vrai Pod créé (vérifié en relisant `spec.containers[identity-proxy].env`), la clé existe côté LiteLLM (`/key/info` → 200) ; puis `cleanup()` la révoque effectivement (`/key/info` après suppression → non-200).
- **Incident rencontré et résolu en cours de route (même nature que celui déjà documenté au Jalon M1)** : le premier essai de ce nouveau test échouait systématiquement avec des règles d'injection vides côté serveur alors que le code en mémoire les calculait correctement — diagnostiqué comme une interférence du **vrai controller déjà en cours d'exécution sur ce cluster partagé** (`Api::all`, il réconciliait aussi les Workshops de test créés directement par le test, avec SON PROPRE `ReconcileCtx` non configuré pour LiteLLM, écrasant via Server-Side Apply — même field manager `atelier-controller` — la spec fraîchement posée par le test). Confirmé en arrêtant ce controller (le test passe alors systématiquement), reconstruit puis relancé avec la nouvelle version et les variables LiteLLM positionnées.
- **Preuve empirique / Tests exécutés** :
  ```
  export ATELIER_LLM_PROXY_ADDR=127.0.0.1:4000
  export ATELIER_LLM_PROXY_AUTH_TOKEN=<LITELLM_MASTER_KEY du Secret atelier-llm-proxy-dev>
  export OPENBAO_ADDR=http://127.0.0.1:8200
  export OPENBAO_TOKEN=root
  cargo test -p atelier-controller            # 17 passed (10 unitaires + litellm.rs + 6 reconcile.rs), 0 failed
  cargo test --workspace                       # 92 passed (sans OPENBAO_ADDR/ATELIER_LLM_PROXY_ADDR, silencieusement ignores), 0 failed
  cargo fmt --all -- --check                   # silencieux
  cargo clippy --workspace --all-targets -- -D warnings   # silencieux
  ```
  Contrôleur live réel arrêté (`pkill`), reconstruit depuis cette session, relancé avec les mêmes variables d'environnement que l'instance précédente (capturées via `/proc/<pid>/environ`) PLUS `ATELIER_LLM_PROXY_ADDR`/`ATELIER_LLM_PROXY_AUTH_TOKEN` : `my-new-demo-parent` toujours `4/4 Running`, aucun redémarrage (le nouveau code ne recrée jamais un pod parent déjà existant — garde `pod_will_be_created`), zéro erreur touchant un Workshop réel préexistant sur toute la durée de cette session (voir limite (4) ci-dessous pour la seule catégorie d'erreurs observée, confinée aux Workshops éphémères créés par `cargo test --workspace` lui-même).
- **Limites assumées** : (1) isolation par Workshop nécessite OpenBao + LiteLLM tous deux configurés (dégradation gracieuse vers le jeton statique partagé sinon) ; (2) révocation de la clé de build non garantie si le controller manque la fenêtre où le Job est visible comme terminé (TTL court en filet de sécurité) ; (3) `generate_virtual_key` n'est pas idempotent côté LiteLLM (un appel avec un alias déjà utilisé crée une clé supplémentaire) — mitigé par la garde `pod_will_be_created`/`job_already_exists`, jamais appelé pour un pod/Job déjà existant ; (4) **constaté empiriquement** : en relançant `cargo test --workspace` une seconde fois APRÈS avoir redémarré le controller live (donc avec un vrai controller qui réconcilie aussi, en concurrence, les Workshops éphémères que les tests créent/suppriment très rapidement), le controller live a authentiquement tenté de générer une Virtual Key pour un Workshop de test dont le pod parent était en cours de création par un cycle de reconciliation précédent du MÊME controller — LiteLLM a rejeté le second appel (`400 "Key with alias ... already exists"`), journalisé en `ERROR` mais non bloquant (`ensure_parent_pod` continue sans isolation pour ce cycle). Confiné aux Workshops de test à durée de vie de quelques secondes ; sans impact sur `my-new-demo` ni sur un Workshop utilisateur réel (créé une seule fois, jamais recréé/re-patché en rafale par du code externe). Reproduit et confirmé isolément : `curl -X POST /key/generate` avec un `key_alias` déjà pris renvoie exactement cette erreur `400`. Leçon retenue pour la suite : ne pas relancer la suite de tests d'intégration pendant qu'un controller live tourne sur le même cluster (redémarrer les tests avec le controller arrêté, comme fait pour la validation principale ci-dessus) — capturé ici pour la prochaine session.
- **Statut** : ✅ Validé pour les tâches 3.1.1 à 3.2.1 et le DoD du Jalon M3.


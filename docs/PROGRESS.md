# Point de situation

> Etat courant du projet. Ce document reste volontairement court : il dit ou
> on en est et ce qui vient ensuite, rien d'autre.
>
> - **Pieges connus, a lire avant de coder** :
>   [`architecture/pieges.md`](architecture/pieges.md)
> - **Suivi des taches par jalon** :
>   [`specs/PLAN-ACTION-GLOBAL.md`](specs/PLAN-ACTION-GLOBAL.md) (source unique)
> - **Conception cible** : [`ARCHITECTURE.md`](ARCHITECTURE.md)
> - **Recits detailles des sessions passees** :
>   [`archive/PROGRESS-2026-08.md`](archive/PROGRESS-2026-08.md) (fige)

Toutes les briques listees "Fonctionnel" ci-dessous ont ete validees contre
de la vraie infrastructure (kind reel, Firecracker/jailer reels, Keycloak et
OpenBao reels en conteneur, registre OCI reel) — jamais contre des mocks.
C'est un choix delibere du projet : les bugs trouves en cours de route sont
attendus et font partie du processus, ils sont consignes dans
[`architecture/pieges.md`](architecture/pieges.md).

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
| Claude Code dans le Workshop (via LiteLLM/DeepSeek) | Fonctionnel | Verifie dans une vraie microVM Firecracker : `claude --model claude-3-5-sonnet-20241022 --print` cree reellement le fichier demande, avec le contenu attendu. Chaine complete Claude Code -> `ANTHROPIC_BASE_URL=http://llm-proxy` -> alias `net-proxy` -> LiteLLM -> DeepSeek. Le modele doit etre epingle par l'appelant (le defaut du CLI change a chaque version et fait echouer Claude Code sans qu'il ecrive quoi que ce soit) — voir section du 2026-08-30 |
| `pm-engine` — graphe LangGraph complet | Partiel (PR vide) | Tous les noeuds traverses de bout en bout sur un vrai ticket Forgejo : analyse, decoupage en sous-taches paralleles, provisioning de vraies microVM, `exec_in_workshop`, tests, boucle de correction, PR ouverte, suspension et revue HITL enregistree. **La PR produite est vide** : le correctif du modele epingle n'a pas ete valide par un run complet — voir "Points ouverts" |
| Observabilite — Grafana/dashboard de supervision | Backlog | Explicitement reporte |
| Repo GitHub `atelier` | Publie (public) | Controller/api-server/dashboard/etc — CI GitHub Actions, images GHCR, site de doc MkDocs, licence AGPLv3 |
| Repo GitHub `atelier-workspace` | Publie (public) | Devcontainer de demo `ministack-workshop` (docker-in-docker, ministack, Claude Code, code-server), depot dedie separe pour que `image-builder` le clone sans identifiants git |

## Prochaines etapes

> **Points ouverts laisses par la session du 2026-08-30** (voir "Premiere
> execution complete du PM autonome" ci-dessus pour le contexte complet) :
>
> 0a. **Course sur `status.imageDigest`** — le plus important, et
>     independant du PM. `image-builder` patche `status.imageDigest` a la fin
>     du build, mais le controller ecrit ensuite un statut complet calcule
>     depuis une copie en memoire anterieure, ce qui **efface le digest**. Le
>     Workshop reste alors bloque en `BuildingImage` pour toujours, avec un
>     Job `Completed` et une image bel et bien publiee dans le cache.
>     Observe 1 fois sur 3 Workshops crees simultanement. Contournement
>     manuel utilise : re-patcher `status` avec le digest lu dans les logs du
>     Job. Correction a envisager : patch JSON merge cible sur les seuls
>     champs calcules, ou relecture du Workshop juste avant `update_status`.
>
> 0b. **La PR ouverte par le PM est vide** — le correctif "modele epingle"
>     n'a pas ete valide par un run complet. Ajouter au passage un garde-fou
>     dans `OpenPullRequest` : ouvrir une PR sans aucun changement devrait
>     echouer ou avertir, pas passer silencieusement.
>
> 0c. **`apply_wires_the_llm_virtual_key_injection_rule_when_configured`
>     echoue** (`crates/controller/tests/reconcile.rs`) : la regle
>     d'injection `llm-proxy` est absente (`[]`). Verifie comme
>     **preexistant** — l'echec se reproduit a l'identique sur un arbre
>     propre, sans les changements de cette session.
>
> 0d. **`ATELIER_LLM_PROXY_ADDR` souffre du meme defaut que
>     `OPENBAO_ADDR`** (corrige, lui, par `pod_addr`) : la meme valeur sert
>     aux appels du controller et a ce qui est injecte dans les pods. En dev,
>     la generation de Virtual Key echoue donc toujours et retombe sur le
>     jeton statique partage — degradation silencieuse mais fonctionnelle.

Au-dela de ces points ouverts, les chantiers encore devant nous :

7. Offload/reload du cache d'images vers S3 (prevu des la conception,
   explicitement differe).

8. Stack d'observabilite complet : collector OTLP + backend de stockage +
   Grafana.

> Les etapes 1 a 6 et 9 de la liste historique sont terminees (microVM
> builder, canal suspend/resume, reseau du pod parent, OAuth2 reel,
> `mcp-gateway`, device plugin `/dev/kvm`, devcontainer de demo) — leur
> recit complet est dans
> [`archive/PROGRESS-2026-08.md`](archive/PROGRESS-2026-08.md).

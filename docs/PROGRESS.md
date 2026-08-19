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
| `controller` — cycle suspend/resume | Fonctionnel (pod only) | Test reel : pod libere puis recree, entite Kanidm/role OpenBao preserves |
| `controller` — cleanup a la suppression | Fonctionnel | Finalizer `atelier.dev/cleanup`, verifie via test reel |
| `image-builder` — pipeline devcontainer → ext4 | Fonctionnel | Build reel via envbuilder + push registre + `crane export` + `mke2fs` sur un vrai depot (`vscode-remote-try-python`) |
| `image-builder` — publication PVC + patch status | Fonctionnel | Verifie manuellement (digest + `rootfs.ext4` presents dans le cache) |
| `vm-supervisor` — boot Firecracker jaile | Fonctionnel | VM reelle demarree via jailer + capabilities, hors et dans un pod `privileged: true` |
| `vm-supervisor` — snapshot/restore | Fonctionnel | Cycle snapshot → kill process → restore valide en local (hors pod) |
| `crates/firecracker` (lib partagee, extrait de `vm-supervisor`) | Fonctionnel | Meme test boot/snapshot/restore reel qu'avant le refactor, toujours vert |
| `crates/firecracker::network` (TAP link-local pour la microVM "builder") | Fonctionnel (composant seul) | Creation/config/suppression d'un vrai device TAP testee reellement (`unshare --net --map-root-user`, sans besoin de root) |
| `crates/builder-vm-init` (guest init de la microVM "builder") | Ecrit, boot complet toujours bloque | Compile, image Docker construite, convertie en `rootfs.ext4` bootable ; boot isole (rootfs seul, rootfs+reseau sans init custom) valide reellement, mais le boot complet (avec `envbuilder`) reste bloque avant meme d'atteindre `net-proxy` — cause pas encore identifiee, voir "Builder microVM" ci-dessous |
| Boucle complete Workshop → pod → microVM `Running` | Fonctionnel (en 2 temps) | Demontre de bout en bout ; le Job `image-builder` reel en cluster reste bloque avant l'etape finale (voir "Reseau kind ↔ registre" ci-dessous), donc le cache a ete peuple a la main pour ce test |
| Observabilite (OpenTelemetry) | Fonctionnel (base) | `atelier_common::telemetry::init()` cable sur tous les binaires, spans sur la boucle de reconciliation |
| `api-server` | Fonctionnel | JWT valide reellement (RS256, cle generee via `openssl`, JWKS charge au demarrage — pas encore teste contre un vrai flux OAuth2 Kanidm, aucun n'est configure dans l'instance de dev) ; endpoints CRUD + suspend/resume sur `Workshop` via `kube::Api`, testes reellement contre kind (creation, isolation par `owner_subject`, suspend/resume, suppression) |
| `net-proxy` — egress (allowlist + proxy parent) | Fonctionnel (composant seul) | Proxy HTTP explicite (relai en clair + tunnel `CONNECT`) avec allowlist par domaine/wildcard, et chainage optionnel vers un proxy parent (`ATELIER_UPSTREAM_PROXY`) avec bypass `ATELIER_NO_PROXY`. Teste reellement : allow/deny en HTTP et CONNECT, chainage via un second net-proxy en guise de proxy parent, bypass no_proxy verifie. Container pas encore ajoute au pod parent, allowlist pas encore alimentee depuis `Workshop.spec.egress_allowlist` par le controller |
| `net-proxy` — port-forward (microVM → exterieur) | Fonctionnel (composant seul) | Endpoint websocket `/portforward`, multiplexage de canaux dans le style `kubectl port-forward` (net-proxy = kubelet, `api-server` = coordinateur a authentifier — pas encore ecrit cote `api-server`). TCP et UDP. Teste via un vrai client websocket (`tokio-tungstenite`) : relai de donnees bout en bout et remontee d'erreur de connexion sur le canal dedie |
| `net-proxy` — DNS (UDP+TCP) | Fonctionnel (composant seul) | Resolveur DNS pour la VM, meme allowlist que l'egress (nom refuse → `REFUSED` local, jamais transmis a l'upstream). Teste reellement avec `dig` (UDP et TCP) contre un vrai upstream (resolveur systemd-resolved local), plus tests unitaires (parsing QNAME, upstream jamais contacte pour un nom refuse) |
| `identity-proxy` | Fonctionnel (composant seul) | Proxy HTTP explicite : injecte un en-tete (`Authorization` ou autre) construit depuis un secret OpenBao (cache rafraichi periodiquement, login Kubernetes reel) dans les requetes HTTP en clair dont l'hote correspond a une regle (`ATELIER_IDENTITY_INJECTION_RULES`), puis relaie vers `net-proxy` (`ATELIER_NET_PROXY_ADDR`) via un tunnel `CONNECT`. `CONNECT`/HTTPS reste un tunnel opaque, non injectable sans MITM (limite documentee). Teste reellement : bout-en-bout sur de vraies sockets TCP (injection verifiee sur ce que recoit la "destination"). Container pas encore ajoute au pod parent, regles pas encore alimentees depuis `Workshop.spec` par le controller |
| `mcp-gateway` | Non demarre | — |
| `dashboard` | Squelette Next.js | Pas encore branche sur `api-server` |
| Observabilite — Grafana/dashboard de supervision | Backlog | Explicitement reporte |
| Repo GitHub | Publie | Depot prive cree et pousse via `gh` |

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
- **Diagnostic pousse, cause du blocage principal toujours pas identifiee** :
  isole methodiquement par elimination — boot avec reseau + petit rootfs de
  test (`vm-supervisor`) : OK, rapide. Boot du rootfs "builder" (726 Mo)
  seul, sans reseau : OK. Boot du rootfs "builder" avec reseau mais sans
  `init=` custom (donc sans jamais executer `envbuilder`) : OK. Seule la
  combinaison complete (rootfs builder + reseau + `atelier-builder-vm-init`
  qui execute reellement `envbuilder`) bloque, avant meme d'emettre une
  seule requete vers `net-proxy` — donc probablement encore avant ou pendant
  la configuration reseau du guest (`configure_network`) ou tres tot dans
  l'invocation d'`envbuilder`, pas dans le clone/build lui-meme. Sans canal
  de controle (vsock) ni console capturee de maniere exploitable en dehors
  du `tracing::debug!` (invisible sans subscriber configure dans le test),
  la suite du diagnostic demande de capturer la console autrement (fichier
  plutot que `tracing`, ou logger explicitement chaque etape de
  `atelier-builder-vm-init` avant de shell out vers `ip`/`envbuilder`).
- **Phase suivante (hors perimetre de cette session)** : finir ce diagnostic
  (marche a suivre avec conteneur `--privileged` dans
  `deploy/dev/builder-vm/README.md`, a mettre a jour avec cette procedure),
  puis brancher ce composant dans `image-builder`/`reconcile.rs` a la place
  de l'invocation directe d'`envbuilder`, et passer le Job `image-builder`
  en `privileged: true` + `/dev/kvm` (exerce par notre jailer, jamais par le
  contenu du depot cible) pour clore l'item 1 de la roadmap.

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

## Prochaines etapes (par priorite)

1. Finir de valider la microVM "builder" (section dediee ci-dessus) —
   boot complet + `envbuilder` + push registre via `net-proxy`, sur une
   machine avec un vrai `CAP_NET_ADMIN` — puis la brancher dans
   `image-builder`/`reconcile.rs` a la place de l'invocation directe
   d'`envbuilder`. Le reseau kind ↔ registre lui-meme est deja resolu et
   verifie (Service/EndpointSlice statique). Une fois branche, le pipeline
   `image-builder` → cache → `vm-supervisor` peut tourner automatiquement
   de bout en bout, sans peuplage manuel du PVC.
2. Canal de controle vsock entre `controller`/`vm-supervisor` pour que
   `suspend` declenche un vrai `snapshot/create` avant liberation du pod
   (aujourd'hui le pod est simplement supprime, sans snapshot).
3. `identity-proxy` (logique de proxy/injection de credentials) est
   maintenant ecrit et teste en composant seul (voir tableau ci-dessus) ;
   reste a faire : ajouter `net-proxy` et `identity-proxy` comme conteneurs
   du pod parent, cabler l'allowlist de `net-proxy`
   (`ATELIER_EGRESS_ALLOWLIST`) et les regles d'injection d'`identity-proxy`
   (`ATELIER_IDENTITY_INJECTION_RULES`) depuis `Workshop.spec` cote
   controller, et trancher le TODO ouvert dans `docs/ARCHITECTURE.md`
   (l'agent parle-t-il a `identity-proxy` en direct, port TAP dedie, ou
   l'injection passe-t-elle par `net-proxy` lui-meme).
   - Donner un TAP reseau a la VM de l'agent (aujourd'hui absent —
     `vm-supervisor` boote sans interface reseau) en reutilisant
     `crates/firecracker::network::setup_link_local_tap` (lien
     point-a-point vers `net-proxy`, pas de NAT/acces direct a Internet —
     voir section "Builder microVM" ci-dessus pour le detail de ce choix,
     applicable tel quel a la VM de l'agent).
4. `api-server` : CRUD/suspend/resume et validation JWT sont ecrits et
   testes reellement (voir tableau et resume chronologique ci-dessus) ; reste
   a faire : configurer un vrai OAuth2 Resource Server cote Kanidm (aucun
   n'existe encore dans l'instance de dev, seulement des service accounts —
   voir `deploy/dev/kanidm/README.md`) pour valider `ATELIER_JWT_ISSUER`/
   `ATELIER_JWT_JWKS_URL` contre un vrai flux, et le role de coordinateur de
   port-forward (terminer la connexion du client final, verifier qu'il est
   bien proprietaire du `Workshop`, puis relayer vers le websocket
   `/portforward` de `net-proxy` — cote net-proxy est deja ecrit, voir
   composant "port-forward" ci-dessus).
5. `mcp-gateway` et le premier simulateur (candidat : LocalStack pour AWS).
6. Device plugin Kubernetes pour `/dev/kvm`, afin de sortir du pod
   `privileged: true`.
7. Offload/reload du cache d'images vers S3 (prevu des la conception,
   explicitement differe).
8. Stack d'observabilite complet : collector OTLP + backend de stockage +
   Grafana.

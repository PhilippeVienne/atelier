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
| Boucle complete Workshop → pod → microVM `Running` | Fonctionnel (en 2 temps) | Demontre de bout en bout ; le Job `image-builder` reel en cluster reste bloque avant l'etape finale (voir "Reseau kind ↔ registre" ci-dessous), donc le cache a ete peuple a la main pour ce test |
| Observabilite (OpenTelemetry) | Fonctionnel (base) | `atelier_common::telemetry::init()` cable sur tous les binaires, spans sur la boucle de reconciliation |
| `api-server` | Squelette | Auth JWT/Kanidm et endpoints CRUD/suspend/resume pas encore ecrits |
| `net-proxy` — egress (allowlist + proxy parent) | Fonctionnel (composant seul) | Proxy HTTP explicite (relai en clair + tunnel `CONNECT`) avec allowlist par domaine/wildcard, et chainage optionnel vers un proxy parent (`ATELIER_UPSTREAM_PROXY`) avec bypass `ATELIER_NO_PROXY`. Teste reellement : allow/deny en HTTP et CONNECT, chainage via un second net-proxy en guise de proxy parent, bypass no_proxy verifie. Container pas encore ajoute au pod parent, allowlist pas encore alimentee depuis `Workshop.spec.egress_allowlist` par le controller |
| `net-proxy` — port-forward (microVM → exterieur) | Fonctionnel (composant seul) | Endpoint websocket `/portforward`, multiplexage de canaux dans le style `kubectl port-forward` (net-proxy = kubelet, `api-server` = coordinateur a authentifier — pas encore ecrit cote `api-server`). TCP et UDP. Teste via un vrai client websocket (`tokio-tungstenite`) : relai de donnees bout en bout et remontee d'erreur de connexion sur le canal dedie |
| `identity-proxy` | Non demarre | Container pas encore ajoute au pod parent ; logique de proxy/injection pas ecrite |
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
- **A trancher avant d'activer** : isoler le Job `image-builder` plus
  fortement avant de lui donner cette capacite — `runtimeClass` gVisor/Kata
  si disponible sur le cluster cible, `NetworkPolicy` limitant son egress
  au registre + au depot git, node pool dedie avec taint pour borner le
  rayon d'action d'une evasion, ou a plus long terme faire tourner le build
  lui-meme dans une microVM plutot que dans un conteneur Kubernetes
  directement privilegie.

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

## Prochaines etapes (par priorite)

1. Trancher l'isolation du Job `image-builder` (voir section dediee
   ci-dessus) avant d'activer la capacite de mount dont il a besoin pour
   fonctionner en cluster — le reseau kind ↔ registre lui-meme est deja
   resolu et verifie (Service/EndpointSlice statique). Une fois tranche,
   le pipeline `image-builder` → cache → `vm-supervisor` peut tourner
   automatiquement de bout en bout, sans peuplage manuel du PVC.
2. Canal de controle vsock entre `controller`/`vm-supervisor` pour que
   `suspend` declenche un vrai `snapshot/create` avant liberation du pod
   (aujourd'hui le pod est simplement supprime, sans snapshot).
3. Ecrire `identity-proxy` (logique de proxy/injection de credentials) ;
   ajouter `net-proxy` et `identity-proxy` comme conteneurs du pod parent,
   et cabler l'allowlist de `net-proxy` (`ATELIER_EGRESS_ALLOWLIST`) depuis
   `Workshop.spec.egress_allowlist` cote controller.
   - Donner un TAP reseau a la VM de l'agent (aujourd'hui absent —
     `vm-supervisor` boote sans interface reseau, cf. commentaire de tete
     de `crates/firecracker/src/network.rs`) et poser les regles de
     pare-feu qui restreignent la VM a `net-proxy`/`identity-proxy`
     uniquement (mecanisme et regles `iptables` precises deja specifiees
     dans `docs/ARCHITECTURE.md`, section "Isolation reseau de la
     microVM") — ne pas reutiliser tel quel le `MASQUERADE` inconditionnel
     de `setup_link_local_tap` (legitime seulement pour la VM "builder").
4. `api-server` : validation JWT Kanidm reelle + endpoints CRUD et
   suspend/resume, plus le role de coordinateur de port-forward (terminer
   la connexion du client final, verifier qu'il est bien proprietaire du
   `Workshop`, puis relayer vers le websocket `/portforward` de `net-proxy`
   — cote net-proxy est deja ecrit, voir composant "port-forward"
   ci-dessus).
5. `mcp-gateway` et le premier simulateur (candidat : LocalStack pour AWS).
6. Device plugin Kubernetes pour `/dev/kvm`, afin de sortir du pod
   `privileged: true`.
7. Offload/reload du cache d'images vers S3 (prevu des la conception,
   explicitement differe).
8. Stack d'observabilite complet : collector OTLP + backend de stockage +
   Grafana.

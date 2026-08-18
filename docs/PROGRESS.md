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
| Boucle complete Workshop → pod → microVM `Running` | Fonctionnel (en 2 temps) | Demontre de bout en bout ; le Job `image-builder` ne joint pas encore le registre de dev depuis l'interieur de kind, donc le cache a ete peuple a la main pour ce test — pas un bug de code, un trou reseau de l'environnement de dev (voir backlog) |
| Observabilite (OpenTelemetry) | Fonctionnel (base) | `atelier_common::telemetry::init()` cable sur tous les binaires, spans sur la boucle de reconciliation |
| `api-server` | Squelette | Auth JWT/Kanidm et endpoints CRUD/suspend/resume pas encore ecrits |
| `net-proxy` | Non demarre | Container pas encore ajoute au pod parent |
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

1. Cabler le reseau kind ↔ registre de dev pour que le pipeline
   `image-builder` → cache → `vm-supervisor` tourne automatiquement de bout
   en bout, sans peuplage manuel du PVC.
2. Canal de controle vsock entre `controller`/`vm-supervisor` pour que
   `suspend` declenche un vrai `snapshot/create` avant liberation du pod
   (aujourd'hui le pod est simplement supprime, sans snapshot).
3. Ecrire `identity-proxy` (logique de proxy/injection de credentials) et
   `net-proxy` (allowlist egress), les ajouter comme conteneurs du pod
   parent.
4. `api-server` : validation JWT Kanidm reelle + endpoints CRUD et
   suspend/resume.
5. `mcp-gateway` et le premier simulateur (candidat : LocalStack pour AWS).
6. Device plugin Kubernetes pour `/dev/kvm`, afin de sortir du pod
   `privileged: true`.
7. Offload/reload du cache d'images vers S3 (prevu des la conception,
   explicitement differe).
8. Stack d'observabilite complet : collector OTLP + backend de stockage +
   Grafana.

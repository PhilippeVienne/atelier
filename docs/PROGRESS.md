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
| CRD `Workshop` + types (`crates/common`) | Fonctionnel | Round-trip serde valide sur kind |
| `controller` — reconciliation, OpenBao, suspend/resume, cleanup | Fonctionnel | Tests d'integration reels contre kind (snapshot/restore Firecracker cross-process, finalizer) |
| `image-builder` — pipeline devcontainer → ext4 | Fonctionnel | Build reel de bout en bout dans la microVM "builder" (`envbuilder` + `crane export` + `mke2fs`) |
| `vm-supervisor` / `crates/firecracker` — boot, snapshot/restore, reseau TAP | Fonctionnel | Boot jaile non privilegie (`atelier.dev/kvm`), cycle suspend/resume cross-process verifie |
| `kvm-device-plugin` | Fonctionnel | Expose `/dev/kvm`+`/dev/net/tun` via device plugin kubelet, verifie contre kind |
| `crates/builder-vm-init` | Fonctionnel | Cycle boot+reseau+`envbuilder`+extinction verifie de bout en bout |
| Boucle Workshop → pod → microVM `Running` | Fonctionnel (automatique) | `kubectl apply` declenche build + boot sans peuplage manuel du cache |
| Cache d'images/snapshots — offload S3 + eviction LRU | Fonctionnel | PVC local = cache chaud, S3 = source durable ; correctif d'une fuite reelle (snapshots orphelins a la suppression d'un Workshop) — `docs/specs/13-image-cache-offload.md` |
| `api-server` | Fonctionnel | JWT OIDC (JWKS), CRUD/suspend/resume `Workshop`, port-forward WS, ponts `code-server`/`ttyd`, exec REST (`/exec`, verifie contre un vrai `sshd` et dans l'invite, tache 14.6), champs de session du `devcontainer.json` appliques (tache 14.7) |
| `net-proxy` — egress/port-forward/DNS | Fonctionnel | Allowlist + passerelle transparente + websocket port-forward + resolveur DNS, testes reellement |
| `identity-proxy` | Fonctionnel | Injection d'en-tete depuis OpenBao, verifie contre un vrai pod |
| `mcp-gateway` | Fonctionnel (HTTP/SSE + vsock) | 3 tools verifies contre OpenBao/net-proxy/LocalStack reels |
| `dashboard` | Fonctionnel | Next.js 16 BFF, CRUD Workshops, VS Code/terminal en navigateur reel |
| `llm-proxy` (LiteLLM) | Fonctionnel | Routage Claude Code → DeepSeek verifie de bout en bout via l'alias `net-proxy` ; configuration des modeles par un admin (ajout/modification/suppression) depuis le Dashboard — `docs/specs/11-admin-litellm-model-config.md` |
| `charts/atelier` (Helm, Jalon M6) | Fonctionnel | Chart monolithique (control plane + infra embarquee), 4 Ingress/TLS, Jobs d'init sequences — `docs/admin-guide.md` |
| `scripts/install.sh` (single-node low-cost) | Fonctionnel (shellcheck + verif GHCR) | `docs/specs/10-low-cost-single-node-install.md` — non execute de bout en bout sur serveur frais |
| `pm-engine` — graphe LangGraph (PM autonome) | Valide de bout en bout | Ticket Forgejo reel → decoupage → microVMs → PR — `docs/specs/05-devfactory-pm-engine.md` |
| `pm-engine` — equipe consultative (Architecte/QA/Securite/Ops) | Valide de bout en bout | `docs/specs/08-equipe-it-consultative.md` |
| `pm-engine` — validateur QA post-merge (`QAValidation`) | Valide de bout en bout | Workshop dedie post-merge, preuves S3 — `docs/specs/09-qa-validation-post-merge.md` |
| Observabilite — traces/metriques/logs (`grafana/otel-lgtm`) | Fonctionnel (base) | Traces (`api-server`, `controller`) et metriques HTTP (`api-server`) exportees et verifiees dans Tempo/Prometheus reels — `docs/specs/12-observabilite.md`. Logs de conteneurs et dashboards Grafana dedies restent hors perimetre (spec §2). |
| Repo GitHub `atelier` / `atelier-workspace` | Publie (public) | CI GitHub Actions, images GHCR, site MkDocs, AGPLv3 |

## Prochaines etapes

Jalons M1 a M8 entierement `[x]`. Le jalon M14 (sessions pour apprenants,
`docs/specs/19-sessions-pour-apprenants.md`) est presque termine : faites
14.1, 14.2, 14.4 a 14.6, 14.8, 14.11 a 14.15. Restent ouvertes :

- **14.3** (services jamais en root) et **14.9** (noyau `nf_tables`) : le code
  est ecrit, la preuve dans l'invite manque. Il faut une image glibc **sans
  systemd** (14.3) et une image Docker qui ne pose pas
  `DOCKER_INSECURE_NO_IPTABLES_RAW` (14.9) ; aucune des images de Mentor ne
  convient, et l'init d'Atelier ne demarre pas sur une image musl (voir
  `architecture/pieges.md`).
- **14.7** : `cloneWorkspace`, `remoteUser`, `workspaceFolder` et
  `containerEnv` sont faits ; `postStartCommand`, `hostRequirements` et le
  choix du compte par appel d'exec ne le sont pas.
- **14.10** : questions ouvertes de la spec §4, a instruire.

Le jalon M13 (reutilisation des images par source) et le jalon M9
(CLI/Pont IDE, simulateurs in-VM, HITL) ne sont pas commences. Detail dans
`docs/specs/PLAN-ACTION-GLOBAL.md`. Le noyau invite se construit avec
`deploy/dev/firecracker/kernel/build.sh` (non versionne dans les images de
production pour l'instant).

> Le récit complet des jalons 1 à 6 (microVM builder, canal suspend/resume,
> reseau du pod parent, OAuth2/OIDC, `mcp-gateway`, device plugin `/dev/kvm`,
> devcontainer de demo, PM Engine, chart Helm) est dans `git log` et
> [`archive/PROGRESS-2026-08.md`](archive/PROGRESS-2026-08.md).

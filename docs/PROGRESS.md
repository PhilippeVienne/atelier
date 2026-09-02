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
| `api-server` | Fonctionnel | JWT OIDC (JWKS), CRUD/suspend/resume `Workshop`, port-forward WS, ponts `code-server`/`ttyd` |
| `net-proxy` — egress/port-forward/DNS | Fonctionnel | Allowlist + passerelle transparente + websocket port-forward + resolveur DNS, testes reellement |
| `identity-proxy` | Fonctionnel | Injection d'en-tete depuis OpenBao, verifie contre un vrai pod |
| `mcp-gateway` | Fonctionnel (HTTP/SSE + vsock) | 3 tools verifies contre OpenBao/net-proxy/LocalStack reels |
| `dashboard` | Fonctionnel | Next.js 16 BFF, CRUD Workshops, VS Code/terminal en navigateur reel |
| `llm-proxy` (LiteLLM) | Fonctionnel (base) | Routage Claude Code → DeepSeek verifie de bout en bout via l'alias `net-proxy` |
| `charts/atelier` (Helm, Jalon M6) | Fonctionnel | Chart monolithique (control plane + infra embarquee), 4 Ingress/TLS, Jobs d'init sequences — `docs/admin-guide.md` |
| `scripts/install.sh` (single-node low-cost) | Fonctionnel (shellcheck + verif GHCR) | `docs/specs/10-low-cost-single-node-install.md` — non execute de bout en bout sur serveur frais |
| `pm-engine` — graphe LangGraph (PM autonome) | Valide de bout en bout | Ticket Forgejo reel → decoupage → microVMs → PR — `docs/specs/05-devfactory-pm-engine.md` |
| `pm-engine` — equipe consultative (Architecte/QA/Securite/Ops) | Valide de bout en bout | `docs/specs/08-equipe-it-consultative.md` |
| `pm-engine` — validateur QA post-merge (`QAValidation`) | Valide de bout en bout | Workshop dedie post-merge, preuves S3 — `docs/specs/09-qa-validation-post-merge.md` |
| Observabilite — Grafana/dashboard de supervision | Backlog | Explicitement reporte |
| Repo GitHub `atelier` / `atelier-workspace` | Publie (public) | CI GitHub Actions, images GHCR, site MkDocs, AGPLv3 |

## Prochaines etapes

- Offload/reload du cache d'images `image-builder` vers S3 (prevu des la
  conception, encore un `TODO` dans `crates/image-builder/src/main.rs`).
- Stack d'observabilite complet : collector OTLP + backend de stockage + Grafana.
- `apply_wires_the_llm_virtual_key_injection_rule_when_configured`
  (`crates/controller/tests/reconcile.rs`) : dernier statut connu "echoue,
  regle d'injection absente" — pas revérifié depuis, à confirmer avant d'y
  toucher.

> Le récit complet des jalons 1 à 6 (microVM builder, canal suspend/resume,
> reseau du pod parent, OAuth2/OIDC, `mcp-gateway`, device plugin `/dev/kvm`,
> devcontainer de demo, PM Engine, chart Helm) est dans `git log` et
> [`archive/PROGRESS-2026-08.md`](archive/PROGRESS-2026-08.md).

# Plan d'Action Global d'Implémentation

> **Statut** : Plan Cadre Opérationnel & Feuille de Route d'Ingénierie
> **Dernière compression** : 2026-09-05 — les jalons M1 à M6 (117 tâches,
> toutes `[x]`) ont été sortis vers
> [`docs/archive/PLAN-ACTION-M1-M6.md`](../archive/PLAN-ACTION-M1-M6.md)
> (même raison que l'archivage de `docs/PROGRESS.md` fin août 2026 : une
> tâche validée n'a plus besoin de son détail ligne à ligne dans le
> document courant, seulement dans `git log` et l'archive). Ce document ne
> garde que ce qui reste utile pour démarrer la prochaine tâche : le
> protocole de traçabilité, les principes transversaux, la carte des
> dépendances, la Matrice Récapitulative des jalons clos, et les jalons en
> cours ou à venir.
> **Protocole de Transition Multi-Agents & États de Tâche** : Ce plan est conçu pour être **exécuté de manière asynchrone, interrompu à tout moment et repris sans friction par n'importe quel autre agent IA** (Claude Code, Gemini CLI, Antigravity).
> **Règle de Traçabilité Obligatoire & États `[ ]` / `[-/<agent>/<session_id>]` / `[x]`** :
> 1. `[ ]` : Tâche en attente / non démarrée.
> 2. `[-/<agent_family>/<session_id>]` : **Tâche en cours d'exécution par un LLM / Agent identifié** (ex: `[-/claude-code/sess-4a8b]` ou `[-/antigravity/c192a786]`). Permet de savoir précisément qui travaille sur la tâche et si la session est toujours active.
> 3. `[x]` : **Tâche terminée et validée empiriquement** par tests réels sans mocks et journalisée dans [`docs/PROGRESS.md`](../PROGRESS.md).

---

## Sommaire

1. [Protocole de Transmission & Traçabilité Multi-Agents](#1-protocole-de-transmission--traçabilité-multi-agents)
2. [Principes Directeurs & Definition of Done (DoD) Transversale](#2-principes-directeurs--definition-of-done-dod-transversale)
3. [Cartographie des Dépendances & Matrice d'Impact Globale](#3-cartographie-des-dépendances--matrice-dimpact-globale)
4. [Jalons Clos (M1 à M6) — Matrice Récapitulative](#4-jalons-clos-m1-à-m6--matrice-récapitulative)
5. [Jalon 7 (M7) : Stack d'Observabilité (Traces, Métriques, Logs)](#5-jalon-7-m7--stack-dobservabilité-traces-métriques-logs)
6. [Jalon 8 (M8) : Offload S3 du Cache d'Images et des Snapshots](#6-jalon-8-m8--offload-s3-du-cache-dimages-et-des-snapshots)
7. [Jalon 9 (M9) : Expérience Dev (CLI, Pont IDE), Simulateurs in-VM & HITL](#7-jalon-9-m9--expérience-dev-cli-pont-ide-simulateurs-in-vm--hitl)

---

## 1. Protocole de Transmission & Traçabilité Multi-Agents

Afin de permettre une collaboration fluide entre différents agents IA ou sessions interrompues :

### 📋 Instructions Strictes pour l'Agent Exécuteur :
1. **Vérification Initiale & Verrouillage Nominatif (`[-/<family>/<id>]`)** :
   - Inspecter `git status`, [`docs/PROGRESS.md`](../PROGRESS.md) et ce document `PLAN-ACTION-GLOBAL.md`.
   - **Vérifier qu'aucune tâche antérieure n'est laissée en cours `[-/...]`** ou non validée `[ ]`.
   - Dès qu'un agent prend en charge une tâche `[ ]`, il **DOIT IMMÉDIATEMENT positionner le marqueur nominatif `[-/<agent_family>/<session_id>]`** (ex: `[-/antigravity/c192a786]` ou `[-/claude-code/sess-xyz]`) sur la tâche dans `PLAN-ACTION-GLOBAL.md`.
2. **Validation d'une tâche (`[x]`)** :
   - Exécuter impérativement les tests unitaires et de linter (`cargo test`, `cargo clippy`, `cargo fmt` ou `pytest`).
   - Remplacer le marqueur `[-/...]` par **`[x]`** (indiquant formellement le travail terminé) dans ce document `PLAN-ACTION-GLOBAL.md`.
   - Consigner ce qui doit réellement l'être dans `docs/PROGRESS.md`/`docs/architecture/pieges.md` (voir `AGENTS.md`, section « Mise à jour de la Documentation ») — pas d'entrée narrative datée par tâche.
3. **Interruption / Passage de relais** :
   - Si une session s'arrête en cours de tâche, laisser le marqueur `[-/<family>/<id>]` et documenter dans `docs/PROGRESS.md` l'état exact d'avancement pour que l'agent suivant sache exactement d'où repartir.

---

## 2. Principes Directeurs & Definition of Done (DoD) Transversale

Conformément à [`AGENTS.md`](file:///home/philippe/github.com/PhilippeVienne/atelier/AGENTS.md) et [`00-architecture-principles-substitutability.md`](00-architecture-principles-substitutability.md) :

### 🛡️ Exigences de Qualité Transversales
1. **Zero `unsafe`** dans tout le code Rust de production (`crates/*/src/`).
2. **Formatage & Linting Stricts** :
   - `cargo fmt --all -- --check` est 100% propre.
   - `cargo clippy --workspace --all-targets -- -D warnings` ne retourne aucun avertissement.
3. **Vérification Empirique sans Mocks (Infrastructure Dev Réelle)** :
   - Tous les tests d'intégration s'exécutent contre de vrais conteneurs / pods locaux (PostgreSQL réel sous Kind port 5433, S3/MinIO réel, Forgejo réel, OpenBao réel, LiteLLM réel, Redis réel, cluster Kind réel avec microVMs Firecracker).
   - Zéro mock factice remplaçant les composants réseau ou de stockage.
4. **Documentation Vivante** :
   - Mise à jour systématique de [`docs/PROGRESS.md`](file:///home/philippe/github.com/PhilippeVienne/atelier/docs/PROGRESS.md) avec preuves d'exécution.
5. **Dashboard Next.js 16** :
   - Validation de la compilation TypeScript et du build (`npm run build`).

---

## 3. Cartographie des Dépendances & Matrice d'Impact Globale

```mermaid
graph TD
    subgraph Layer1["1. Contrats Partagés"]
        CRD["crates/common/src/crd.rs\n(Suppression Kanidm, ajout maxLlmBudgetUsd)"]
        SQL_SCHEMA["Schémas SQL & Migrations\n(PostgreSQL 16 + pgvector)"]
    end

    subgraph Layer2["2. Control Plane Rust"]
        API["crates/api-server\n(OIDC, sqlx, S3, /v1/mcp, Basic Auth OpenBao, Health)"]
        CTRL["crates/controller\n(sqlx, LiteLLM keys, HTTPS Git, Session Auth Vault, Health)"]
        IDP["crates/identity-proxy\n(Injection PAT HTTPS)"]
    end

    subgraph Layer3["3. Infrastructure & IA"]
        KC["Keycloak / OIDC Provider"]
        FJ["Forgejo / GitHub / GitLab"]
        LLM["LiteLLM (Virtual Keys)"]
        OB["OpenBao / Vault (Secrets & Session Auth)"]
        S3["RustFS / GCS / Azure / AWS (S3)"]
        REDIS["Redis (Streams)"]
    end

    subgraph Layer4["4. DevFactory & Dashboard"]
        PM["services/pm-engine\n(Python, LangGraph, pgvector)"]
        DASH["dashboard/\n(Next.js 16, OIDC, Ask PM, VS Code, Terminal)"]
    end

    subgraph Layer5["5. Packaging Helm & Dev Scripts"]
        HELM["charts/atelier\n(Monolithique, 4 Ingress, BYO, Cloud IAM)"]
        DEV_STACK["deploy/dev/local-stack.sh & teardown-stack.sh"]
        DOC["docs/admin-guide.md\n(Runbook d'exploitation)"]
    end

    CRD --> CTRL
    CRD --> API
    SQL_SCHEMA --> API
    SQL_SCHEMA --> CTRL
    SQL_SCHEMA --> PM

    KC --> API
    KC --> DASH
    FJ --> PM
    LLM --> CTRL
    LLM --> PM
    OB --> CTRL
    OB --> API
    S3 --> API
    S3 --> FJ
    REDIS --> PM

    CTRL --> IDP
    API --> DASH
    PM --> API
    PM --> DASH

    API --> HELM
    CTRL --> HELM
    PM --> HELM
    HELM --> DOC
    DEV_STACK --> HELM
```

---

## 4. Jalons Clos (M1 à M6) — Matrice Récapitulative

Détail tâche par tâche (fichiers exacts, garde-fous, sous-tâches) dans
[`docs/archive/PLAN-ACTION-M1-M6.md`](../archive/PLAN-ACTION-M1-M6.md).

| Jalon | Intitulé | Livrables & Composants Clés | Critère de Validation Empirique (Go / No-Go) |
| :--- | :--- | :--- | :--- |
| **M1** | **Socle DB, OIDC, Basic Auth & Health** | `crates/common`, `crates/api-server`, `crates/controller`, `dashboard/` | `cargo test --workspace` passe avec vrai Postgres & OIDC, Basic Auth OpenBao et sondes /health opérationnelles. |
| **M2** | **S3 & Git HTTPS (Dev Pods)** | `crates/api-server/src/storage.rs`, `crates/identity-proxy`, `deploy/dev/{s3,forgejo}` | Upload de session S3 réussi, clone Git HTTPS privé réussi contre Forgejo dev. |
| **M3** | **LiteLLM & Budgets** | `crates/controller/src/litellm.rs`, `crates/common/src/crd.rs` | Virtual Key créée avec TTL court renouvelé à chaud post-resume, blocage 429 au dépassement de quota. |
| **M4** | **Serveur MCP Externe** | `crates/api-server/src/mcp_*.rs` | Client Claude Desktop connecté sur `/v1/mcp`, streaming `exec_in_workshop` bufferisé dans Postgres. |
| **M5** | **DevFactory PM Engine** | `services/pm-engine`, `dashboard/`, `deploy/dev/redis` | Workflow LangGraph complet (issue ➔ sous-branches ➔ auto-correction ➔ git-sync ➔ snapshot S3 ➔ merge). |
| **M6** | **Helm & Admin Doc** | `charts/atelier/`, `deploy/dev/*-stack.sh`, `docs/admin-guide.md` | `helm install` 100% opérationnel sur Kind avec 4 Ingress, identités Cloud, scripts dev et hooks validés — inclut la configuration des modèles LiteLLM par un admin (`docs/specs/11-admin-litellm-model-config.md`). |

---

## 5. Jalon 7 (M7) : Stack d'Observabilité (Traces, Métriques, Logs)

* **Spécification** : [`12-observabilite.md`](12-observabilite.md)
* **Constat** : `crates/common/src/telemetry.rs` exporte déjà des traces OTLP si `OTEL_EXPORTER_OTLP_ENDPOINT` est positionnée — elle ne l'est nulle part (chart, dev). Pire, vérifié empiriquement : même en la positionnant vers un vrai collecteur, **zéro trace n'est produite**, aucune route/boucle n'étant instrumentée par un span (`TraceLayer`/`#[instrument]` absents partout).
* **Fichiers** : `crates/common/src/telemetry.rs`, `crates/api-server/src/routes.rs`, `crates/controller/src/reconcile.rs`, `charts/atelier/templates/infra/observability-deployment.yaml` (nouveau), `charts/atelier/values.yaml`, `deploy/dev/local-stack.sh`.
  - [x] **7.1** : Nouveau composant chart `observability` (`grafana/otel-lgtm`, un seul pod — voir spec §3 pour la justification et les mesures empiriques de démarrage/mémoire), `.Values.observability.enabled`/`.resources`. Déployé réellement sur `kind-atelier-dev` : pod `Running` en 37s, `otel-lgtm:0.32.1` épinglé (même digest que `:latest` au moment de la vérification). `helm lint`/`helm template` passent.
  - [x] **7.2** : `OTEL_EXPORTER_OTLP_ENDPOINT` câblée sur les Deployments `api-server`/`controller` du chart quand `observability.enabled`. `deploy/dev/local-stack.sh` déploie inconditionnellement `deploy/dev/otel/dev-deployment.yaml` (remplace l'ancien `collector-config.yaml`, orphelin depuis `a51b1c9`, jamais câblé) et ouvre un port-forward local (4317) — vérifié contre le cluster de dev réel (Service→pod atteignable, `nc` réussi). `helm lint`/`helm template`/`shellcheck` propres.
  - [x] **7.3** : `tower_http::TraceLayer` sur `routes::router` (`api-server`) — un span par requête HTTP, méthode/chemin/statut en attributs. Piège trouvé en vérifiant : `TraceLayer` crée son span par défaut à `DEBUG`, filtré par l'`EnvFilter` par défaut de `telemetry::init` ("info") — sans `.level(Level::INFO)` explicite, aucune trace n'était exportée malgré `TraceLayer` en place. Vérifié bout en bout via le chemin réel (Service Kubernetes + port-forward 4317, cluster de dev, `RUST_LOG` non défini) : les requêtes HTTP produisent bien des traces `service.name=atelier-api-server` dans Tempo. `cargo test`/`clippy`/`fmt` propres.
  - [x] **7.4** : Déjà fait — `crates/controller/src/reconcile.rs` porte `#[tracing::instrument(skip_all, ...)]` sur 5 fonctions depuis le commit `a51b1c9` (2026-08-18), dont deux avec `fields(workshop = %workshop.name_any())`. Découvert en cours de rédaction de la spec (une recherche trop stricte l'avait manqué) puis reverifié bout en bout : `atelier-controller` local + `OTEL_EXPORTER_OTLP_ENDPOINT` vers un `grafana/otel-lgtm` réel + réconciliation réelle contre le cluster de dev → 5 traces `service.name=atelier-controller` retrouvées dans Tempo. Aucune implémentation nécessaire, seul le câblage de 7.2 manquait.
  - [x] **7.5** : `telemetry.rs` gagne un `MeterProvider` (`opentelemetry_otlp::MetricExporter` + `with_periodic_exporter` ; feature `metrics` déjà en défaut chez `opentelemetry_sdk`/`-otlp`, aucune dépendance nouvelle). Nouveau `crates/api-server/src/http_metrics.rs` : compteur de requêtes + histogramme de latence, posés via `.route_layer()` pour que `MatchedPath` soit résolu (évite la cardinalité non bornée du chemin brut). Vérifié contre une instance réelle de `grafana/otel-lgtm` : `http_server_request_count_total`/`http_server_duration_milliseconds_count` visibles dans Prometheus avec les bons labels. `cargo test`/`clippy`/`fmt` propres. **M7 (Observabilité) entièrement clos.**

---

## 6. Jalon 8 (M8) : Offload S3 du Cache d'Images et des Snapshots

* **Spécification** : [`13-image-cache-offload.md`](13-image-cache-offload.md)
* **Constat** : le `TODO` de `image-builder::publish_to_cache` n'était pas qu'une optimisation théorique — **découvert en creusant ce chantier** : le disque de la machine de dev était à 96 % d'utilisation, `local-path-provisioner` ne fait respecter AUCUN quota (400 Go réels contre un PVC nominal `20Gi`), et surtout `crates/controller/src/reconcile.rs::cleanup()` ne supprime JAMAIS le snapshot d'un `Workshop` supprimé (103 Go de snapshots orphelins mesurés). Pansement déjà appliqué cette session (contenu du PVC vidé manuellement, disque revenu à 50 %) — pas une solution durable.
* **Fichiers** : `crates/controller/src/reconcile.rs` (`cleanup`), `crates/controller/src/storage.rs`, `crates/image-builder/src/main.rs` (`publish_to_cache`), `crates/vm-supervisor/src/main.rs`, `crates/api-server/src/storage.rs` → à déplacer vers `crates/common/src/storage.rs` (spec §3.3), `charts/atelier/values.yaml` (`S3_BUCKET_IMAGE_CACHE`).
  - [x] **8.1** : **Correctif prioritaire (bug, pas une optimisation)** — `cleanup()` lance un Job éphémère (`busybox`, `ttlSecondsAfterFinished=300`) qui monte le PVC de cache et supprime `snapshots/<ns>_<name>` du Workshop supprimé — le controller lui-même ne monte jamais ce PVC. Vérifié de bout en bout contre le cluster de dev réel : faux snapshot créé, Workshop supprimé, Job `Complete` en 6s, répertoire confirmé absent après coup. `cargo test`/`clippy`/`fmt` propres.
  - [x] **8.2** : `StorageBackend`/`S3StorageBackend` déplacés vers `crates/common/src/storage.rs` (spec §3.3) ; `api-server` importe désormais ce type depuis `atelier_common` (dépendances `aws-sdk-s3`/`async-compression`/`bytes`/`async-trait`/`aws-config` déplacées ou retirées en conséquence, `aws-config` n'était déjà plus utilisé nulle part). Tests d'intégration déplacés (`crates/common/tests/storage.rs`) et re-vérifiés réellement contre RustFS (`cargo test -p atelier-common --test storage` + `cargo test -p atelier-api-server --test session_recorder`, les deux passent contre un vrai bucket). `cargo test`/`clippy`/`fmt` propres sur tout le workspace.
  - [x] **8.3** : Nouvelle variable `S3_BUCKET_IMAGE_CACHE` (spec §3.2, optionnelle — contrairement à `S3_BUCKET_SESSIONS`/`SNAPSHOTS`, seul `image-builder` en a besoin) ; `image-builder` téléverse vers S3 après `publish_to_cache` local (best-effort, non bloquant). **Correction découverte en implémentant** : le digest n'est connu qu'APRÈS la construction complète (`sha256_file` sur le `rootfs.ext4` fini) — aucune vérification préalable ne peut donc éviter un rebuild `envbuilder`, contrairement à ce qu'affirmait la première rédaction de la spec (corrigée, voir spec §3). Le gain réel : survie de l'artefact à une éviction locale ultérieure (8.5), pas une évitation de rebuild.
    `ReconcileCtx` gagne `s3`/`s3_pod_endpoint` (controller ne s'en sert pas lui-même, retransmet au Job — **piège trouvé en vérifiant** : sans `s3_pod_endpoint`, un controller de dev hors cluster transmettait son PROPRE `S3_ENDPOINT` en `127.0.0.1:9000`, injoignable depuis le Job ; même correctif que `llm_proxy_pod_addr`/`OpenBaoConfig::pod_addr`). Câblé dans `controller-deployment.yaml` (RustFS embarqué ET S3 externe) et `deploy/dev/local-stack.sh`. Vérifié de bout en bout contre le cluster de dev réel : vrai Workshop créé, Job `image-build` inspecté, `S3_ENDPOINT` correct des deux côtés (`127.0.0.1:9000` côté controller, DNS in-cluster côté Job) ; test d'intégration `upload_image_cache_file_is_retrievable_with_the_conventional_key` passe contre RustFS réel. `cargo test`/`clippy`/`fmt`/`helm lint`/`shellcheck` propres.
  - [x] **8.4** : `vm-supervisor` téléverse ses fichiers de snapshot vers `S3_BUCKET_SNAPSHOTS` après publication locale (`snapshot_and_publish`, best-effort) et les retélécharge si absents localement (évincés) avant `Vm::restore_persisted` — téléchargement partiel (un seul des deux fichiers) traité comme un échec complet, les deux sont supprimés pour retomber proprement sur le boot à froid. Nouvelles méthodes `S3StorageBackend::upload_snapshot_file`/`download_snapshot_to_file` (`crates/common`), clé S3 = même préfixe que le répertoire local (`storage::snapshot_cache_subdir`), transmis par le `controller` via `ATELIER_VM_SNAPSHOT_S3_PREFIX` — `vm-supervisor` n'a besoin de connaître ni `ns` ni `name` lui-même. Même câblage `ctx.s3`/`s3_pod_endpoint` que 8.3 (déjà vérifié). Nouveau test d'intégration réel (`upload_snapshot_file_survives_local_eviction_via_download`, cycle upload→suppression locale→téléchargement→vérification de contenu) passe contre RustFS réel. `cargo test`/`clippy`/`fmt` propres.
  - [x] **8.5** : Passe périodique (`crates/controller/src/eviction.rs`, boucle `tokio::spawn` indépendante de la réconciliation, toutes les 15 min) qui crée un Job éphémère (`minio/mc`) montant le PVC de cache, plafond configurable (`.Values.imageCache.evictionThresholdGb`/`ATELIER_IMAGE_CACHE_EVICTION_THRESHOLD_GB`), désactivée si `S3_BUCKET_IMAGE_CACHE` n'est pas configuré. **Piège trouvé en vérifiant** : l'image `minio/mc` n'a NI `find` NI `awk` (quasi-distroless) — le script initial (basé sur `find -printf`/`awk` comme `s3-init-job.yaml`) aurait échoué silencieusement en prod. Réécrit avec uniquement `stat -c '%Y %n' | sort -n` (tri par date) et un accumulateur shell pur (pas d'`awk`), vérifié ligne par ligne dans un pod jetable contre le cluster de dev réel avant de porter le script dans le code Rust. Scénario de sécurité vérifié explicitement : une entrée ancienne mais absente de S3 est bien préservée malgré son ancienneté, une entrée plus récente mais confirmée sur S3 est évincée à sa place. Job réellement créé et exécuté via le vrai binaire `controller` (`Complete` en 4s). `cargo test`/`clippy`/`fmt`, `helm lint`/`template`, `shellcheck` propres. **M8 (Offload S3 du Cache d'Images et des Snapshots) entièrement clos.**

---

## 7. Jalon 9 (M9) : Expérience Dev (CLI, Pont IDE), Simulateurs in-VM & HITL

* **Spécification** : [`14-devex-cli-simulateurs-hitl.md`](14-devex-cli-simulateurs-hitl.md)
* **Constat** : L'accès aux microVMs Atelier nécessite aujourd'hui le Dashboard web ou `kubectl`. Par ailleurs, les agents de code in-VM manquent de dépendances d'appoint (Postgres, S3 LocalStack, stubs API) sans accès Internet direct, et les opérations sensibles (extension allowlist, PRs critiques) nécessitent une boucle de décision humaine ("Human-in-the-Loop").
* **Fichiers** : `crates/cli` (nouveau), `crates/common/src/crd.rs` (`simulators`), `crates/controller/src/reconcile.rs`, `crates/mcp-gateway/src/`, `crates/api-server/src/approvals.rs` (nouveau), `dashboard/app/approvals/`.
  - [x] **9.1** : Nouveau crate `crates/cli` — gestion multi-environnements `atelier context add/use/list` (support d'un cluster local Kind ou distant EKS/GKE/bare-metal), flux d'auth `atelier auth login` (Device Authorization Grant RFC 8628, decouverte OIDC generique), stockage des jetons dans le trousseau OS (`keyring`, backend `linux-native`/keyutils), et commandes `atelier workshops list/create/status/stop/resume/delete`. Verifie de bout en bout contre de la vraie infra : nouveau client Keycloak `atelier-cli` (Device Authorization Grant active) ajoute a `deploy/dev/keycloak/realm-export.json` et cree via l'API admin sur l'instance de dev deja en cours ; login reel via le navigateur (utilisateur `atelier-test-user`) jusqu'a stockage du jeton dans le trousseau ; `atelier-api-server` reel lance en local, cycle complet `create` → `list` → `status` → `stop` → `resume` → `delete` execute contre lui, Workshop confirme absent de `kubectl get workshop` apres coup. `cargo check`/`clippy`/`fmt` propres. Point ouvert non bloquant : le backend `keyring` choisi (`linux-native`) n'est pas cross-platform (voir commentaire dans `crates/cli/Cargo.toml`) — a revisiter si portage macOS/Windows requis.
  - [x] **9.2** : Pont Tunnels dans la CLI — `crates/cli/src/tunnel.rs` reimplemente cote client le sous-protocole websocket `portforward.k8s.io` deja parle par `crates/api-server/src/portforward.rs`/`crates/net-proxy/src/portforward.rs` (canal `2*i` donnees / `2*i+1` erreurs, un octet de prefixe par message). `atelier port-forward <id> <local>:<remote>` (mode ecoute, une session par connexion TCP acceptee) et `atelier port-forward --stdio <id> <port>` (relais stdin/stdout, utilisable comme `ProxyCommand` SSH) ; `atelier ssh <id>` et `atelier code <id>` deleguent a un client `ssh`/IDE systeme reel via ce `ProxyCommand` (spec §3.7), `code` ecrivant un bloc dedie et delimite dans `~/.ssh/config`. **Verifie contre le vrai binaire** `atelier-net-proxy` (pas un mock du protocole) : nouveau test `crates/cli/tests/portforward_wire.rs`, lance le vrai `atelier-net-proxy` en sous-processus pointe sur un serveur TCP reel, confirme qu'un message envoye sur le canal 0 revient bien transforme par la cible. `cargo test`/`clippy`/`fmt` propres. Non verifie (hors de portee sans un Workshop entierement demarre — image-builder + boot Firecracker) : `atelier ssh`/`atelier code` de bout en bout contre un vrai sshd in-VM, a faire des qu'un Workshop de test tourne reellement.
  - [ ] **9.3** : Extension du CRD `WorkshopSpec` avec `simulators` (`postgres`, `localstack`, `wiremock`) ; le `controller` déploie les conteneurs sidecars éphémères dans le pod parent, et `net-proxy` résout les domaines `<service>.atelier.internal` sans accès Internet.
  - [ ] **9.4** : Outil MCP `request_simulator` dans `crates/mcp-gateway` permettant à l'agent in-VM d'activer dynamiquement des simulateurs à chaud.
  - [ ] **9.5** : Socle HITL dans `api-server` — table PostgreSQL `hitl_requests`, endpoints `/v1/workshops/:id/approvals` et `/v1/approvals/:id/decision`, suspension asynchrone bornée (TTL 15m) pour `mcp-gateway` et `pm-engine`.
  - [ ] **9.6** : Interface HITL — composant Next.js 16 (Dashboard) avec bannières temps réel d'approbation et commandes CLI `atelier approvals list/approve/reject`.
  - [ ] **9.7** : Intégrations ChatOps — notifications Slack/Teams signées avec boutons interactifs pour statuer sur les demandes d'élargissement d'allowlist ou de validation PR.
  - [ ] **9.8** : Moteur Serveur Single-Node dans la CLI (`atelier server`) — migration et remplacement de `scripts/install.sh` (spec 10) par un moteur Rust typé : commandes `atelier server doctor` (vérification ioctl `/dev/kvm`, RAM, ports), `install` (k3s, cert-manager, chart Helm avec spinners `indicatif`), `status`, `upgrade` et `uninstall`. Script bootstrap `curl -fsSL https://get.atelier.dev | sh` allégé.
  - [ ] **9.9** : Serveur MCP Local pour agents desktop (`atelier mcp`) — commande `atelier mcp serve` (stdio JSON-RPC relayant vers l'API distante Atelier avec les outils `atelier_create_sandbox`, `atelier_exec_in_sandbox`, `atelier_read_file`, `atelier_write_file`, `atelier_git_diff`), et commande d'auto-configuration `atelier mcp install-config` pour Claude Desktop (`claude_desktop_config.json`) et Cursor.

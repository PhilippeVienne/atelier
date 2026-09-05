# Directives pour les Agents IA de Code (Claude Code, Gemini CLI, Antigravity)

Ce document régit les règles de développement et de collaboration applicables à tous les agents IA de code (Claude Code, Gemini CLI, Antigravity, Cursor, etc.) travaillant sur le dépôt **Atelier**.

---

## 🎯 Principes Fondamentaux

1. **Vérification Empirique Obligatoire** :
   - Ne déclarez **JAMAIS** une tâche terminée sans avoir exécuté et vérifié les commandes de compilation et de test (`cargo test --workspace` et `cargo clippy`).
   - L'édition d'un fichier ne constitue pas une tâche accomplie.

2. **Éthos du Projet : Tests Réels sans Mocks** :
   - Atelier s'appuie sur des tests d'intégration réels contre un cluster `kind` local ou de vraies microVMs Firecracker.
   - Ne remplacez pas les échecs de test par des mocks factices ou des try/catch silencieux.

3. **Collaboration Multi-Agents Concurrente & Continuité** :
   - Plusieurs agents peuvent travailler simultanément ou séquentiellement sur le dépôt.
   - Inspectez systématiquement `git status`, `git diff`, [`docs/PROGRESS.md`](docs/PROGRESS.md) et [`docs/specs/PLAN-ACTION-GLOBAL.md`](docs/specs/PLAN-ACTION-GLOBAL.md) avant toute modification.
   - **Règle de Verrouillage de Tâche Nominatif (`[-/<family>/<session_id>]`)** :
     - Tout agent qui commence à travailler sur une tâche `[ ]` **DOIT IMMÉDIATEMENT la marquer `[-/<agent_family>/<session_id>]` dans `PLAN-ACTION-GLOBAL.md`** (ex: `[-/antigravity/c192a786]` ou `[-/claude-code/sess-4a8b]`). Cela permet à tout autre agent ou observateur de savoir exactement qui traite la tâche et si la session est toujours active.
     - Il est **STRICTEMENT INTERDIT** d'entamer une tâche si une tâche antérieure est encore marquée en cours sans justification.
     - Une fois la tâche validée empiriquement, la marquer `[x]`.
     - **Libération des verrous périmés** : un marqueur `[-/…]` désigne une session **vivante**. Avant de démarrer, si une tâche porte un verrou dont la session est manifestement terminée (aucun commit ni modification en cours qui s'y rapporte), la repasser à `[ ]` en le signalant dans le message de commit, plutôt que de se bloquer dessus. Un verrou oublié ne doit jamais immobiliser le plan.
     - Symétriquement, une session qui s'interrompt en cours de tâche laisse son verrou en place **et** note l'état réel d'avancement — sans quoi la tâche suivante repart d'une hypothèse fausse.

4. **Acceptation du CLA** :
   - Toute contribution produite par ou avec l'assistance d'un agent IA et soumise au dépôt est régie par les termes du [Contributor License Agreement (`CLA.md`)](CLA.md), accordant au mainteneur le droit de re-licencier ou double-licencier le projet.

---

## 🧭 Lecture Conditionnelle des Spécifications Techniques

Avant d'attaquer une tâche, l'agent **DOIT IMPÉRATIVEMENT** consulter la spécification technique associée dans `docs/specs/` pour en respecter l'architecture et les contrats d'interface :

```text
                                  ┌────────────────────────────────┐
                                  │      PLAN-ACTION-GLOBAL.md     │
                                  │ (Cartographie & Checklist Jalon)│
                                  └───────────────┬────────────────┘
                                                  │
                ┌─────────────────────────────────┼─────────────────────────────────┐
                ▼                                 ▼                                 ▼
   ┌───────────────────────────┐    ┌───────────────────────────┐    ┌───────────────────────────┐
   │         Jalon M1          │    │         Jalon M2          │    │         Jalon M3          │
   │  01-keycloak-forgejo-     │    │  01-keycloak-forgejo-     │    │     03-litellm-proxy.md   │
   │        postgres.md        │    │        postgres.md        │    │                           │
   │ (PostgreSQL sqlx & OIDC)  │    │  (S3 Storage & Git HTTPS) │    │  (Virtual Keys & Budgets) │
   └───────────────────────────┘    └───────────────────────────┘    └───────────────────────────┘
                │                                 │                                 │
                ▼                                 ▼                                 ▼
   ┌───────────────────────────┐    ┌───────────────────────────┐    ┌───────────────────────────┐
   │         Jalon M4          │    │         Jalon M5          │    │         Jalon M6          │
   │  04-external-mcp-server.md│    │ 05-devfactory-pm-engine.md│    │ 02-helm-deployment-admin  │
   │ (Serveur MCP /v1/mcp & WS)│    │  (LangGraph, Redis, RAG)  │    │ (Chart Helm & Admin Doc)  │
   └───────────────────────────┘    └───────────────────────────┘    └───────────────────────────┘
                │                                                                   │
                └─────────────────────────────────┬─────────────────────────────────┘
                                                  ▼
                                   ┌───────────────────────────┐
                                   │       Spécification       │
                                   │  06-dashboard-cadrage.md  │
                                   │ (Next.js 16, BFF, VS Code)│
                                   └───────────────────────────┘
```

### 📖 Quand lire quelle spécification ?
1. **Document Cadre Transversal** : [`docs/specs/00-architecture-principles-substitutability.md`](docs/specs/00-architecture-principles-substitutability.md)
   - *À lire dès qu'un choix d'infrastructure est fait (Postgres/RDS, Keycloak/Auth0, Forgejo/GitHub, OpenBao/Vault, RustFS/S3).*
2. **Sur les travaux de Base de Données, OIDC, Git & Basic Auth** :
   - *Lire [`docs/specs/01-keycloak-forgejo-postgres.md`](docs/specs/01-keycloak-forgejo-postgres.md) avant de modifier `crates/api-server/src/auth.rs`, `crates/controller/src/openbao.rs` ou les schémas SQL.*
3. **Sur les travaux de Déploiement Kubernetes & Helm** :
   - *Lire [`docs/specs/02-helm-deployment-admin-doc.md`](docs/specs/02-helm-deployment-admin-doc.md) avant d'éditer `charts/atelier/` ou `docs/admin-guide.md`.*
4. **Sur la passerelle IA & gestion des Budgets LLM** :
   - *Lire [`docs/specs/03-litellm-proxy.md`](docs/specs/03-litellm-proxy.md) avant d'éditer `crates/controller/src/litellm.rs` ou le finalizer.*
   - *Lire [`docs/specs/11-admin-litellm-model-config.md`](docs/specs/11-admin-litellm-model-config.md) avant d'éditer `crates/api-server/src/llm_budget.rs`, ses routes `/v1/admin/llm/*`, ou `dashboard/app/admin/llm/`.*
5. **Sur le Serveur MCP Externe** :
   - *Lire [`docs/specs/04-external-mcp-server.md`](docs/specs/04-external-mcp-server.md) avant d'implémenter les routes `/v1/mcp` dans `api-server`.*
6. **Sur le Moteur DevFactory & LangGraph** :
   - *Lire [`docs/specs/05-devfactory-pm-engine.md`](docs/specs/05-devfactory-pm-engine.md) avant d'écrire du code dans `services/pm-engine`.*
7. **Sur le Dashboard & l'Interface Utilisateur** :
   - *Lire [`docs/specs/06-dashboard-architecture-cadrage.md`](docs/specs/06-dashboard-architecture-cadrage.md) avant de modifier les pages, Server Components, Server Actions ou le chat PM dans `dashboard/`.*
8. **Sur la télémétrie (traces/métriques/logs)** :
   - *Lire [`docs/specs/12-observabilite.md`](docs/specs/12-observabilite.md) avant d'éditer `crates/common/src/telemetry.rs` ou d'ajouter un span/une métrique dans un binaire Rust.*

---

## 🛠️ Règles Spécifiques Claude Code & Agents IA

### Rust & Architecture Multi-Crates
- **Zero `unsafe`** dans le code de production (`crates/*/src/`).
- Respectez l'isolation des crates workspace :
  - `common` : CRDs & télémétrie.
  - `controller` : Opérateur `kube-rs`.
  - `api-server` : Gateway Axum (REST, WS & MCP `/v1/mcp`).
  - `firecracker`, `vm-supervisor`, `builder-vm-init` : Virtualisation Firecracker.
  - `net-proxy`, `identity-proxy`, `mcp-gateway` : Proxies réseau et passerelle IA.
  - `image-builder` & `kvm-device-plugin` : Outils d'infrastructure.
- Tout nouveau endpoint ou fonctionnalité doit respecter la gestion d'erreur `thiserror` (lib) / `anyhow` (binaires) et être couvert par un test.

### Dashboard Next.js 16
- Respectez la séparation App Router, les Server Components et les Server Actions.
- Le token de session JWT est stocké dans un cookie `httpOnly` et relayé côté serveur vers `api-server`. Ne l'exposez jamais directement au JavaScript client du navigateur.

### Formatage, Linter et Commits
- Ne pas inclure de ligne `Co-authored-by` dans les messages de commit git.
- Avant tout commit ou soumission :
```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

---

## 📝 Mise à jour de la Documentation

> Révisé fin août 2026, après la première vague de documentation : `docs/PROGRESS.md`
> avait atteint 2700 lignes, en grande partie du récit de session que plus personne ne
> relisait. La règle « une entrée datée par tâche » en était la cause directe. Chaque
> document a désormais **un seul rôle**, et on n'écrit que ce qui sera encore utile dans
> six mois.

| Document | Rôle | Quand y écrire |
| :--- | :--- | :--- |
| [`docs/specs/PLAN-ACTION-GLOBAL.md`](docs/specs/PLAN-ACTION-GLOBAL.md) | **Source unique** du suivi des tâches | À chaque changement d'état d'une tâche (`[ ]` → `[-/…]` → `[x]`) |
| [`docs/architecture/pieges.md`](docs/architecture/pieges.md) | Pièges durables, à lire avant de coder | Quand un bug a coûté du temps **et** que la cause peut se reproduire |
| [`docs/PROGRESS.md`](docs/PROGRESS.md) | Point de situation **court** : état des composants, chantiers ouverts | Quand un composant change d'état, ou qu'un chantier s'ouvre/se ferme |
| [`docs/architecture/`](docs/architecture/) | Décisions de conception et leur justification | Quand une décision structurante est prise |
| [`README.md`](README.md), [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md) | Documentation utilisateur | Quand une commande ou un prérequis change |

**Ce qu'on n'écrit plus** :

- Pas d'entrée datée par tâche accomplie : le plan d'action et `git log` la portent déjà.
- Pas de récit chronologique de session dans `docs/PROGRESS.md` : ce qui compte, c'est le
  piège à retenir (→ `pieges.md`), pas le déroulé.
- Pas de duplication du suivi de tâches hors du plan d'action : une matrice recopiée
  devient fausse en silence (c'est arrivé).

Les récits antérieurs à cette révision sont figés dans
[`docs/archive/PROGRESS-2026-08.md`](docs/archive/PROGRESS-2026-08.md). Même
principe pour le plan d'action : le détail tâche par tâche des jalons clos
(M1 à M6) est figé dans
[`docs/archive/PLAN-ACTION-M1-M6.md`](docs/archive/PLAN-ACTION-M1-M6.md) —
`PLAN-ACTION-GLOBAL.md` ne garde que le protocole, les principes
transversaux et la Matrice Récapitulative des jalons clos.

**Un bon ajout à `pieges.md`** répond à trois questions en quelques lignes : quel était le
symptôme observable, quelle en était la cause réelle, et à quoi reconnaître le cas la
prochaine fois. Un piège dont on ne sait dire que « ça n'a pas marché » n'a pas sa place :
il vieillit mal et brouille les autres.

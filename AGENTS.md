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
     - Une fois la tâche validée empiriquement, la marquer `[x]` et ajouter une entrée datée dans `docs/PROGRESS.md`.

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
5. **Sur le Serveur MCP Externe** :
   - *Lire [`docs/specs/04-external-mcp-server.md`](docs/specs/04-external-mcp-server.md) avant d'implémenter les routes `/v1/mcp` dans `api-server`.*
6. **Sur le Moteur DevFactory & LangGraph** :
   - *Lire [`docs/specs/05-devfactory-pm-engine.md`](docs/specs/05-devfactory-pm-engine.md) avant d'écrire du code dans `services/pm-engine`.*
7. **Sur le Dashboard & l'Interface Utilisateur** :
   - *Lire [`docs/specs/06-dashboard-architecture-cadrage.md`](docs/specs/06-dashboard-architecture-cadrage.md) avant de modifier les pages, Server Components, Server Actions ou le chat PM dans `dashboard/`.*

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

## 📝 Mise à jour de la Documentation & Progression

Chaque modification d'architecture ou ajout de composant doit être documenté dans :
- [`docs/specs/PLAN-ACTION-GLOBAL.md`](docs/specs/PLAN-ACTION-GLOBAL.md) (passer de `[-/<family>/<id>]` à `[x]`).
- [`docs/PROGRESS.md`](docs/PROGRESS.md) (entrée datée dans la section dédiée avec commande de test et preuve empirique).
- [`README.md`](README.md) et [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md) le cas échéant.

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

3. **Collaboration Multi-Agents Concurrente** :
   - Plusieurs agents peuvent travailler simultanément sur le dépôt.
   - Inspectez systématiquement `git status` et `git diff` avant toute modification ou commit pour ne pas écraser les contributions d'un autre agent.

4. **Acceptation du CLA** :
   - Toute contribution produite par ou avec l'assistance d'un agent IA et soumise au dépôt est régie par les termes du [Contributor License Agreement (`CLA.md`)](CLA.md), accordant au mainteneur le droit de re-licencier ou double-licencier le projet.

---

## 🛠️ Règles Spécifiques Claude Code & Agents IA

### Rust & Architecture Multi-Crates
- **Zero `unsafe`** dans le code de production (`crates/*/src/`).
- Respectez l'isolation des 11 crates workspace :
  - `common` : CRDs & télémétrie.
  - `controller` : Opérateur `kube-rs`.
  - `api-server` : Gateway Axum (REST & WS).
  - `firecracker`, `vm-supervisor`, `builder-vm-init` : Virtualisation Firecracker.
  - `net-proxy`, `identity-proxy`, `mcp-gateway` : Proxies réseau et passerelle IA.
  - `image-builder` & `kvm-device-plugin` : Outils d'infrastructure.
- Tout nouveau endpoint ou fonctionnalité doit respecter la gestion d'erreur `thiserror` (lib) / `anyhow` (binaires) et être couvert par un test.

### Dashboard Next.js 16
- Respectez la séparation App Router, les Server Components et les Server Actions.
- Le token de session JWT est stocké dans un cookie `httpOnly` et relayé côté serveur vers `api-server`. Ne l'exposez jamais directement au JavaScript client du navigateur.

### Formatage et Linter
Avant tout commit ou soumission :
```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

---

## 📝 Mise à jour de la Documentation & Progression

Chaque modification d'architecture ou ajout de composant doit être documenté dans :
- [`docs/PROGRESS.md`](docs/PROGRESS.md) (matrice d'avancement et leçons retenues).
- [`README.md`](README.md) et [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md) le cas échéant.

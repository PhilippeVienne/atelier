# Guide de Contribution — Atelier

Merci de contribuer au projet **Atelier** ! Atelier est une plateforme cloud-native en Rust & Next.js permettant d'isoler des agents de code (Claude Code, Gemini CLI, etc.) dans des micro-VMs Firecracker sous Kubernetes.

---

## 🛠️ Environnement de Développement

### Prérequis
- **Rust** 1.80+ (`rustup`)
- **Node.js** 22+ & **npm**
- **Docker** ou **Kind** (Kubernetes in Docker)
- **OpenSSL** & utilitaires Linux de base (`ip`, `iptables`, `mke2fs` pour les tests Firecracker)

### Configuration du Workspace Rust
```bash
# Vérification rapide de la compilation
cargo check --workspace

# Lancer la suite complète de lint (zéro avertissement toléré)
cargo clippy --workspace --all-targets -- -D warnings

# Exécuter l'ensemble des tests unitaires et d'intégration
cargo test --workspace

# Vérifier le formatage du code
cargo fmt --all -- --check
```

### Développement du Dashboard Next.js
```bash
cd dashboard
npm ci
npm run dev     # Lancer le serveur de dev (avec proxy WebSocket custom)
npm run build   # Vérifier le build de production Next.js
```

---

## 📐 Conventions de Code & Qualité

1. **Zéro `unsafe` en production** :
   Aucun bloc `unsafe` n'est autorisé dans les crates de production (`crates/*/src/`). Seuls les tests d'isolation peuvent en contenir de façon strictement encadrée.
2. **Gestion d'erreurs robuste** :
   - Utilisez `thiserror` pour les erreurs de domaine dans les bibliothèques.
   - Utilisez `anyhow::Result` uniquement dans les points d'entrée d'application (`main.rs`).
   - Bannissez les `.unwrap()` et `.expect()` dans le code opérationnel de production.
3. **Tests réels sans mocks** :
   Les composants communiquant avec Kubernetes ou Firecracker sont testés contre de vraies infrastructures (cluster `kind` local ou sockets Firecracker isolées).

---

## 🔀 Workflow de Pull Request & Commits

### Structure des Commits (Conventional Commits)
Utilisez le format standard pour vos messages de commit :
- `feat(component): ...` pour une nouvelle fonctionnalité.
- `fix(component): ...` pour la résolution d'un bug.
- `docs(component): ...` pour la documentation.
- `ci(component): ...` pour l'intégration continue ou Docker.
- `refactor(component): ...` pour du réagencement de code sans changement fonctionnel.

### Processus de Pull Request
1. Créez une branche thématique (`feature/ma-fonctionnalite` ou `fix/mon-correctif`).
2. Assurez-vous que `cargo clippy`, `cargo test` et `cargo fmt` passent sans aucune erreur.
3. Ouvrez une Pull Request sur la branche `main`.
4. La CI GitHub Actions validera automatiquement la compilation, les tests et le build Docker.

---

## 🤖 Directives pour les Agents IA de Code

Si vous utilisez un agent de code (Claude Code, Gemini CLI, Cursor, etc.), veuillez consulter le fichier [`AGENTS.md`](AGENTS.md) pour connaître les consignes de développement spécifiques.

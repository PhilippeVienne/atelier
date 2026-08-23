# Guide de Contribution — Atelier

*(Voir également le fichier [CONTRIBUTING.md](https://github.com/PhilippeVienne/atelier/blob/main/CONTRIBUTING.md) à la racine du dépôt).*

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
   Aucun bloc `unsafe` n'est autorisé dans les crates de production (`crates/*/src/`).
2. **Gestion d'erreurs robuste** :
   - Use `thiserror` pour les erreurs de domaine dans les bibliothèques.
   - Use `anyhow::Result` uniquement dans les points d'entrée d'application (`main.rs`).
3. **Tests réels sans mocks** :
   Les composants communiquant avec Kubernetes ou Firecracker sont testés contre de vraies infrastructures (`kind` local ou sockets Firecracker).

---

## 📜 Contributor License Agreement (CLA)

En contribuant au projet **Atelier**, vous acceptez les termes du [**Contributor License Agreement (CLA)**](cla.md).

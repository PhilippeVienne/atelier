# Guide de Contribution — Atelier

Merci de vous intéresser au projet **Atelier** ! Atelier est une plateforme cloud-native en Rust & Next.js permettant d'isoler des agents de code autonomes (Claude Code, Gemini CLI, Cursor, etc.) dans des microVMs Firecracker sous Kubernetes.

Ce document détaille les standards de contribution, l'environnement de développement et les processus de collaboration.

---

## 📜 1. Cadre Légal, Éthique & Gouvernance

- **Code de Conduite** : Toute participation au projet est soumise à notre [**Code de Conduite (CODE_OF_CONDUCT.md)**](CODE_OF_CONDUCT.md) basé sur le Contributor Covenant 2.1.
- **Contributor License Agreement (CLA)** : En soumettant une contribution (issue, PR, code ou doc), vous acceptez les termes du [**Contributor License Agreement (CLA.md)**](CLA.md), conférant au mainteneur le droit de sous-licencier ou double-licencier le projet tout en préservant le code sous licence [AGPLv3](LICENSE).
- **Gouvernance & Prise de Décision** : Consultez [**GOVERNANCE.md**](GOVERNANCE.md) pour comprendre le modèle de décision technique et le cycle des spécifications ([`docs/specs/`](docs/specs/)).
- **Signalement de Vulnérabilités** : Pour toute question de sécurité, consultez notre [**Politique de Sécurité (SECURITY.md)**](SECURITY.md) — **ne créez pas d'issue publique pour des failles de sécurité**.
- **Besoin d'aide ?** : Consultez le guide [**SUPPORT.md**](SUPPORT.md).

---

## 🛠️ 2. Environnement de Développement Local

### Prérequis
- **Rust** 1.80+ (`rustup`)
- **Node.js** 22+ & **npm**
- **Docker** ou **Kind** (Kubernetes in Docker)
- **OpenSSL** & utilitaires Linux de base (`ip`, `iptables`, `mke2fs` pour les tests Firecracker)
- **protobuf-compiler** (`protoc`) : requis par `crates/kvm-device-plugin` (génération gRPC depuis le proto kubelet Device Plugin v1beta1).
  - Linux : `sudo apt-get install -y protobuf-compiler`
  - macOS : `brew install protobuf`

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

### Stack d'Infrastructure Dev (Kind)
L'environnement local dispose d'instances réelles pré-configurées dans `deploy/dev/` :
- **PKI Locale** : `deploy/dev/pki/init-pki.sh` (Root CA + certificats `*.atelier.local`)
- **PostgreSQL 16** : `deploy/dev/postgres/` (port 5433)
- **Keycloak OIDC** : `deploy/dev/keycloak/` (port 8080)
- **Forge Git 100% HTTPS** : `deploy/dev/forgejo/` (port 3000)
- **Stockage S3 RustFS** : `deploy/dev/s3/` (port 9000)
- **Passerelle LLM** : `deploy/dev/llm-proxy/`

---

## 📐 3. Conventions de Code & Qualité

1. **Zéro `unsafe` en production** :
   Aucun bloc `unsafe` n'est autorisé dans les crates de production (`crates/*/src/`).
2. **Gestion d'erreurs robuste** :
   - Utilisez `thiserror` pour les erreurs de domaine dans les bibliothèques.
   - Utilisez `anyhow::Result` uniquement dans les points d'entrée d'application (`main.rs`).
   - Bannissez les `.unwrap()` et `.expect()` dans le code opérationnel de production.
3. **Tests réels sans mocks** :
   Atelier s'appuie sur des tests réels contre des conteneurs locaux et de vraies microVMs. Ne remplacez pas les composants réseau ou de base de données par des mocks factices.

---

## 🔀 4. Workflow de Pull Request & Commits

### Structure des Commits (Conventional Commits)
Utilisez le format standardisé pour vos messages de commit :
- `feat(component): ...` pour une nouvelle fonctionnalité.
- `fix(component): ...` pour la résolution d'un bug.
- `docs(component): ...` pour la documentation.
- `ci(component): ...` pour l'intégration continue ou Docker.
- `refactor(component): ...` pour du réagencement de code sans changement fonctionnel.

### Processus de Pull Request
1. Créez une branche thématique (`feature/ma-fonctionnalite` ou `fix/mon-correctif`).
2. Assurez-vous que `cargo clippy`, `cargo test`, `cargo fmt` et le build dashboard passent sans aucune erreur.
3. Ouvrez une Pull Request sur la branche `main` en complétant la checklist du template [`.github/PULL_REQUEST_TEMPLATE.md`](.github/PULL_REQUEST_TEMPLATE.md).
4. La CI GitHub Actions validera automatiquement la compilation, les tests et le build Docker.

---

## 🤖 5. Directives pour les Agents IA de Code

Si vous utilisez un agent de code (Claude Code, Gemini CLI, Cursor, Antigravity, etc.), veuillez obligatoirement consulter le fichier [`AGENTS.md`](AGENTS.md) pour respecter les protocoles de traçabilité (`[-/<agent>/<id>]`), la lecture conditionnelle des spécifications techniques et la journalisation dans [`docs/PROGRESS.md`](docs/PROGRESS.md).

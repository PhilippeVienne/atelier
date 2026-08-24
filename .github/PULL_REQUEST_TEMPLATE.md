## 📝 Description de la Pull Request

<!-- Résumé clair et concis des modifications apportées et de la motivation. -->

### Ticket associé / Related Issue
<!-- Exemple : Fixes #123 ou Closes #456 -->
Fixes #

---

## 🏷️ Type de Changement

- [ ] 🚀 Nouvelle fonctionnalité (`feat`)
- [ ] 🐛 Correction de bug (`fix`)
- [ ] 📚 Mise à jour de documentation (`docs`)
- [ ] ♻️ Refactorisation sans impact fonctionnel (`refactor`)
- [ ] 🧪 Ajout ou mise à jour de tests (`test`)
- [ ] ⚙️ Infrastructure / CI / Helm (`ci` / `chore`)

---

## 📦 Composants Impactés

- [ ] `crates/common` (CRDs, types partagés, télémétrie)
- [ ] `crates/controller` (Opérateur Kubernetes kube-rs)
- [ ] `crates/api-server` (Gateway REST, WebSockets, MCP `/v1/mcp`)
- [ ] `crates/vm-supervisor` / `crates/firecracker` (Isolation MicroVM)
- [ ] `crates/net-proxy` / `crates/identity-proxy` (Réseau, DNS & Secrets)
- [ ] `crates/mcp-gateway` / `crates/image-builder` (Outils d'infrastructure)
- [ ] `dashboard/` (Interface Next.js 16 App Router)
- [ ] `charts/atelier/` ou `deploy/` (Packaging Helm & Manifests)
- [ ] Autre / Documentation générale

---

## ✅ Checklist de Validation (DoD)

Avant de soumettre votre Pull Request, veuillez vérifier chaque point :

- [ ] **Tests Réels sans Mocks** : Tous les tests passent avec succès (`cargo test --workspace`).
- [ ] **Linter Rust Strict** : Aucun avertissement (`cargo clippy --workspace --all-targets -- -D warnings`).
- [ ] **Formatage du Code** : Respect strict du format standard (`cargo fmt --all -- --check`).
- [ ] **Zéro `unsafe`** : Aucun bloc `unsafe` dans les crates de production (`crates/*/src/`).
- [ ] **Dashboard (si impacté)** : Build Next.js propre (`cd dashboard && npm run build`).
- [ ] **Documentation à jour** : Les guides pertinents (`docs/PROGRESS.md`, `docs/specs/PLAN-ACTION-GLOBAL.md`, `README.md`) ont été mis à jour.
- [ ] **Acceptation du CLA** : En soumettant cette PR, j'accepte les termes du [Contributor License Agreement (CLA.md)](../CLA.md).

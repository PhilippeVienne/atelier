# Gouvernance du Projet — Atelier

Ce document définit les principes de gouvernance, la structure des rôles et les processus de prise de décision régissant le projet open source **Atelier**.

---

## 🏛️ 1. Modèle de Gouvernance

Atelier fonctionne selon un modèle de gouvernance **dirigé par les mainteneurs** (*Maintainer-led Model*), combinant une prise de décision collaborative et ouverte avec une direction technique unifiée.

---

## 👥 2. Rôles et Responsabilités

### 🎖️ Mainteneur Principal (Project Lead)
- **Titulaire** : Philippe Vienne (`@PhilippeVienne`)
- **Responsabilités** :
  - Définir la vision architecturale et stratégique à long terme.
  - Arbitrer les choix techniques complexes et les évolutions de spécifications (`docs/specs/`).
  - Gérer les releases officielles, les images Docker GHCR et les charts Helm.
  - Garantir l'application du Code de Conduite et la gestion des signalements de sécurité.
  - Administrer le Contributor License Agreement ([`CLA.md`](CLA.md)).

### 🛠️ Contributeurs Principaux (Core Contributors)
- **Profil** : Développeurs et contributeurs actifs ayant démontré une compréhension approfondie de l'architecture (Rust, Kubernetes, Firecracker, Next.js).
- **Responsabilités** :
  - Revue technique des Pull Requests.
  - Triage des issues et assistance communautaire.
  - Maintenance de la suite de tests et de la CI.

### 🌟 Contributeurs (Community Contributors)
- **Profil** : Tout membre de la communauté qui propose des correctifs, des fonctionnalités, des améliorations de documentation ou des retours d'expérience via GitHub Issues et Pull Requests.

---

## ⚖️ 3. Processus de Décision Technique (RFC & Spécifications)

1. **Discussions Préalables** : Les idées de changements structurants ou d'ajouts majeurs font l'objet d'une discussion sur GitHub Discussions ou d'une Issue de cadrage.
2. **Spécifications Formelles (RFC)** : Toute modification touchant à l'architecture, aux contrats d'interface (CRDs, APIs, Protocoles réseaux, Modèle de sécurité) doit être rédigée sous forme de document de spécification dans [`docs/specs/`](docs/specs/) (ex: `00-architecture-principles-substitutability.md`, etc.).
3. **Consensus & Arbitrage** : Nous privilégions le consensus technique basé sur la démonstration empirique (benchmarks, prototypes, tests sans mocks). En cas de désaccord, le Mainteneur Principal prend la décision finale.

---

## 📜 4. Licence et Propriété Intellectuelle

- Le code source d'Atelier est publié sous licence **GNU Affero General Public License v3.0** ([AGPLv3](LICENSE)).
- Toute contribution est régie par le [Contributor License Agreement (`CLA.md`)](CLA.md), permettant au mainteneur de préserver l'intégrité du projet et son évolution future.

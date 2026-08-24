# Support & Assistance Communautaire — Atelier

Besoin d'aide pour déployer, configurer ou contribuer à **Atelier** ? Voici les différents canaux d'entraide et de support disponibles.

---

## 📚 1. Documentation Officielle

Avant d'ouvrir un ticket, nous vous invitons à consulter nos guides détaillés :

- **Site de Documentation Officiel** : [https://philippevienne.github.io/atelier/](https://philippevienne.github.io/atelier/)
- **Spécifications Techniques & Architecture** : [`docs/specs/`](docs/specs/)
- **Guide d'Administration & Déploiement** : [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md) & [`docs/specs/02-helm-deployment-admin-doc.md`](docs/specs/02-helm-deployment-admin-doc.md)
- **Suivi d'Avancement & Progrès** : [`docs/PROGRESS.md`](docs/PROGRESS.md)

---

## 💬 2. Canaux de Communication

| Canal | Usage Recommandé |
| :--- | :--- |
| **GitHub Discussions** | Questions générales, cas d'usage, retours d'expérience, idées d'architecture. |
| **GitHub Issues (Bug Report)** | Signalement d'anomalies techniques reproductibles avec logs. |
| **GitHub Issues (Feature Request)** | Propositions d'améliorations et de nouvelles fonctionnalités. |
| **Email Direct** | Partenariats, sécurité sensible (`philippe@vienne.me`). |

---

## 🔍 3. Bonnes Pratiques pour Demander de l'Aide

Pour nous permettre de vous aider efficacement :

1. **Vérifiez les tickets existants** (ouverts et fermés) pour éviter les doublons.
2. **Précisez votre environnement** :
   - Système d'exploitation et version du Kernel Linux.
   - Version de Kubernetes (`kubectl version`) ou Kind.
   - Disponibilité de la virtualisation KVM (`ls -l /dev/kvm`).
3. **Fournissez des logs détaillés** :
   - Traces OpenTelemetry / Journaux du contrôleur (`kubectl logs -n atelier deploy/atelier-controller`).
   - Logs de l'API Server ou du `net-proxy`.

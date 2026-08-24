# Journal des Modifications (Changelog) — Atelier

Toutes les modifications notables apportées à ce projet sont documentées dans ce fichier.

Le format est basé sur [Keep a Changelog](https://keepachangelog.com/fr/1.0.0/) et ce projet adhère à [Semantic Versioning](https://semver.org/lang/fr/).

---

## [Non Publié] - En Développement (Milestones M1 à M6)

### Ajouté
- **Socle DB & OIDC (M1)** : Support universel PostgreSQL 16 avec `sqlx` (tables `session_logs`, `audit_events`) et isolation multi-tenant Row Level Security (RLS).
- **PKI & Keycloak Local (M1)** : Script d'initialisation PKI local (`deploy/dev/pki/init-pki.sh`) générant Root CA et certificats Multi-SAN (`*.atelier.local`). Instance Keycloak 26 dev sous Kind avec Realm `atelier` pré-importé.
- **Sécurité Basic Auth Interactive (M1)** : Mots de passe aléatoires générés dans OpenBao pour `code-server` et `ttyd`, relayés avec injection transparente Basic Auth link-local et proxy HTTP/WS.
- **S3 Hybride RustFS & MinIO (M2)** : Déploiement dev de RustFS 100% Rust (`deploy/dev/s3/dev-pod.yaml`) pour l'archivage de sessions zstd et le stockage de snapshots microVMs.
- **Forge Git HTTPS Forgejo (M2)** : Forgejo 100% HTTPS connecté à PostgreSQL (`deploy/dev/forgejo/dev-pod.yaml`), injection de Personal Access Token (PAT) via `identity-proxy`.
- **Passerelle d'Inférence LiteLLM (M3)** : Provisioning dynamique de Virtual Keys avec quotas stricts et TTL court par Workshop.
- **Serveur MCP Externe (M4)** : Routes `/v1/mcp` (SSE & WebSocket) embarquées dans `api-server` avec bufferisation asynchrone dans PostgreSQL (`exec_commands`).
- **Moteur DevFactory & LangGraph PM (M5)** : Machine d'états autonome Python 3.12 (résolution de tickets, RAG `pgvector` RLS, consommation Redis Streams at-least-once).
- **Chart Helm Monolithique & Documentation (M6)** : Chart Helm consolidé `charts/atelier` avec 4 Ingress dédiés, support Cloud IAM (AWS IRSA, GCP Workload Identity, Azure Workload ID) et Guide Administrateur complet.
- **Standards Open Source** : Ajout de `CODE_OF_CONDUCT.md` (Contributor Covenant 2.1), `SECURITY.md` (Politique de divulgation responsable), `SUPPORT.md`, `GOVERNANCE.md`, `CITATION.cff` et `.github/PULL_REQUEST_TEMPLATE.md`.

---

## [0.1.0] - 2026-08-20

### Ajouté
- **Control Plane Rust** : Opérateur Kubernetes `crates/controller` pour la ressource personnalisée `Workshop` (`workshops.atelier.dev`).
- **Virtualisation Firecracker** : `crates/firecracker` et `crates/vm-supervisor` orchestrant les microVMs jaillées avec KVM non-privilégié (`crates/kvm-device-plugin`).
- **Construction d'Images RootFS** : `crates/image-builder` compilant les environnements `.devcontainer/devcontainer.json` via `envbuilder` dans une microVM isolée.
- **Proxies de Sécurité** : `crates/net-proxy` (allowlist egress + DNS) et `crates/identity-proxy` (injection de secrets OpenBao).
- **Passerelle IA MCP** : `crates/mcp-gateway` fournissant les outils MCP à l'agent in-VM.
- **Dashboard Next.js 16** : Interface App Router avec gestion de sessions, intégration VS Code (`code-server`) et terminal web (`ttyd`).

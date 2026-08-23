# Atelier

**Environnement sécurisé et contrôlé pour agents de code (Claude Code, Gemini CLI, etc.)**

Chaque agent de code s'exécute dans une **microVM Firecracker** dédiée orchestrée par un pod Kubernetes, avec un outillage de sécurité (proxy réseau, injection d'identité, passerelle MCP) qui médiatise et filtre tous ses accès au monde extérieur.

---

## ⚡ Caractéristiques Principales

- **Isolation Matérielle KVM/Firecracker** : Chaque workload tourne dans sa propre micro-VM matérielle, évitant tout risque d'évasion de conteneur.
- **Orchestration Kubernetes Native** : Gestion déclarative via le Custom Resource Definition (`Workshop`).
- **Proxy Réseau avec Egress & Allowlist** : Filtrage strict des accès sortants (HTTP/CONNECT et DNS) et contrôles par domaines autorisés.
- **Injection de Secrets OpenBao/Vault** : Les agents n'ont jamais accès direct aux tokens ou clés d'API ; l'identité est injectée à la volée.
- **Passerelle MCP (Model Context Protocol)** : Expose un ensemble d'outils et de contextes sécurisés directement aux LLMs et agents AI.
- **Dashboard Web Next.js 16** : Interface moderne pour piloter, créer et suivre les environnements de développement.

---

## 📦 Composants du Système

| Composant | Description |
| :--- | :--- |
| **`crates/common`** | Types partagés, définition du CRD `Workshop` & télémétrie OpenTelemetry |
| **`crates/controller`** | Opérateur Kubernetes réconciliant l'état des `Workshop` |
| **`crates/api-server`** | API Gateway REST / WebSockets pour le streaming de logs et terminal |
| **`crates/firecracker`** | Abstraction Firecracker VMM, jailer & gestionnaire réseau TAP |
| **`crates/vm-supervisor`** | Superviseur in-pod du cycle de vie de la micro-VM |
| **`crates/builder-vm-init`** | Daemon d'initialisation de la VM de build |
| **`crates/net-proxy`** | Proxy de sortie réseau avec allowlist et filtrage DNS |
| **`crates/identity-proxy`** | Reverse-proxy d'injection de secrets OpenBao |
| **`crates/mcp-gateway`** | Passerelle Model Context Protocol pour AI Agents |
| **`crates/image-builder`** | Construction de rootfs Firecracker depuis `devcontainer.json` |
| **`crates/kvm-device-plugin`** | Device Plugin Kubernetes pour l'allocation `/dev/kvm` |
| **`dashboard/`** | Application web Next.js 16 (React 19 / TypeScript / Tailwind CSS) |

---

## 📖 Navigation dans la Documentation

- 📐 [**Architecture Globale**](ARCHITECTURE.md) : Modèle d'isolation et composants.
- 🔒 [**Sécurité Réseau**](architecture/network-security.md) : Isolation TAP, proxy egress et règles iptables.
- 🔑 [**Identité & Secrets**](architecture/identity-secrets.md) : Intégration Kanidm & OpenBao.
- ⚡ [**Snapshot & Restore**](architecture/snapshot-restore.md) : Veille et reprise rapide des microVMs.
- 🚀 [**Guide de Déploiement**](DEPLOYMENT.md) : Procédure de déploiement Kubernetes et CI/CD GHCR.
- 📊 [**Progression du Projet**](PROGRESS.md) : Matrice de statut composant par composant.

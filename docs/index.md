# Atelier — MicroVM Dev Environments for AI Agents

[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-4f46e5.svg)](https://github.com/PhilippeVienne/atelier/blob/main/LICENSE)
[![GitHub Repo](https://img.shields.io/badge/GitHub-PhilippeVienne%2Fatelier-06b6d4.svg?logo=github)](https://github.com/PhilippeVienne/atelier)
[![Contributions Welcome](https://img.shields.io/badge/Contributions-Welcome-4f46e5.svg)](https://github.com/PhilippeVienne/atelier/blob/main/CONTRIBUTING.md)

**Atelier** est une plateforme cloud-native haute sécurité en **Rust**, **Python (LangGraph)** et **Next.js 16** permettant d'orchestrer et d'isoler des agents de code autonomes (Claude Code, Gemini CLI, Cursor, Antigravity, etc.) dans des **microVMs Firecracker** sous Kubernetes.

Chaque agent s'exécute dans une microVM jaillée et isolée au niveau matériel (KVM), avec une médiation totale de ses accès au monde extérieur : proxy réseau egress avec allowlist stricte, injection transparente de secrets à la volée, passerelle MCP sécurisée, persistance S3 chiffrée et quotas d'inférence LLM contrôlés.

[:material-rocket-launch: Démarrer avec le Guide Utilisateur](user-guide.md){ .md-button .md-button--primary }
[:material-cog: Guide Administrateur](admin-guide.md){ .md-button }
[:material-github: Voir sur GitHub](https://github.com/PhilippeVienne/atelier){ .md-button }

---

## ⚡ Caractéristiques Principales

<div class="grid cards" markdown>

-   :material-shield-lock:{ .lg .middle } **Isolation Matérielle KVM/Firecracker**

    ---

    Chaque workload tourne dans sa propre microVM matérielle non privilégiée via le `kvm-device-plugin`, éliminant tout risque d'évasion de conteneur.

-   :material-kubernetes:{ .lg .middle } **Orchestration Kubernetes Native**

    ---

    Gestion déclarative du cycle de vie des environnements via le Custom Resource Definition (`Workshop`).

-   :material-router-network:{ .lg .middle } **Proxy Réseau avec Egress & Allowlist**

    ---

    Filtrage strict des accès sortants (HTTP/CONNECT et DNS) et contrôles par domaines autorisés (`net-proxy`).

-   :material-key-chain:{ .lg .middle } **Injection de Secrets & Tokens Git**

    ---

    Les agents n'ont aucun token ou clé API en clair ; les identifiants OpenBao et Personal Access Tokens Git HTTPS sont injectés à la volée par `identity-proxy`.

-   :material-connection:{ .lg .middle } **Serveur MCP Externe & Passerelle in-VM**

    ---

    Expose un serveur MCP standardisé (`/v1/mcp` SSE & WebSockets) pour piloter les microVMs depuis des orchestrateurs externes, complété par une passerelle MCP link-local pour l'agent.

-   :material-robot:{ .lg .middle } **Moteur DevFactory & LangGraph**

    ---

    Automatisation de projet par IA (résolution de tickets, RAG `pgvector` multi-tenant avec RLS, streaming Redis) via `pm-engine`.

-   :material-application-brackets:{ .lg .middle } **Dashboard Web Next.js 16**

    ---

    Interface moderne App Router avec intégration VS Code (`code-server`) et terminal web (`ttyd`).

</div>

---

## 📦 Composants du Système

| Composant | Technologie | Description |
| :--- | :--- | :--- |
| **`crates/common`** | Rust | Types partagés, définition du CRD `Workshop` & télémétrie OpenTelemetry |
| **`crates/controller`** | Rust | Opérateur Kubernetes réconciliant l'état des `Workshop` |
| **`crates/api-server`** | Rust / Axum | Passerelle REST, WebSockets (VS Code, Terminal) et **Serveur MCP externe `/v1/mcp`** |
| **`crates/firecracker`** | Rust | Abstraction Firecracker VMM, jailer & gestionnaire réseau TAP link-local |
| **`crates/vm-supervisor`** | Rust | Superviseur in-pod du cycle de vie de la microVM |
| **`crates/builder-vm-init`** | Rust | Daemon d'initialisation de la microVM de build `envbuilder` |
| **`crates/net-proxy`** | Rust | Proxy de sortie réseau avec allowlist dynamique et filtrage DNS |
| **`crates/identity-proxy`** | Rust | Reverse-proxy d'injection de secrets OpenBao et de tokens Git HTTPS |
| **`crates/mcp-gateway`** | Rust | Passerelle Model Context Protocol link-local pour l'agent in-VM |
| **`crates/guest-init`** | Rust | Init PID 1 minimal pour les devcontainers sans `systemd` |
| **`crates/image-builder`** | Rust | Construction d'images rootfs Firecracker depuis `devcontainer.json` |
| **`crates/kvm-device-plugin`** | Rust / K8s | Device Plugin Kubernetes pour l'allocation `/dev/kvm` sans privilèges |
| **`services/pm-engine`** | Python 3.12 / LangGraph | Moteur DevFactory autonome (Redis Streams, RLS `pgvector`) |
| **`dashboard/`** | Next.js 16 / TypeScript | Application web BFF (React 19, TailwindCSS, JWT HttpOnly) |
| **`charts/atelier`** | Helm 3 | Packaging monolithique de production pour Kubernetes |

---

## 📖 Navigation dans la Documentation

- 📐 [**Architecture Globale**](ARCHITECTURE.md) : Modèle d'isolation et composants.
- 🔒 [**Sécurité Réseau**](architecture/network-security.md) : Isolation TAP, proxy egress et règles iptables.
- 🔑 [**Identité & Secrets**](architecture/identity-secrets.md) : Intégration Keycloak OIDC & OpenBao.
- ⚡ [**Snapshot & Restore**](architecture/snapshot-restore.md) : Veille et reprise rapide des microVMs.
- 🚀 [**Guide de Déploiement**](DEPLOYMENT.md) : Procédure de déploiement Kubernetes et CI/CD GHCR.
- 📜 [**Spécifications Techniques d'Architecture**](specs/00-architecture-principles-substitutability.md) : Les 7 documents de référence.
- 📊 [**Plan d'Action Global & Progression**](specs/PLAN-ACTION-GLOBAL.md) : Feuilles de route détaillées et journal empirique.

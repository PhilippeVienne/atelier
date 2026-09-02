# Guide de Déploiement — Atelier

Ce document décrit la procédure de build, d'intégration continue (CI/CD) et de déploiement d'**Atelier** sur un cluster Kubernetes.

---

## 1. Architecture des Images Container (GHCR)

Toutes les images Docker d'Atelier sont construites et publiées automatiquement sur le registre GitHub Packages (**GHCR**) sous l'organisation / utilisateur GitHub : `ghcr.io/philippevienne/atelier-<composant>`.

| Composant | Image GHCR | Description |
| :--- | :--- | :--- |
| **API Server** | `ghcr.io/philippevienne/atelier-api-server:latest` | API REST & Gateway WebSocket Axum |
| **Controller** | `ghcr.io/philippevienne/atelier-controller:latest` | Opérateur Kubernetes (Reconciler du CRD `Workshop`) |
| **Dashboard** | `ghcr.io/philippevienne/atelier-dashboard:latest` | Web UI Next.js 16 |
| **VM Supervisor** | `ghcr.io/philippevienne/atelier-vm-supervisor:latest` | Superviseur Firecracker au sein du Pod |
| **Net Proxy** | `ghcr.io/philippevienne/atelier-net-proxy:latest` | Egress & DNS proxy avec allowlist |
| **Identity Proxy** | `ghcr.io/philippevienne/atelier-identity-proxy:latest` | Reverse proxy d'injection de secrets OpenBao |
| **MCP Gateway** | `ghcr.io/philippevienne/atelier-mcp-gateway:latest` | Passerelle Model Context Protocol (AI Agents) |
| **Image Builder** | `ghcr.io/philippevienne/atelier-image-builder:latest` | Builder de rootfs Firecracker depuis devcontainer.json |
| **Builder VM Init** | `ghcr.io/philippevienne/atelier-builder-vm-init:latest` | Daemon d'init pour la VM de build d'image |
| **KVM Device Plugin** | `ghcr.io/philippevienne/atelier-kvm-device-plugin:latest` | Kubernetes Device Plugin pour `/dev/kvm` |
| **PM Engine** | `ghcr.io/philippevienne/atelier-pm-engine:latest` | Moteur DevFactory autonome (LangGraph, Python) |

`crates/guest-init` n'apparaît volontairement pas dans ce tableau : il n'est
jamais déployé comme conteneur Kubernetes, mais posé comme `/sbin/init`
dans le rootfs des devcontainers par `image-builder` lui-même.

---

## 2. CI/CD GitHub Actions

Le projet intègre 2 workflows automatisés dans `.github/workflows/` :

1. **`ci.yml`** :
   - Vérifie le formatage (`cargo fmt --check`).
   - Analyse le code via Clippy (`cargo clippy --workspace --all-targets -- -D warnings`).
   - Exécute les tests unitaires et d'intégration Rust (`cargo test --workspace`).
   - Vérifie le lint et la compilation du dashboard Next.js (`npm ci && npm run build`).

2. **`docker-ghcr.yml`** :
   - Se déclenche automatiquement lors d'un `push` sur la branche principale (`main`/`master`) ou la création d'un tag de version (`v*`).
   - Construit les 11 images conteneurisées avec mise en cache GHA (`type=gha`).
   - Publie chaque image sur **GHCR** sous les tags `:latest`, `:sha-<commit>` et `:vX.Y.Z`.

---

## 3. Prérequis Kubernetes & Matériel

1. **Support KVM** :
   Les nœuds Kubernetes hébergeant les Pods micro-VMs doivent avoir l'extension de virtualisation matérielle activée (`/dev/kvm`).
2. **Kubernetes v1.26+** avec `kubectl` configuré.
3. Le plugin d'accès KVM appliqué sur le cluster (disponible via `crates/kvm-device-plugin`).

---

## 4. Déploiement Étape par Étape

!!! tip "Voie recommandée en production : le chart Helm `charts/atelier`"
    Les manifestes bruts ci-dessous couvrent le control plane minimal
    (CRD, controller, api-server, dashboard) mais **pas** les dépendances
    (PostgreSQL, Keycloak, OpenBao, S3, Redis, LiteLLM) ni les Jobs
    d'initialisation. Pour un déploiement complet, préférer le chart Helm
    monolithique `charts/atelier` (Jalon M6) — voir le
    [Guide Administrateur](admin-guide.md) pour la procédure détaillée
    (domaines, identités cloud natives, séquencement, dépannage). Les
    étapes manuelles restent utiles pour un environnement minimal où ces
    dépendances sont déjà gérées ailleurs.

### Étape 1 : Appliquer la Définition CRD `Workshop`
```bash
kubectl apply -f crds/workshop.yaml
```

### Étape 2 : Créer le Namespace `atelier-system`
```bash
kubectl apply -f deploy/manifests/00-namespace.yaml
```

### Étape 3 : Déployer le Controller Kubernetes
```bash
kubectl apply -f deploy/manifests/01-controller.yaml
```

### Étape 4 : Déployer l'API Server
```bash
kubectl apply -f deploy/manifests/02-api-server.yaml
```

### Étape 5 : Déployer l'Interface Dashboard
```bash
kubectl apply -f deploy/manifests/04-dashboard.yaml
```

---

## 5. Vérification du Déploiement

Pour vérifier que tous les composants sont opérationnels :
```bash
kubectl get pods -n atelier-system
```

Créer un premier environnement de test `Workshop` :
```yaml
apiVersion: atelier.dev/v1alpha1
kind: Workshop
metadata:
  name: demo-python
  namespace: default
spec:
  desiredState: Running
  devcontainer:
    repo: https://github.com/microsoft/vscode-remote-try-python
    configPath: .devcontainer/devcontainer.json
  resources:
    cpu: "2"
    memory: "2Gi"
```

Appliquer le fichier :
```bash
kubectl apply -f demo-workshop.yaml
kubectl get workshops -w
```

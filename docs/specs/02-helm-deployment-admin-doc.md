# Spécification Technique : Déploiement Helm & Documentation Administrateur

> **Statut** : Validé suite aux sessions d'itération et stress-test d'architecture (Grill-Me)  
> **Date** : 2026-08-23  
> **Auteur** : Équipe Atelier  
> **Contexte** : Chart Helm monolithique tout-en-un, rolling upgrades non perturbateurs (statut NeedsRestartForUpgrade), support des identités Cloud natives (AWS IRSA / AssumeRole) et guide d'exploitation.

---

## 1. Objectifs & Décisions Clés

1. **Rolling Upgrades Non Perturbateurs du Chart Helm** :
   - Lors d'un `helm upgrade` mettant à jour `atelier-controller` ou `atelier-api-server`, les microVMs Firecracker actives dans leurs pods respectifs **continuent de tourner sans interruption**.
   - Le controller compare le hash de version du template de pod avec le statut du Workshop. Si un décalage est détecté, le statut `status.upgradeState: NeedsRestartForUpgrade` est positionné, permettant à l'utilisateur de terminer ses tâches avant de redémarrer (ou lors du prochain cycle suspend/resume).
2. **Priorité aux Identités Cloud Natives (Zero Static Secrets)** :
   - **AWS EKS** : Support complet d'**IRSA**, EKS Pod Identity et `sts:AssumeRole` via annotations sur les ServiceAccounts.
   - **GCP GKE** : Support de **Workload Identity Federation**.
   - **Azure AKS** : Support de **Microsoft Entra Workload ID**.
3. **Chart Helm Monolithique Personnalisé (`charts/atelier`)** :
   - Déploiement complet embarqué : Keycloak, Forgejo (100% HTTPS, dépôts internes), PostgreSQL, OpenBao, LiteLLM Proxy, Redis (Redis Streams), DevFactory PM Engine (LangGraph) et optionnellement RustFS (S3).
   - 4 Ingress dédiés (`auth`, `git`, `app`, `api`).
4. **Documentation Administrateur Complète (`docs/admin-guide.md`)** :
   - Guide d'installation complet, prérequis KVM, configuration IRSA / Workload Identity, 4 domaines DNS, gestion des mises à jour sans interruption, procédures de backup/restore et troubleshooting.

---

## 2. Modèle `values.yaml`

```yaml
# ==============================================================================
# Configuration des Domaines Obligatoires (4 Ingress Dédiés avec TLS)
# ==============================================================================
domains:
  keycloak: "auth.atelier.example.com"
  forgejo: "git.atelier.example.com"
  dashboard: "app.atelier.example.com"
  apiServer: "api.atelier.example.com"

tls:
  enabled: true
  certManager:
    enabled: true
    issuer: "letsencrypt-prod"
  secretName: "atelier-tls-certs"

# ==============================================================================
# Identités Cloud & Fallback Secrets Statiques
# ==============================================================================
cloudIdentity:
  provider: "none" # Options: 'none' (on-premise), 'aws' (IRSA/AssumeRole), 'gcp' (Workload Identity), 'azure' (Workload ID)
  fallbackSecretName: "atelier-external-credentials" # Utilisé si provider == 'none'

# --- Control Plane Atelier ---
controller:
  enabled: true
  replicaCount: 1
  image:
    repository: ghcr.io/philippevienne/atelier-controller
    tag: latest
  serviceAccount:
    create: true
    annotations: {} # Ex: eks.amazonaws.com/role-arn: "arn:aws:iam::123:role/atelier-controller"

apiServer:
  enabled: true
  replicaCount: 2
  image:
    repository: ghcr.io/philippevienne/atelier-api-server
    tag: latest
  serviceAccount:
    create: true
    annotations: {} # Ex: eks.amazonaws.com/role-arn: "arn:aws:iam::123:role/atelier-apiserver"
  jwt:
    audience: "atelier-api"
  ingress:
    annotations:
      nginx.ingress.kubernetes.io/proxy-read-timeout: "3600"
      nginx.ingress.kubernetes.io/proxy-send-timeout: "3600"
      nginx.ingress.kubernetes.io/websocket-services: "atelier-api-server"

dashboard:
  enabled: true
  replicaCount: 2
  image:
    repository: ghcr.io/philippevienne/atelier-dashboard
    tag: latest

# --- Moteur DevFactory & Project Manager (LangGraph) ---
pmEngine:
  enabled: true
  replicaCount: 1
  image:
    repository: ghcr.io/philippevienne/atelier-pm-engine
    tag: latest
  serviceAccount:
    create: true
    annotations: {}
  hitlPolicy: "require_approval_before_merge"
  embeddingModel: "text-embedding-3-small"

# --- Files de Messages Asynchrones (Redis Streams) ---
redis:
  enabled: true
  image:
    repository: redis
    tag: "7.2-alpine"
  persistence:
    size: 5Gi

# --- Passerelle IA LiteLLM ---
litellm:
  enabled: true
  image:
    repository: ghcr.io/berriai/litellm
    tag: "main-latest"
  masterKey: "change-me-litellm-master-key"
  defaultWorkshopBudgetUsd: 5.00

# --- Infrastructure KVM ---
kvmDevicePlugin:
  enabled: true
  image:
    repository: ghcr.io/philippevienne/atelier-kvm-device-plugin
    tag: latest

# --- Services d'Infrastructure Déployés ou BYO ---
postgresql:
  enabled: true
  image:
    repository: pgvector/pgvector
    tag: "pg16"
  persistence:
    size: 20Gi
  external:
    host: ""
    port: 5432
    sslMode: "require"
    iamAuthEnabled: false

keycloak:
  enabled: true
  image:
    repository: quay.io/keycloak/keycloak
    tag: "24.0"
  auth:
    adminUser: admin
    adminPassword: "change-me-keycloak-admin"
  external:
    url: ""

forgejo:
  enabled: true
  image:
    repository: codeberg.org/forgejo/forgejo
    tag: "7.0"
  persistence:
    size: 10Gi # Dépôts Git nus
  external:
    url: ""

openbao:
  enabled: true
  image:
    repository: openbao/openbao
    tag: "2.0.0"
  persistence:
    size: 10Gi
  devMode: false
  external:
    url: ""

# --- Stockage d'Objets S3 (RustFS, GCS, Azure, AWS avec AssumeRole) ---
s3Storage:
  rustfs:
    enabled: true
    image:
      repository: rustfs/rustfs
      tag: "latest"
    persistence:
      size: 100Gi
    auth:
      accessKey: "atelier-rustfs-access-key"
      secretKey: "atelier-rustfs-secret-key"
  external:
    enabled: false
    endpoint: ""
    region: "eu-west-1"
    assumeRoleArn: ""
    forcePathStyle: false
    buckets:
      sessions: "atelier-sessions"
      snapshots: "atelier-snapshots"
      forgejo: "forgejo-lfs-attachments"
```

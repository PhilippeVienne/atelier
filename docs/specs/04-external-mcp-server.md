# Spécification Technique : Serveur MCP Externe (Intégré dans API Server)

> **Statut** : Validé suite aux sessions d'itération et stress-test d'architecture (Grill-Me)  
> **Date** : 2026-08-23  
> **Auteur** : Équipe Atelier  
> **Contexte** : Serveur Model Context Protocol (MCP) intégré directement dans `atelier-api-server` sous `/v1/mcp`, authentifié via OIDC Keycloak, supportant l'exécution asynchrone bufferisée, le confinement de sécurité automatique et le principe Fast-Fail de dépendances.

---

## 1. Objectifs & Choix d'Architecture

Le **Serveur MCP Externe** est directement **embarqué au sein de `atelier-api-server`** (accessible sur la route `https://api.atelier.example.com/v1/mcp/sse` ou `/v1/mcp/ws`) :
- **Principe Fast-Fail sur Dépendances Critiques** :
  Si LiteLLM ou OpenBao est inaccessible, l'API Server refuse immédiatement les requêtes `/v1/mcp` créatrices d'état (`create_workshop`, `exec_in_workshop`) avec une erreur explicite `503 Service Unavailable (Security dependencies unreachable)`, garantissant qu'aucun environnement ne démarre sans politique de sécurité ou de budget active.
- **Résilience aux Coupures Réseau & Exécutions Longues** :
  L'exécution de commande `exec_in_workshop` est découplée de manière asynchrone avec persistance du buffer de sortie dans PostgreSQL.
- **Confinement Automatique de Sécurité (Egress Lockdown & Snapshot)** :
  Si `net-proxy` détecte une tentative d'évasion (scan de ports internes, tentative de contournement d'allowlist ou inondation réseau), la microVM est **immédiatement suspendue (Snapshot d'urgence)**, l'accès réseau est gelé (`Lockdown`), et une alerte de sécurité est journalisée sur le Dashboard.

---

## 2. Flux d'Exécution Asynchrone Découplé (`exec_in_workshop`)

```mermaid
sequenceDiagram
    autonumber
    actor Client as Client MCP (Claude Desktop / Cursor)
    participant API as atelier-api-server (/v1/mcp)
    participant DB as PostgreSQL (atelier_apiserver.exec_commands)
    participant VMS as vm-supervisor (Pod Workshop)
    participant Guest as microVM Firecracker (Agent)

    Client->>API: MCP Tool Call: exec_in_workshop(name, "cargo test --workspace")
    API->>DB: INSERT INTO exec_commands (id, status: 'Running', stdout: '')
    API-->>Client: Retourne execution_id + URL de stream/reconnexion
    API->>VMS: Démarre commande via WebSocket/AF_VSOCK
    VMS->>Guest: Exécute commande dans le shell

    loop Streaming & Buffering
        Guest-->>VMS: Chunks stdout/stderr
        VMS-->>API: Stream chunks
        API->>DB: Append chunks dans le buffer
        API-->>Client: Stream temps réel (si connecté)
    end

    Note over Client,API: Si le client se déconnecte, l'exécution continue
    Client->>API: Reconnexion sur GET /v1/workshops/{name}/exec/{id}/stream
    API->>DB: Relit buffer manquant et reprend le streaming en direct

    Guest-->>VMS: Exit Code: 0
    VMS-->>API: Commande terminée (exit_code: 0)
    API->>DB: UPDATE exec_commands SET status = 'Completed', exit_code = 0
    API-->>Client: Résultat final JSON (stdout, stderr, exit_code)
```

---

## 3. Confinement Automatique de Sécurité

```mermaid
flowchart TD
    VM["microVM Firecracker (Agent)"]
    NetProxy["net-proxy (Surveillance réseau)"]
    Supervisor["vm-supervisor"]
    Controller["atelier-controller"]
    Dash["Dashboard Admin"]

    VM -->|"Scan de ports internes / Flooding"| NetProxy
    NetProxy -->|"Détection d'attaque (Anomalie)"| Supervisor
    Supervisor -->|"1. Coupe immédiatement le bridge TAP (Egress Lockdown)"| NetProxy
    Supervisor -->|"2. Déclenche Snapshot RAM d'urgence (Gel d'état)"| VM
    Supervisor -->|"3. Patch Workshop status (SecurityLockdown)"| Controller
    Controller -->|"4. Notification Alerte Critique"| Dash
```

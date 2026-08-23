# Spécification Technique : Cadrage & Architecture du Dashboard Next.js 16

> **Statut** : Validé suite aux sessions d'itération et de stress-test d'architecture (Grill-Me)  
> **Date** : 2026-08-23  
> **Auteur** : Équipe Atelier  
> **Composant** : `dashboard/` (Next.js 16 App Router, React 19, Tailwind CSS, TypeScript)  
> **Principe Cadre** : Pattern Backend-for-Frontend (BFF) strict, Server Actions, Zero Token Exposure côté client, WebSockets managés et Basic Auth sécurisée OpenBao.

---

## 1. Vision & Rôle du Dashboard Atelier

Le **Dashboard Atelier** est l'interface unifiée de gestion, d'observabilité et d'interaction pour les développeurs et administrateurs de la plateforme :

1. **Cycle de Vie des Workshops & Réactivité Temps Réel** :
   - Tableau de bord des environnements actifs, en veille (`Suspended`) ou en construction (`BuildingRootfs`).
   - Mises à jour réactives sans polling via le composant client `LiveRefresh` (écoute SSE `/api/workshops/events`).
   - Formulaire de création guidé avec surcharge de ressources et budget LLM (`max_llm_budget_usd`).
2. **Accès Interactif aux MicroVMs (Pont HTTP, WebSockets Managés & Injection Basic Auth)** :
   - Intégration de **VS Code Web (`code-server`)** et d'un **Terminal Web (`ttyd`)** sans port SSH exposé.
   - **Protection par Basic Auth & Injection OpenBao** : Les daemons invités `code-server` et `ttyd` sont protégés par mot de passe aléatoire. L'accès utilisateur est authentifié par JWT OIDC auprès de `api-server`, qui injecte automatiquement le header `Authorization: Basic <base64(atelier:password)>` extrait d'OpenBao lors du relai vers la microVM.
   - **Gestion de l'Inactivité & Auto-Suspend** : Le serveur Node.js custom (`server.ts`) détecte la fermeture des sockets WebSockets clients et envoie des heartbeats d'activité. Après 15 minutes d'inactivité totale, la microVM est mise en veille (`suspend`).
3. **Console DevFactory & Chat "Ask Project Manager" (Streaming SSE BFF)** :
   - Vue dédiée par projet permettant d'interroger la mémoire sémantique du PM (`pgvector`).
   - Streaming SSE sécurisé via Route Handler Next.js `/api/pm/chat` (Zero Token OIDC côté client).
   - Interface d'approbation **Human-in-the-Loop (HITL)** pour valider ou rejeter les Pull Requests.
4. **Maintien de Session Continue (`SessionKeepalive`)** :
   - Le composant client `SessionKeepalive` envoie un heartbeat `/api/auth/refresh` toutes les 5 minutes pour maintenir la session active.

---

## 2. Architecture Technique : Pattern Backend-for-Frontend (BFF)

```mermaid
flowchart TB
    Browser["Navigateur Utilisateur (Client React 19)"]

    subgraph Dashboard_NextJS["Atelier Dashboard (Next.js 16 App Router)"]
        subgraph Client_Side["Client Components"]
            ChatUI["PM Chat Interface (SSE)"]
            TerminalUI["Terminal & VS Code Embed (Iframe/WS)"]
            LiveRefresh["LiveRefresh (SSE Event Listener)"]
            Keepalive["SessionKeepalive (Heartbeat 5m)"]
        end

        subgraph Server_Side["Server Layer (Node.js Custom server.ts)"]
            AuthRoutes["/api/auth/* (PKCE, Refresh, Logout)"]
            SSERoutes["/api/pm/chat & /api/workshops/events (ReadableStream)"]
            ServerActions["Server Actions (CRUD, Suspend, Resume)"]
            CookieManager["Session Cookie Manager (httpOnly, Secure, SameSite)"]
            CustomWSServer["WebSocket Upstream Proxy (Heartbeat & Auto-Suspend)"]
        end
    end

    subgraph External_Services["Services Externes"]
        Keycloak["Fournisseur OIDC (Keycloak / Auth0 / Okta)"]
        ApiServer["atelier-api-server\n(+ Injection Basic Auth OpenBao)"]
        Vault[("OpenBao Vault\n(Secret session_auth)")]
        PMEngine["atelier-pm-engine (FastAPI / LangGraph)"]
        Guest["microVM Firecracker (ttyd / code-server protégés par mot de passe)"]
    end

    Browser -->|"1. Requête Page / Server Action"| ServerActions
    Browser <-->|"WebSocket direct (ttyd / code-server)"| CustomWSServer
    ChatUI -->|"SSE Stream (via BFF)"| SSERoutes
    Keepalive -->|"Heartbeat Token Refresh"| AuthRoutes
    LiveRefresh -->|"SSE K8s Events"| SSERoutes

    AuthRoutes <-->|"OIDC PKCE & Refresh"| Keycloak
    CookieManager -.->|"Injecte JWT Bearer côté serveur"| ServerActions
    CookieManager -.->|"Injecte JWT Bearer côté serveur"| SSERoutes
    ServerActions -->|"Appels REST authentifiés"| ApiServer
    SSERoutes -->|"Relai SSE streaming"| PMEngine
    CustomWSServer -->|"Relai WS avec JWT"| ApiServer
    ApiServer -->|"Résout secret session_auth"| Vault
    ApiServer -->|"Injecte Authorization: Basic"| Guest
```

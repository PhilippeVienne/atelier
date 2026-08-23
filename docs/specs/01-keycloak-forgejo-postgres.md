# Spécification Technique : Intégration IAM, Git, PostgreSQL & Stockage S3 Hybride

> **Statut** : Validé suite aux sessions d'itération et stress-test d'architecture (Grill-Me)  
> **Principe Cadre** : Conforme au document [`00-architecture-principles-substitutability.md`](00-architecture-principles-substitutability.md) (Toutes les briques sont substituables par des standards du marché).  
> **Date** : 2026-08-23  
> **Auteur** : Équipe Atelier  

---

## 1. Décisions d'Architecture & Périmètre d'Isolation

1. **Gestion de l'Identité & Rotation Dynamique des Clés (Keycloak / OIDC)** :
   - **Rotation Automatique des JWKS** : `atelier-api-server` intègre un cache JWKS dynamique (rafraîchissement périodique en tâche de fond toutes les 10 min et refetch immédiat à la volée en cas de `kid` inconnu), garantissant qu'aucune rotation de clé de signature Keycloak ne provoque de rejets `401 Unauthorized`.
   - **Substituabilité Totale** : Compatible avec **Auth0, Okta, Microsoft Entra ID, GitLab OIDC, Authentik**.
   - **Périmètre Strict Utilisateurs Humains & SSO** : L'authentification des microVMs passe uniquement par des tokens injectés.
2. **Sécurisation des Accès Interactifs MicroVM (Basic Auth VS Code & `ttyd` via OpenBao)** :
   - **Génération Aléatoire du Secret de Session** : Lors du provisioning du Workshop, le `controller` génère un mot de passe aléatoire dédié (`workshop_secret`) et le persiste dans OpenBao sous `secret/data/workshops/<name>/session_auth`.
   - **Injection dans la MicroVM** : Le secret est injecté dans les arguments de lancement des daemons invités (`code-server --auth password` et `ttyd --credential atelier:<password>`).
   - **Relai Transparent par l'API Server** : Lorsque l'utilisateur ouvre sa session VS Code ou son Terminal depuis le Dashboard (authentifié par son token OIDC JWT), `api-server` extrait le secret depuis OpenBao et injecte à la volée le header `Authorization: Basic <base64(atelier:password)>` dans le pont HTTP/WebSocket vers la microVM. Tout accès direct non autorisé depuis l'intérieur du pod ou du réseau est ainsi systématiquement rejeté (401).
3. **Forge Git Interne & Stratégie Multi-Branches Éphémères** :
   - **Périmètre d'Accès de l'Agent** : L'agent dans la microVM ne peut accéder qu'à des **dépôts Git internes pré-provisionnés et hébergés sur Forgejo** (ou la forge interne substituée).
   - **Isolation Stricte des Branches par Tâche (`feature/task-<id>`)** : Lorsque le PM Engine parallélise plusieurs tâches ou microVMs sur un même projet, chaque Workshop opère sur sa propre **sous-branche Git éphémère dédiée** et soumet une Pull Request distincte.
   - **Garantie d'Intégrité Git Pré-Snapshot (`git-sync-hook`)** :
     - Avant toute mise en veille (`suspend_workshop`), `vm-supervisor` exécute un hook de synchronisation interne (`git add -A && git commit -m "wip: auto-checkpoint" && git push origin feature/task-<id>`).
   - **Stockage Hybride des Données Git** :
     - **Dépôts Git nus (`.git`)** : Conservés sur un petit volume bloc POSIX (PVC 10 Go si Forgejo embarqué).
     - **Stockage d'Objets / LFS / Packages** : Déportés sur RustFS ou un S3 Cloud managé (AWS S3, GCS, Azure Blob).
4. **Persistance PostgreSQL & Isolation Multi-Tenant par Row Level Security (RLS)** :
   - **Row Level Security (RLS) Natif** : L'isolation multi-tenant dans toutes les bases partagées (`atelier_pm`, `atelier_apiserver`) est renforcée au niveau du moteur PostgreSQL par **Row Level Security (RLS)** via la variable de session `SET LOCAL app.current_tenant = '<tenant_id>'`.
   - **Zéro Secret en Base Relationnelle** : **Aucun token, mot de passe ou secret brut n'est jamais stocké dans PostgreSQL**. La base relationnelle ne conserve que des UUIDs, des métadonnées et des chemins de référence vers **OpenBao / Vault**.
   - **PostgreSQL Obligatoire au Démarrage** : `atelier-api-server` et `atelier-controller` nécessitent une connexion PostgreSQL active pour démarrer.
   - **Gestion Sécurisée des Migrations** : Les migrations SQL sont exécutées par des Jobs Kubernetes dédiés utilisant un compte d'administration de schéma (`atelier_migrator`).
5. **Stockage d'Objets, Décharge S3 Multipart & Pipeline Rootfs Intègre** :
   - **Streaming Multipart S3 Asynchrone Throttlé** : Lors de la suspension des microVMs, le pod parent décharge le snapshot mémoire (2 à 8 Go) directement vers S3 en streaming Multipart Upload régulé.
   - **Intégrité Garantie du Rootfs (`builder-vm-init`)** :
     Flush complet du cache disque (`sync`) et démontage propre avant `crane export -> mke2fs`.

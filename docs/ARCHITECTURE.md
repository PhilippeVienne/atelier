# Architecture d'Atelier

## Objectif

Fournir a un agent de code (Claude Code, Gemini CLI, etc.) un environnement
d'execution auquel on peut accorder des pouvoirs larges (shell, reseau, ecriture
disque) sans risque pour le reste du systeme, parce qu'il est execute dans une
prison suffisamment etanche : une microVM Firecracker, elle-meme orchestree
depuis un pod Kubernetes.

## Vue d'ensemble

```
                         ┌────────────────────────────────────────┐
Client externe (JWT) ───►│              api-server                │
                         │  (auth JWT, expose Workshop CRUD)       │
                         └───────────────┬──────────────────────────┘
                                          │ cree/lit des CR
                                          ▼
                         ┌────────────────────────────────────────┐
                         │              controller                │
                         │  (operateur, reconcilie Workshop → Pod) │
                         └───────────────┬──────────────────────────┘
                                          │ cree
                                          ▼
      ┌───────────────────────── Pod parent (namespace isole) ─────────────────────────┐
      │                                                                                  │
      │   ┌───────────────┐   ┌───────────┐   ┌────────────────┐   ┌─────────────────┐  │
      │   │ vm-supervisor │   │ net-proxy │   │ identity-proxy │   │   mcp-gateway    │  │
      │   │ (lifecycle    │   │ (egress   │   │ (injection de  │   │ (agent ↔ monde   │  │
      │   │  Firecracker) │   │  allowlist│   │  credentials)  │   │  exterieur, MCP) │  │
      │   └───────┬───────┘   └─────┬─────┘   └────────┬───────┘   └────────┬─────────┘  │
      │           │ vsock/API       │ reseau            │ reseau            │ vsock       │
      │           ▼                 └──────────┬────────┴───────────────────┘             │
      │   ┌─────────────────────────────────────▼─────────────────────────────────────┐   │
      │   │                     microVM Firecracker (jailer)                          │   │
      │   │   agent de code (Claude Code, ...) + shell + acces disque de travail       │   │
      │   └─────────────────────────────────────────────────────────────────────────┘   │
      │                                                                                  │
      └──────────────────────────────────────────────────────────────────────────────────┘
```

## Composants

### control plane (hors du pod parent)

- **api-server** (`crates/api-server`) : API HTTP externe. Authentifie les
  appels via un JWT dont l'issuer est signe par un provider externe
  pre-enregistre (liste d'issuers de confiance + JWKS). Cree/lit/detruit des
  CR `Workshop`.
- **controller** (`crates/controller`) : operateur Kubernetes (kube-rs) qui
  reconcilie les CR `Workshop` en ressources concretes (pod parent,
  ResourceQuota, NetworkPolicy) et met a jour leur statut.
- **CRD `Workshop`** (`crates/common/src/crd.rs`, manifeste genere dans
  `crds/workshop.yaml`) : source de verite declarative pour un environnement
  (image de microVM, ressources, allowlist reseau, outils/simulateurs actifs,
  proprietaire).

### tooling du pod parent (a cote de la microVM, pas dedans)

- **vm-supervisor** (`crates/vm-supervisor`) : demarre/arrete la microVM
  Firecracker via jailer + socket API, expose un canal de controle (vsock)
  vers l'agent.
- **net-proxy** (`crates/net-proxy`) : seul chemin de sortie reseau autorise
  pour la microVM ; n'autorise que les domaines listes dans
  `Workshop.spec.egress_allowlist`, journalise chaque appel.
- **identity-proxy** (`crates/identity-proxy`) : injecte des credentials/tokens
  dans les appels sortants (ex: acces a une API necessitant un token) sans
  jamais exposer le secret brut a l'agent dans la VM.
- **mcp-gateway** (`crates/mcp-gateway`) : serveur MCP expose a l'agent (via
  vsock). C'est le seul point d'entree pour que l'agent (1) agisse sur le
  monde exterieur au-dela du simple reseau proxifie, et (2) demande des
  reglages a l'atelier (elargir une allowlist, activer un simulateur
  d'API/AWS, demander un credential). Point d'extension privilegie pour
  ajouter de nouveaux simulateurs (LocalStack pour AWS, mocks d'API, etc.).

### interfaces utilisateur

- **dashboard** (`dashboard/`, Next.js) : vue admin et vue utilisateur final.
  Lister/creer/detruire des Workshops, visualiser leur etat et leur
  consommation de ressources, s'y connecter (SSH ou VS Code via un
  code-server embarque, cf. `coder/code-server` comme reference
  d'implementation).

## Modele de securite

- La seule surface d'attaque exposee par la microVM vers l'exterieur passe
  par le pod parent : reseau (net-proxy), identite (identity-proxy) et
  controle (mcp-gateway). Aucun acces direct de la VM au reste du cluster.
- Isolation memoire/noyau assuree par Firecracker (jailer, seccomp, cgroups)
  plutot que par la seule isolation de conteneur d'un Pod.
- Authentification externe MVP : JWT signes par un provider externe
  pre-enregistre (liste d'issuers + JWKS statique au demarrage de
  l'api-server). Pas de gestion d'utilisateurs locale dans un premier temps.

## Allocation de ressources et scaling

- Chaque `Workshop` se traduit par un pod avec `resources.requests/limits`
  explicites (`WorkshopSpec.resources`), compatible avec le cluster-autoscaler
  et un HPA standard au niveau du nombre de Workshops actifs.

## Ce qui reste a trancher (hors MVP initial)

- Mecanisme concret d'orchestration Firecracker (jailer pilote directement
  par `vm-supervisor`, decision prise : pas de runtime OCI type Kata).
- Format d'image de microVM (kernel + rootfs) et pipeline de build.
- Modele d'autorisation fin cote `mcp-gateway` (quelles demandes de
  l'agent sont auto-approuvees vs necessitent une validation humaine).
- Stockage des secrets pour `identity-proxy` (Vault ? Secrets Kubernetes
  projetes ?).

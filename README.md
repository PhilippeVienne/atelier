# Atelier

Environnement securise et controle pour agents de code (Claude Code, Gemini
CLI, etc.) : chaque agent tourne dans une microVM Firecracker orchestree par
un pod Kubernetes, avec un tooling dedie (proxy reseau, injection d'identite,
passerelle MCP) qui mediatise tous ses acces au monde exterieur.

Voir [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) pour le detail des
composants et du modele de securite.

## Structure du depot

- `crates/common` — types partages, dont le CRD `Workshop`
- `crates/controller` — operateur Kubernetes (reconciliation des `Workshop`)
- `crates/api-server` — API externe (auth JWT, CRUD de `Workshop`)
- `crates/vm-supervisor` — cycle de vie de la microVM Firecracker (pod parent)
- `crates/net-proxy` — proxy de sortie reseau avec allowlist (pod parent)
- `crates/identity-proxy` — injection de credentials (pod parent)
- `crates/mcp-gateway` — serveur MCP expose a l'agent (pod parent)
- `crds/` — manifestes CRD generes (`cargo run -p atelier-controller --bin crdgen`)
- `dashboard/` — dashboard Next.js (admin + utilisateur final)
- `deploy/` — manifestes de deploiement du control plane

## Developpement

```sh
# control plane (Rust)
cargo check --workspace

# regenerer le CRD apres modification de crates/common/src/crd.rs
cargo run -p atelier-controller --bin crdgen > crds/workshop.yaml

# dashboard
cd dashboard && npm run dev
```

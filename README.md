# Atelier

Environnement securise et controle pour agents de code (Claude Code, Gemini
CLI, etc.) : chaque agent tourne dans une microVM Firecracker orchestree par
un pod Kubernetes, avec un tooling dedie (proxy reseau, injection d'identite,
passerelle MCP) qui mediatise tous ses acces au monde exterieur.

Voir [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) pour le detail des
composants et du modele de securite, et [docs/PROGRESS.md](docs/PROGRESS.md)
pour l'etat d'avancement courant (composant par composant, contre de la
vraie infrastructure, pas de mocks).

## Structure du depot

- `crates/common` — types partages, dont le CRD `Workshop`
- `crates/controller` — operateur Kubernetes (reconciliation des `Workshop`)
- `crates/api-server` — API externe (auth JWT, CRUD de `Workshop`)
- `crates/image-builder` — devcontainer.json → rootfs Firecracker (cache content-addressed)
- `crates/vm-supervisor` — cycle de vie de la microVM Firecracker (pod parent)
- `crates/firecracker` — lib partagee jailer/boot/snapshot-restore + reseau TAP link-local, utilisee par `vm-supervisor` et `builder-vm-init`
- `crates/builder-vm-init` — init de la microVM jetable qui isole `envbuilder` (pipeline `image-builder`)
- `crates/net-proxy` — proxy de sortie reseau avec allowlist (egress HTTP/CONNECT, resolveur DNS, port-forward kubelet-style) (pod parent)
- `crates/identity-proxy` — injection de credentials OpenBao dans les appels sortants de l'agent (pod parent)
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

## Tests

Les tests des composants control plane qui parlent a Kubernetes (ex:
`crates/controller`) sont des tests d'integration reels contre un cluster,
pas des mocks. Un cluster [kind](https://kind.sigs.k8s.io/) local suffit :

```sh
kind create cluster --name atelier-dev
kubectl apply -f crds/workshop.yaml
cargo test --workspace
```

Les tests de `crates/vm-supervisor` pilotent un vrai Firecracker (necessite
KVM) : voir `deploy/dev/firecracker/README.md` pour recuperer les binaires
et fixtures de test necessaires. Sans ces variables d'environnement, ce test
est ignore comme les autres tests optionnels (Kanidm, OpenBao).

## Observabilite

Tous les binaires appellent `atelier_common::telemetry::init(...)` (voir
`docs/ARCHITECTURE.md`). Sans configuration, ils se contentent de logger. Pour
exporter les traces en OTLP vers un collecteur local :

```sh
docker run -d --name atelier-otel-collector-dev -p 4317:4317 \
  -v "$(pwd)/deploy/dev/otel/collector-config.yaml":/etc/otelcol/config.yaml:ro \
  otel/opentelemetry-collector:latest --config /etc/otelcol/config.yaml

OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317 cargo run -p atelier-controller --bin atelier-controller
```


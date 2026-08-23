# Atelier

Environnement securise et controle pour agents de code (Claude Code, Gemini
CLI, etc.) : chaque agent tourne dans une microVM Firecracker orchestree par
un pod Kubernetes, avec un tooling dedie (proxy reseau, injection d'identite,
passerelle MCP) qui mediatise tous ses acces au monde exterieur.

Voir [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) pour le détail des composants et du modèle de sécurité, [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md) pour le guide de déploiement et CI/CD (GHCR), et [docs/PROGRESS.md](docs/PROGRESS.md) pour l'état d'avancement courant.

## 🚀 CI/CD & Images Docker (GHCR)

Les workflows GitHub Actions (`.github/workflows/`) assurent le contrôle de qualité et la publication des conteneurs :
- **CI (`ci.yml`)** : `cargo fmt`, `cargo clippy`, `cargo test`, lint & build dashboard.
- **Docker GHCR (`docker-ghcr.yml`)** : publication automatique des 10 composants sur `ghcr.io/philippevienne/atelier-<composant>:latest`.

## 📦 Structure du dépôt

- `crates/common` — types partagés, dont le CRD `Workshop`
- `crates/controller` — opérateur Kubernetes (réconciliation des `Workshop`)
- `crates/api-server` — API externe (auth JWT, CRUD de `Workshop`)
- `crates/image-builder` — devcontainer.json → rootfs Firecracker (cache content-addressed)
- `crates/vm-supervisor` — cycle de vie de la microVM Firecracker (pod parent)
- `crates/firecracker` — lib partagée jailer/boot/snapshot-restore + réseau TAP link-local
- `crates/builder-vm-init` — init de la microVM jetable qui isole `envbuilder`
- `crates/net-proxy` — proxy de sortie réseau avec allowlist (egress HTTP/CONNECT, DNS, port-forward)
- `crates/identity-proxy` — injection de credentials OpenBao dans les appels sortants
- `crates/mcp-gateway` — serveur MCP exposé à l'agent
- `crates/kvm-device-plugin` — device plugin Kubernetes pour `/dev/kvm`
- `crds/` — manifestes CRD générés (`cargo run -p atelier-controller --bin crdgen`)
- `dashboard/` — dashboard Next.js (admin + utilisateur final)
- `deploy/manifests/` — manifestes Kubernetes prêts pour le déploiement en production
- `deploy/dev/` — environnement de développement local (Kind, OpenBao, Kanidm, OTLP)

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

## 📄 Licence

Ce projet est distribué sous la licence **GNU Affero General Public License v3.0** ([AGPLv3](LICENSE)).



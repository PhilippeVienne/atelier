# Stack de developpement locale complete

`../local-stack.sh` orchestre tout ce qui est deja documente separement
(`deploy/dev/openbao/`, `deploy/dev/kanidm/`, `deploy/dev/llm-proxy/`,
registre OCI) plus le build des images `:dev` necessaires aux pods
Workshop, et ecrit `env.sh` (ignore par git) avec toutes les variables
necessaires pour lancer `controller`/`api-server`/le dashboard en local.

## Prerequis (une seule fois)

- Un cluster kind nomme `atelier-dev` avec `atelier-kvm-device-plugin`
  deploye (`deploy/dev/kvm-device-plugin/README.md`).
- Kanidm deja initialise sur cette machine — suivre
  `deploy/dev/kanidm/README.md` (etapes 1 a 4 : certificats + premier
  demarrage + client OAuth2 `atelier` + redirect URIs pour le dashboard).
  Le script redemarre le conteneur si besoin mais ne fait pas cette
  initialisation la premiere fois.
- Un registre OCI de dev (`docker run -d --name atelier-registry-dev -p
  5000:5000 registry:2`, une seule fois).

## Utilisation

```sh
./deploy/dev/local-stack.sh

# Pour brancher aussi le LLM Proxy (DeepSeek + Anthropic premium) :
DEEPSEEK_API_KEY=sk-... ANTHROPIC_API_KEY=sk-ant-... ./deploy/dev/local-stack.sh
```

Puis, dans trois terminaux :

```sh
source deploy/dev/local-stack/env.sh
cargo run --bin atelier-controller

source deploy/dev/local-stack/env.sh
cargo run -p atelier-api-server

cd dashboard && npm run dev
```

Dashboard sur <http://localhost:3000> — se connecter avec un compte Kanidm
reel (`kanidm person create ...`, voir `deploy/dev/kanidm/README.md`), le
certificat auto-signe de Kanidm declenchera un avertissement navigateur a
accepter une fois (dev uniquement).

## Limite assumee : `controller`/`api-server` en process local, pas en pod

Ce script fait tourner `controller` et `api-server` comme des process
locaux (`cargo run`), pas comme des `Deployment` Kubernetes. Raison : le
certificat TLS de Kanidm n'est valide que pour le nom `localhost`
(`deploy/dev/kanidm/server.toml`, `domain = "localhost"`) — un pod a
l'interieur du cluster ne peut pas l'atteindre sous ce nom sans un
`hostAliases` pointant sur l'IP reelle du conteneur Kanidm (le meme genre
de contournement que celui deja documente pour le registre OCI dans
`docs/PROGRESS.md`, "Reseau kind ↔ registre"), non automatise ici.

Consequence concrete : `api-server` tournant en process local ne peut pas
joindre directement l'IP d'un pod Workshop (le host ne route pas vers le
reseau de pods de kind) — le port-forward K8s
(`/v1/workshops/{name}/portforward`) et le pont "Ouvrir VS Code"
(`/v1/workshops/{name}/vscode/*`) ne fonctionneront donc **pas** dans
cette configuration precise, meme si tout le reste (CRUD de Workshops,
authentification, provisioning Kanidm/OpenBao, build d'image, boot
Firecracker) fonctionne normalement.

Pour lever cette limite : containeriser `api-server` (`crates/api-server/Dockerfile`
existe deja) et le lancer avec `docker run --network container:atelier-dev-control-plane`
(partage le netns du noeud kind, qui route bien vers les IP de pods —
meme methode que celle deja utilisee pour tester le canal de controle
`controller`/`vm-supervisor`, voir `docs/PROGRESS.md`, "Canal de controle
suspend/resume"). Non automatise dans ce script pour l'instant.

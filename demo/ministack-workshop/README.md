# Devcontainer de demo : ministack + docker + Claude Code + code-server

Devcontainer standard ([Dev Containers](https://containers.dev/)) demontrant
qu'un environnement combinant plusieurs outils tourne reellement, comme
cible possible de `Workshop.spec.devcontainer` :

- **docker** (feature `docker-in-docker`) — un vrai moteur Docker tourne
  *dans* le devcontainer, independant de tout docker hote.
- **[ministack](https://github.com/ministackorg/ministack)** — emulateur
  AWS local (alternative libre a LocalStack), installe dans un venv Python
  dedie, demarre en arriere-plan (`postStartCommand`) sur le port `4566`.
- **Claude Code** (feature officielle
  `ghcr.io/anthropics/devcontainer-features/claude-code`).
- **[code-server](https://github.com/coder/code-server)** (VS Code dans le
  navigateur, paquet `.deb` officiel epingle), demarre en arriere-plan sur
  le port `8080` — cf. `docs/ARCHITECTURE.md`, composant `dashboard`, qui
  documente deja ce choix comme reference pour l'acces web a un Workshop.

## Verifie reellement (CLI officiel `devcontainer`, pas de mock)

```sh
npm install -g @devcontainers/cli   # une seule fois
devcontainer up --workspace-folder demo/ministack-workshop

devcontainer exec --workspace-folder demo/ministack-workshop -- docker version
devcontainer exec --workspace-folder demo/ministack-workshop -- claude --version
devcontainer exec --workspace-folder demo/ministack-workshop -- \
  curl -s http://127.0.0.1:4566/_ministack/health
devcontainer exec --workspace-folder demo/ministack-workshop -- \
  curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:8080
```

Verifie au-dela du simple health-check : un vrai bucket S3 emule cree et un
vrai fichier uploade/relu via `aws s3` (`awscli` installe dans le meme venv
que `ministack`) :

```sh
devcontainer exec --workspace-folder demo/ministack-workshop -- bash -c '
  export AWS_ACCESS_KEY_ID=test AWS_SECRET_ACCESS_KEY=test AWS_DEFAULT_REGION=us-east-1
  sudo -E /opt/ministack-venv/bin/pip install -q awscli
  sudo -E /opt/ministack-venv/bin/aws --endpoint-url=http://127.0.0.1:4566 s3 mb s3://demo-bucket
  sudo -E /opt/ministack-venv/bin/aws --endpoint-url=http://127.0.0.1:4566 s3 cp /etc/os-release s3://demo-bucket/
  sudo -E /opt/ministack-venv/bin/aws --endpoint-url=http://127.0.0.1:4566 s3 ls s3://demo-bucket/
'
```

## Boot Firecracker reel : verifie (systemd requis)

Verifie reellement en bootant le `rootfs.ext4` de ce devcontainer (meme
procedure que `image-builder` : export d'image + `mke2fs -d`) directement
avec `atelier-firecracker`, **boot_args par defaut, sans `init=`
personnalise** — exactement ce que fait `vm-supervisor` en production (voir
`crates/firecracker/tests/agent_boot_smoke.rs`).

Premier constat (sans systemd dans l'image) : le noyau ne trouve aucun
`/sbin/init`/`/etc/init`/`/bin/init` et retombe sur un `/bin/sh` nu comme
PID 1 — rien ne demarre jamais tout seul, `postStartCommand`
(`devcontainer.json`) n'etant rejoue que par le CLI `devcontainer`, jamais
par le noyau. **Corrige** : `systemd`/`systemd-sysv` ajoutes au Dockerfile,
`ministack` et `code-server` demarres via deux unites systemd dediees
(`atelier-ministack.service`, `atelier-code-server.service`,
`WantedBy=multi-user.target`) plutot que via `postStartCommand`. Piege
rencontre en le faisant : cette image de base intercepte `systemctl enable`
par un script factice ("systemd is not running in this container due to
son overhead", pense pour eviter des echecs de paquets .deb pendant un
`docker build` classique) — les symlinks d'activation sont donc crees a la
main (`ln -s` dans `/etc/systemd/system/multi-user.target.wants/`), comme
le fait `deb-systemd-helper` pour les paquets (`docker.service` s'active
ainsi correctement tout seul via le paquet Docker, sans intervention).

Resultat, boot reel confirme : `code-server:8080` et `ministack:4566`
repondent tous les deux a une vraie connexion TCP depuis l'hote, dans une
microVM Firecracker bootee exactement comme le fait `vm-supervisor` (meme
boot_args, memes contraintes qu'en production).

## Limite assumee restante

Verifie a la main (rootfs construit manuellement, boot Firecracker isole
via `cargo test`), **pas encore** a travers le pipeline K8s complet
(`Workshop` reel → `image-builder` → cache → `vm-supervisor`, comme les
autres Workshops de ce projet) : reste a creer un vrai `Workshop` pointant
sur ce depot (`Workshop.spec.devcontainer.repo`, `config_path:
demo/ministack-workshop/.devcontainer/devcontainer.json`) une fois l'auth
git sur ce depot prive geree cote `image-builder` (les Workshops testes
jusqu'ici utilisaient des depots publics).

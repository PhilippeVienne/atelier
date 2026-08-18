# image-builder de developpement

Pipeline reel (verifie manuellement de bout en bout, y compris boot
Firecracker du resultat) : `envbuilder` clone le repo et construit le
devcontainer, **le pousse comme image OCI standard** vers un registre, puis
`crane export` l'aplatit en tarball, extrait, empaquete en ext4
(`mke2fs -d`), publie dans le cache (aujourd'hui un repertoire — cf.
« PVC » ci-dessous).

Point de conception issu de tests reels : `envbuilder` ne produit **pas** de
dossier d'export propre. Il construit "en place" sur `/` (il commence par
*supprimer* le contenu du systeme de fichiers du conteneur qui l'execute,
sauf `/.envbuilder`, avant d'y extraire l'image cible), donc `image-builder`
doit tourner dans le **meme conteneur** qu'envbuilder (`crates/image-builder/Dockerfile`),
et tout outil dont il a besoin *apres* l'appel a envbuilder (`crane`, le
cache PVC) doit vivre sur un **point de montage separe** de la racine du
conteneur, sans quoi envbuilder l'efface.

## Recuperer les outils

```sh
# envbuilder (embarque dans l'image Dockerfile, mais utile pour experimenter)
docker create --name envbuilder-extract ghcr.io/coder/envbuilder:latest
docker cp envbuilder-extract:/.envbuilder/bin/envbuilder deploy/dev/envbuilder/envbuilder
docker rm envbuilder-extract
chmod +x deploy/dev/envbuilder/envbuilder

# crane (google/go-containerregistry)
curl -sL https://github.com/google/go-containerregistry/releases/download/v0.21.9/go-containerregistry_Linux_x86_64.tar.gz \
  | tar xz -C deploy/dev/crane crane
chmod +x deploy/dev/crane/crane
```

Ces binaires (`deploy/dev/envbuilder/`, `deploy/dev/crane/`) sont ignores par
git (gros fichiers).

## Registre de developpement

```sh
docker run -d --name atelier-registry-dev -p 5000:5000 registry:2
```

## Construire l'image

```sh
docker build -t atelier-image-builder:dev -f crates/image-builder/Dockerfile .
```

## Lancer un build reel

```sh
mkdir -p /var/tmp/atelier-image-cache-dev
docker run --rm --privileged --network host \
  -e ATELIER_DEVCONTAINER_REPO=https://github.com/microsoft/vscode-remote-try-python \
  -e ATELIER_DEVCONTAINER_REVISION=main \
  -e ATELIER_WORKSHOP_NAME=<nom-du-workshop> \
  -e ATELIER_WORKSHOP_NAMESPACE=default \
  -e ATELIER_REGISTRY_ADDR=localhost:5000 \
  -e ATELIER_REGISTRY_INSECURE=true \
  -e ATELIER_IMAGE_CACHE_DIR=/cache \
  -e ATELIER_CRANE_BIN=/tools/crane \
  -e KUBECONFIG=/kubeconfig/config \
  -v /var/tmp/atelier-image-cache-dev:/cache \
  -v "$HOME/.kube":/kubeconfig \
  -v "$(pwd)/deploy/dev/crane":/tools:ro \
  atelier-image-builder:dev
```

`--privileged` est necessaire pour qu'envbuilder puisse extraire les couches
de l'image de base (manipulation de systeme de fichiers). Le Workshop cible
doit deja exister dans le cluster avec `status.phase` renseigne (le
`controller` s'en charge normalement ; pour un test isole, un
`kubectl patch --subresource status` suffit).

## PVC (cible reelle, pas ce script de dev)

En production/cible, `ATELIER_IMAGE_CACHE_DIR` pointe vers un PVC Kubernetes
monte par le `controller` : en lecture-ecriture dans le Job image-builder,
en lecture seule dans le pod parent (pour que `vm-supervisor` y lise le
rootfs via `ATELIER_VM_ROOTFS_PATH`). Offload/reload vers S3 quand le PVC
est trop rempli : envisage plus tard, pas implemente (cf.
docs/ARCHITECTURE.md).

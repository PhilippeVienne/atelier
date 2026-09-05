#!/usr/bin/env bash
# Export/import des images conteneurs necessaires a `helm install
# charts/atelier` pour un deploiement DECONNECTE (air-gap), tache 11.5,
# spec docs/specs/15-souverainete-airgap-inference-gpu.md §3.3.
#
# Contrairement a `deploy/terraform/aws/mirror-images.sh` (copie
# registre-a-registre via `crane copy`, exige que la source ET la
# destination soient joignables EN MEME TEMPS) : ce script produit une
# archive PORTABLE (`crane pull` vers un fichier local), a transferer par
# n'importe quel moyen hors reseau (cle USB, support physique...) puis a
# rejouer sur le registre d'entreprise cible (`crane push`) sans jamais
# necessiter d'acces simultane aux deux reseaux.
#
# `crane` (github.com/google/go-containerregistry) est deja vendu et utilise
# ailleurs dans ce depot (`deploy/dev/crane/crane`, `crates/image-builder`) :
# reutilise ici plutot que d'introduire une dependance a `skopeo`/`docker`.
#
# Usage :
#   ./scripts/airgap-bundle.sh export [--with-gpu] [output-dir]
#   ./scripts/airgap-bundle.sh import --registry harbor.internal.corp/atelier [bundle.tar.gz]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
VERSION="$(sed -n 's/^version: *//p' "$REPO_ROOT/charts/atelier/Chart.yaml" | head -1)"

CRANE="$REPO_ROOT/deploy/dev/crane/crane"
if [ ! -x "$CRANE" ]; then
  CRANE="$(command -v crane || true)"
fi
[ -n "$CRANE" ] || {
  echo "crane introuvable (deploy/dev/crane/crane ou PATH — voir .github/workflows/docker-ghcr.yml pour la commande de telechargement)" >&2
  exit 1
}

# Memes images que `deploy/terraform/aws/mirror-images.sh` (a garder
# synchronise MANUELLEMENT avec `charts/atelier/values.yaml` — aucun des
# deux scripts ne derive cette liste dynamiquement du chart), mais
# completee : `mirror-images.sh` a pris du retard sur `values.yaml` et omet
# `observability`/`s3Storage` (RustFS)/`gpu`/`litellmVllmModelInit`
# (postgres pour `pgvector` DIFFERE de `postgres:16-alpine`, image du CLIENT
# `psql` des Jobs d'initialisation — deux images distinctes, verifie dans
# `values.yaml`).
THIRD_PARTY_IMAGES=(
  "postgres:16-alpine"                    # initJobs.dbInit/dbMigrate (client psql)
  "pgvector/pgvector:pg16"                # postgresql (base de donnees principale)
  "quay.io/keycloak/keycloak:24.0"
  "codeberg.org/forgejo/forgejo:7.0"
  "openbao/openbao:2.0.0"
  "ghcr.io/berriai/litellm:main-latest"
  "redis:7.2-alpine"
  "minio/mc:latest"                       # initJobs.s3Init
  "registry:2"
  "rustfs/rustfs:latest"                  # s3Storage (RustFS embarque)
  "grafana/otel-lgtm:0.32.1"              # observability
  "curlimages/curl:8.11.0"                # initJobs.litellmVllmModelInit
)
# Volumineuse (plusieurs Go) et seulement necessaire si `gpu.enabled: true`
# (tache 11.3) : exclue par defaut, incluse via `--with-gpu`.
GPU_IMAGE="vllm/vllm-openai:v0.6.6"

# Images geres directement par les templates Helm de ce chart (tag "latest",
# `image.repository` a surcharger UN PAR UN dans les values de la cible —
# **piege trouve en verifiant** : ce chart n'a PAS de `global.imageRegistry`
# (contrairement a ce qu'affirmait la premiere redaction de la spec 15
# §3.3) : chaque composant porte son propre `<composant>.image.repository`
# independant, aucun mecanisme de prefixe global n'existe dans
# `charts/atelier/templates/`).
HELM_MANAGED_COMPONENTS=(controller api-server dashboard pm-engine kvm-device-plugin)
# Images injectees directement par le controller dans les pods Workshop
# (`ATELIER_COMPONENT_IMAGE_REGISTRY`, `crates/controller/src/reconcile.rs::
# component_image_ref`) : SEUL mecanisme de prefixe de registre reellement
# generique de ce depot, mais limite a ces 5 composants. Retagues "dev" a
# l'import, meme convention que `mirror-images.sh` (le controller demande
# toujours ce tag fixe, quel que soit le tag source).
CONTROLLER_INJECTED_COMPONENTS=(net-proxy identity-proxy vm-supervisor mcp-gateway image-builder)
ATELIER_IMAGE_PREFIX="ghcr.io/philippevienne/atelier-"

usage() {
  cat >&2 <<EOF
Usage:
  $0 export [--with-gpu] [output-dir]
  $0 import --registry <registre> [bundle.tar.gz]

  export : a executer sur une machine AVEC acces Internet. Produit
           <output-dir>/atelier-images-$VERSION.tar.gz (output-dir par
           defaut : ./airgap-bundle).
  import : a executer sur la machine/le registre d'entreprise CIBLE
           (Harbor, Nexus...), a partir du fichier .tar.gz transfere.
EOF
  exit 1
}

safe_name() {
  # Un nom de fichier ne peut pas porter '/' ni ':' — meme transformation
  # des deux cotes (export/import) pour rester symetrique.
  echo "$1" | tr '/:' '__'
}

do_export() {
  local with_gpu=0
  local out_dir="./airgap-bundle"
  while [ $# -gt 0 ]; do
    case "$1" in
      --with-gpu) with_gpu=1; shift ;;
      *) out_dir="$1"; shift ;;
    esac
  done

  local images=("${THIRD_PARTY_IMAGES[@]}")
  if [ "$with_gpu" -eq 1 ]; then
    images+=("$GPU_IMAGE")
  fi
  for component in "${HELM_MANAGED_COMPONENTS[@]}" "${CONTROLLER_INJECTED_COMPONENTS[@]}"; do
    images+=("${ATELIER_IMAGE_PREFIX}${component}:latest")
  done

  local staging="$out_dir/atelier-images-$VERSION"
  mkdir -p "$staging"
  : > "$staging/manifest.txt"

  echo "==> Export de ${#images[@]} image(s) vers $staging"
  for image in "${images[@]}"; do
    local tarball
    tarball="$staging/$(safe_name "$image").tar"
    echo "  - $image"
    "$CRANE" pull "$image" "$tarball"
    echo "$image" >> "$staging/manifest.txt"
  done

  local archive="$out_dir/atelier-images-$VERSION.tar.gz"
  echo "==> Archivage vers $archive"
  tar -czf "$archive" -C "$out_dir" "atelier-images-$VERSION"
  rm -rf "$staging"

  echo
  echo "Termine : $archive"
  echo "Transferer ce fichier vers l'environnement deconnecte, puis :"
  echo "  ./scripts/airgap-bundle.sh import --registry <registre-entreprise> $archive"
}

do_import() {
  local registry=""
  local bundle=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --registry) registry="$2"; shift 2 ;;
      *) bundle="$1"; shift ;;
    esac
  done
  [ -n "$registry" ] || { echo "--registry est requis" >&2; usage; }
  [ -n "$bundle" ] || bundle="./airgap-bundle/atelier-images-$VERSION.tar.gz"
  [ -f "$bundle" ] || { echo "archive introuvable : $bundle" >&2; exit 1; }

  # PAS de `local` : le `trap ... EXIT` s'execute apres le retour de cette
  # fonction, au niveau du script entier — une variable `local` serait alors
  # hors de portee ("unbound variable" sous `set -u`), constate en testant
  # reellement (`import` reussi mais message d'erreur final trompeur).
  work_dir="$(mktemp -d)"
  trap 'rm -rf "$work_dir"' EXIT
  echo "==> Extraction de $bundle"
  tar -xzf "$bundle" -C "$work_dir"
  local staging
  staging="$(find "$work_dir" -maxdepth 1 -type d -name 'atelier-images-*')"
  [ -n "$staging" ] || { echo "archive invalide : repertoire atelier-images-* absent" >&2; exit 1; }
  [ -f "$staging/manifest.txt" ] || { echo "archive invalide : manifest.txt absent" >&2; exit 1; }

  echo "==> Import vers $registry"
  while IFS= read -r image; do
    [ -n "$image" ] || continue
    local tarball
    tarball="$staging/$(safe_name "$image").tar"
    local repo="${image%%:*}"
    local tag="${image##*:}"
    local base_name="${repo##*/}"

    if [[ " ${CONTROLLER_INJECTED_COMPONENTS[*]} " == *" ${base_name#atelier-} "* ]] && [[ "$repo" == "${ATELIER_IMAGE_PREFIX}"* ]]; then
      # Meme retaguage "dev" que `mirror-images.sh` : le controller demande
      # toujours ce tag fixe pour ces 5 composants.
      tag="dev"
    fi

    local dest="$registry/$base_name:$tag"
    echo "  - $image -> $dest"
    "$CRANE" push "$tarball" "$dest"
  done < "$staging/manifest.txt"

  cat <<EOF

Termine. Cette cible n'a PAS de "global.imageRegistry" (verifie dans
charts/atelier/templates/ : aucun template ne le lit) — chaque composant
gere par Helm doit voir son "image.repository" surcharge INDIVIDUELLEMENT
dans les values de l'installation, ex:
  apiServer:
    image:
      repository: $registry/atelier-api-server
  controller:
    image:
      repository: $registry/atelier-controller
  # ... (dashboard, pm-engine, kvm-device-plugin, postgresql, keycloak,
  #      forgejo, openbao, litellm, redis, registry, s3Storage.rustfs,
  #      observability, gpu — un "image.repository"/"image.tag" par
  #      section, voir charts/atelier/values.yaml)

Les 5 composants injectes par le controller (net-proxy/identity-proxy/
vm-supervisor/mcp-gateway/image-builder), eux, se reconfigurent en une seule
variable :
  controller:
    env:
      ATELIER_COMPONENT_IMAGE_REGISTRY: "$registry"
EOF
}

[ $# -ge 1 ] || usage
cmd="$1"; shift
case "$cmd" in
  export) do_export "$@" ;;
  import) do_import "$@" ;;
  *) usage ;;
esac

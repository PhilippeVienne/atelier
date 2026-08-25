#!/usr/bin/env bash
# Alimente les depots ECR crees par modules/ecr (deploy/terraform/aws) :
# copie registre-a-registre (`crane copy`, sans passer par un docker local)
# de toutes les images necessaires a un `helm install charts/atelier` en
# mode "airgap ECR" (helm_values_snippet, module.cluster) :
#
#   - Dependances tierces (postgres/keycloak/forgejo/openbao/litellm/redis/
#     minio-mc), depuis leur registre public d'origine (memes tags que
#     charts/atelier/values.yaml).
#   - Les 10 images de composants Atelier, deja publiees par la CI
#     (.github/workflows/docker-ghcr.yml, a chaque push sur main) vers
#     ghcr.io/philippevienne/atelier-<composant>:latest - pas de build
#     local necessaire ici.
#     - 5 gerees par le chart (controller/api-server/dashboard/pm-engine/
#       kvm-device-plugin) : reproduites sous le meme tag "latest" (seul
#       `image.repository` est surcharge par helm_values_snippet, pas
#       `image.tag`).
#     - 5 injectees directement par le controller dans les pods Workshop
#       (net-proxy/identity-proxy/vm-supervisor/mcp-gateway/image-builder,
#       voir ATELIER_COMPONENT_IMAGE_REGISTRY dans
#       crates/controller/src/reconcile.rs) : RE-taguees "dev" a la volee
#       (le controller demande toujours ce tag fixe, quel que soit le tag
#       source - `crane copy` peut changer le tag de destination).
#
# Usage : ./mirror-images.sh [region]
set -euo pipefail

REGION="${1:-eu-west-3}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TF_DIR="$SCRIPT_DIR/live/dev"

CRANE="$SCRIPT_DIR/../../dev/crane/crane"
if [ ! -x "$CRANE" ]; then
  CRANE="$(command -v crane || true)"
fi
[ -n "$CRANE" ] || {
  echo "crane introuvable (deploy/dev/crane/crane ou PATH - voir .github/workflows/docker-ghcr.yml pour la commande de telechargement)" >&2
  exit 1
}
command -v docker >/dev/null || {
  echo "docker requis (pour l'authentification ECR consommee par crane)" >&2
  exit 1
}
command -v aws >/dev/null || {
  echo "aws CLI requis" >&2
  exit 1
}

REGISTRY="$(cd "$TF_DIR" && terraform output -raw ecr_registry)"
echo "==> Registre ECR : $REGISTRY"

aws ecr get-login-password --region "$REGION" | docker login --username AWS --password-stdin "$REGISTRY" >/dev/null
echo "==> Authentifie aupres de $REGISTRY"

mirror() {
  local src="$1" dst="$2"
  echo "==> $src -> $REGISTRY/$dst"
  "$CRANE" copy "$src" "$REGISTRY/$dst"
}

echo "--- Dependances tierces ---"
mirror "postgres:16-alpine" "mirror/postgres:16-alpine"
mirror "quay.io/keycloak/keycloak:24.0" "mirror/keycloak:24.0"
mirror "codeberg.org/forgejo/forgejo:7.0" "mirror/forgejo:7.0"
mirror "openbao/openbao:2.0.0" "mirror/openbao:2.0.0"
mirror "ghcr.io/berriai/litellm:main-latest" "mirror/litellm:main-latest"
mirror "redis:7.2-alpine" "mirror/redis:7.2-alpine"
mirror "minio/mc:latest" "mirror/minio-mc:latest"

echo "--- Images Atelier gerees par le chart (tag inchange: latest) ---"
for component in controller api-server dashboard pm-engine kvm-device-plugin; do
  mirror "ghcr.io/philippevienne/atelier-$component:latest" "atelier-$component:latest"
done

echo "--- Images Atelier injectees par le controller (re-taguees dev) ---"
for component in net-proxy identity-proxy vm-supervisor mcp-gateway image-builder; do
  mirror "ghcr.io/philippevienne/atelier-$component:latest" "atelier-$component:dev"
done

cat <<'EOF'

Termine. Prochaine etape :
  cd live/dev
  terraform output -raw helm_values_snippet > ../../../../../aws-values.yaml
  helm upgrade --install atelier ../../../../../charts/atelier \
    --namespace <irsa_namespace> --create-namespace \
    -f ../../../../../aws-values.yaml
EOF

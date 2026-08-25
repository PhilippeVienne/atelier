#!/usr/bin/env bash
# Provisionne le backend d'etat Terraform (bucket S3, verrouillage natif
# S3 - `use_lockfile`, Terraform >= 1.10, pas de table DynamoDB) pour
# deploy/terraform/aws/live/<env>/, via l'AWS CLI plutot qu'un module
# Terraform separe : evite le probleme de l'oeuf et de la poule (un
# Terraform ne peut pas gerer le bucket qui hebergera son propre state)
# sans dupliquer un root Terraform complet rien que pour ca.
#
# Nom de bucket derive de l'ID de compte AWS courant
# (tf-state-<account-id>-atelier) - globalement unique sans coordination,
# et reproductible : relancer ce script sur le meme compte retombe
# toujours sur le meme nom, sans avoir a le retenir/documenter a part.
#
# Idempotent : ne recree rien si le bucket existe deja.
#
# Usage : ./bootstrap.sh [region] [environment]
#   region      defaut eu-west-3
#   environment defaut dev (nom du repertoire dans live/)
set -euo pipefail

REGION="${1:-eu-west-3}"
ENVIRONMENT="${2:-dev}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIVE_DIR="$ROOT_DIR/live/$ENVIRONMENT"

if [ ! -d "$LIVE_DIR" ]; then
  echo "live/$ENVIRONMENT introuvable ($LIVE_DIR) - creer ce repertoire d'abord (copier live/dev/ comme modele)." >&2
  exit 1
fi

command -v aws >/dev/null || {
  echo "aws CLI requis" >&2
  exit 1
}

ACCOUNT_ID="$(aws sts get-caller-identity --query Account --output text)"
BUCKET="tf-state-${ACCOUNT_ID}-atelier"

echo "==> Compte AWS ${ACCOUNT_ID}, region ${REGION}, environnement ${ENVIRONMENT}"

if aws s3api head-bucket --bucket "$BUCKET" --region "$REGION" 2>/dev/null; then
  echo "==> Bucket $BUCKET deja present"
else
  echo "==> Creation du bucket $BUCKET"
  if [ "$REGION" = "us-east-1" ]; then
    aws s3api create-bucket --bucket "$BUCKET" --region "$REGION"
  else
    aws s3api create-bucket --bucket "$BUCKET" --region "$REGION" \
      --create-bucket-configuration LocationConstraint="$REGION"
  fi
  aws s3api put-bucket-versioning --bucket "$BUCKET" \
    --versioning-configuration Status=Enabled
  aws s3api put-bucket-encryption --bucket "$BUCKET" \
    --server-side-encryption-configuration '{"Rules":[{"ApplyServerSideEncryptionByDefault":{"SSEAlgorithm":"AES256"}}]}'
  aws s3api put-public-access-block --bucket "$BUCKET" \
    --public-access-block-configuration BlockPublicAcls=true,IgnorePublicAcls=true,BlockPublicPolicy=true,RestrictPublicBuckets=true
fi

cat >"$LIVE_DIR/backend.hcl" <<EOF
bucket       = "$BUCKET"
key          = "atelier/$ENVIRONMENT/terraform.tfstate"
region       = "$REGION"
use_lockfile = true
encrypt      = true
EOF
echo "==> Backend ecrit dans live/$ENVIRONMENT/backend.hcl"

TFVARS="$LIVE_DIR/terraform.tfvars"
if [ ! -f "$TFVARS" ]; then
  cp "$LIVE_DIR/terraform.tfvars.example" "$TFVARS"
  echo "==> terraform.tfvars cree depuis l'exemple - a completer (domain_name/cloudflare_zone_id notamment) : $TFVARS"
else
  echo "==> terraform.tfvars deja present, non touche : $TFVARS"
fi

cat <<EOF

Prochaine etape :
  cd live/$ENVIRONMENT
  terraform init -backend-config=backend.hcl
EOF

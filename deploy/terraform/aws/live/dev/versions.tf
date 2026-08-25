# Root Terraform (le seul endroit ou `terraform init/plan/apply` s'execute
# reellement) : configure providers + backend, et appelle le module
# reutilisable (../../modules/cluster). Un second environnement
# (ex: live/prod/) reutiliserait le meme module avec ses propres
# providers/backend/variables, sans dupliquer vpc.tf/eks.tf/etc.
terraform {
  # >= 1.11 : verrouillage natif S3 (`use_lockfile`, backend.hcl genere par
  # ../../bootstrap.sh) - experimental en 1.10, `dynamodb_table` marque
  # deprecated a partir de 1.11 (plus de table DynamoDB necessaire).
  required_version = ">= 1.11"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = ">= 6.60.0, < 7.0.0"
    }
    cloudflare = {
      source  = "cloudflare/cloudflare"
      version = "~> 5.0"
    }
  }

  # Backend configure via `terraform init -backend-config=...` (voir
  # README.md) plutot que des valeurs en dur ici : le bucket S3 du state
  # est lui-meme cree par ../../bootstrap.sh, donc son nom n'est connu
  # qu'apres l'avoir execute.
  backend "s3" {}
}

provider "aws" {
  region = var.region

  default_tags {
    tags = {
      "atelier.dev/managed-by"  = "terraform"
      "atelier.dev/environment" = var.environment
    }
  }
}

# Aucun `api_token` en dur ici : le provider lit CLOUDFLARE_API_TOKEN dans
# l'environnement (voir README.md) - jamais ecrit dans un fichier .tf/.tfvars.
provider "cloudflare" {}

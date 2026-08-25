# Module reutilisable : aucun bloc provider{}/backend{} ici, voir
# modules/cluster/versions.tf pour la justification (identique).
terraform {
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = ">= 6.0.0, < 7.0.0"
    }
  }
}

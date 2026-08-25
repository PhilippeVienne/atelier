# Module reutilisable : uniquement les contraintes de version des
# providers qu'il utilise. Ni bloc `provider {}` ni `backend {}` ici -
# c'est au root qui appelle ce module (live/<env>/) de les configurer,
# pour permettre plusieurs environnements (comptes/regions differents)
# sans dupliquer cette logique.
terraform {
  required_providers {
    aws = {
      source = "hashicorp/aws"
      # >= 6.60 : premiere version connue exposant cpu_options.nested_virtualization
      # sur aws_launch_template (fonctionnalite EC2 introduite en fevrier 2026,
      # voir variables.tf / node_instance_type pour le detail).
      version = ">= 6.60.0, < 7.0.0"
    }
  }
}

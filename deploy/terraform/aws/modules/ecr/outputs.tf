output "registry" {
  description = "Hote du registre ECR (<compte>.dkr.ecr.<region>.amazonaws.com), sans nom de depot. Valeur exacte a donner a ATELIER_COMPONENT_IMAGE_REGISTRY (crates/controller/src/reconcile.rs) : les depots atelier-net-proxy/etc. portent deja le prefixe \"atelier-\" dans leur NOM, pas dans un sous-chemin - ne pas rajouter \"/atelier\" ici."
  value       = split("/", values(aws_ecr_repository.this)[0].repository_url)[0]
}

output "repository_urls" {
  description = "URL complete (registre + nom) de chaque depot cree, par nom de depot."
  value       = { for name, repo in aws_ecr_repository.this : name => repo.repository_url }
}

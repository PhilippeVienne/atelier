variable "domain_name" {
  description = "Domaine racine des 4 Ingress du chart (docs/admin-guide.md section 2 : auth./git./app./api.<domain_name>). Une zone Route53 est creee pour ce nom exact - le domaine parent reste gere ailleurs (Cloudflare) et doit deleguer via les NS renvoyes par ce module (output name_servers). Aucune valeur par defaut : nom de domaine reel, a fournir via live/<env>/terraform.tfvars (gitignore, jamais commite)."
  type        = string
}

variable "cloudflare_zone_id" {
  description = "Zone ID Cloudflare de la zone PARENTE (ex: exemple.com, pas atelier.exemple.com) dans laquelle la delegation NS de var.domain_name est ecrite (cloudflare.tf). Necessite CLOUDFLARE_API_TOKEN dans l'environnement (jamais dans ce fichier), scope minimal Zone:DNS:Edit sur cette zone. Aucune valeur par defaut : identifiant reel, a fournir via live/<env>/terraform.tfvars (gitignore, jamais commite)."
  type        = string
}

variable "cluster_name" {
  description = "Utilise uniquement pour taguer la zone Route53 (aucune dependance fonctionnelle envers modules/cluster)."
  type        = string
  default     = "atelier"
}

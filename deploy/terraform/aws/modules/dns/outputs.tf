output "zone_id" {
  description = "ID de la zone Route53 creee pour var.domain_name (a reutiliser pour y ajouter plus tard les enregistrements A/ALIAS vers l'Ingress, une fois le chart Helm installe)."
  value       = aws_route53_zone.atelier.zone_id
}

output "name_servers" {
  description = "Serveurs de noms Route53 (deja pousses dans Cloudflare par ce module - expose ici pour verification, ex: `dig NS`)."
  value       = aws_route53_zone.atelier.name_servers
}

output "domain_name" {
  description = "Reflet de var.domain_name, pour que le root puisse le passer tel quel a modules/cluster sans le redeclarer deux fois s'il prefere le lire depuis ce module."
  value       = var.domain_name
}

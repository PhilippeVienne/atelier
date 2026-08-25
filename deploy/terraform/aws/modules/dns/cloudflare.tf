# Delegation NS de var.domain_name (sous-domaine de la zone Cloudflare
# parente, ex: exemple.com) vers la zone Route53 creee dans route53.tf -
# ferme la boucle "map Cloudflare" sans etape manuelle. Ce module est
# independant du cycle de vie du cluster EKS (modules/cluster) : la zone
# et sa delegation survivent a un "down" complet du cluster (voir
# README.md).
resource "cloudflare_dns_record" "atelier_delegation" {
  # Index statiques (pas `toset(aws_route53_zone.atelier.name_servers)`) :
  # a la creation, cette liste n'est connue qu'apres l'apply de la zone
  # (les NS sont assignes par AWS a ce moment-la) - Terraform refuse un
  # for_each dont l'ensemble des CLES depend d'une valeur pas encore
  # connue, meme si sa cardinalite (toujours 4 pour une zone publique
  # Route53) est fixe. Indexer par position statique contourne le
  # probleme : seule la VALEUR lue a chaque cle est resolue a l'apply.
  for_each = toset(["0", "1", "2", "3"])

  zone_id = var.cloudflare_zone_id
  name    = var.domain_name
  type    = "NS"
  content = aws_route53_zone.atelier.name_servers[tonumber(each.key)]
  ttl     = 3600
  # Un enregistrement NS ne peut jamais etre proxy (orange-cloud) - la
  # delegation doit rester en resolution DNS directe vers Route53.
}

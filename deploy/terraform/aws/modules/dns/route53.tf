# Zone hebergee pour `var.domain_name` (les 4 sous-domaines d'Ingress du
# chart - docs/admin-guide.md section 2 - vivront dedans : auth./git./app.
# /api.<domain_name>). Le domaine parent (ex: exemple.com) reste gere chez un
# registrar/DNS tiers (Cloudflare) : cette zone ne devient autoritaire pour
# <domain_name> qu'une fois les NS ci-dessous ajoutes cote Cloudflare (voir
# output route53_name_servers et README.md).
#
# Delibrement une zone separee pour le sous-domaine plutot qu'une zone pour
# exemple.com entier : la delegation NS ne transfere l'autorite que sur
# atelier.exemple.com, Cloudflare reste seul maitre du reste de la zone
# racine.
resource "aws_route53_zone" "atelier" {
  name = var.domain_name

  tags = {
    "atelier.dev/cluster" = var.cluster_name
  }
}

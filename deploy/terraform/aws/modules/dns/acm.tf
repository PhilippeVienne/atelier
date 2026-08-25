# Certificat ACM wildcard (couvre auth./git./app./api.<domain_name> en une
# seule fois) pour le futur ALB (voir modules/cluster/alb-controller.tf) -
# termine le TLS cote AWS, pas besoin de cert-manager/Let's Encrypt puisque
# la zone Route53 necessaire a la validation DNS est deja possedee par ce
# module. ACM ne facture jamais un certificat public : seul l'ALB qui
# l'utilise a un cout (voir README.md).
resource "aws_acm_certificate" "atelier" {
  domain_name               = "*.${var.domain_name}"
  subject_alternative_names = [var.domain_name]
  validation_method         = "DNS"

  lifecycle {
    create_before_destroy = true
  }

  tags = {
    "atelier.dev/cluster" = var.cluster_name
  }
}

# `for_each` sur un set d'objets (pas juste des noms) : chaque SAN peut en
# theorie produire un enregistrement de validation different, meme si ici
# les deux (wildcard + apex) partagent le meme nom de sous-domaine
# "_xxxx.<domain_name>" en pratique - voir la doc du provider aws pour ce
# pattern standard de validation ACM par Route53.
resource "aws_route53_record" "acm_validation" {
  for_each = {
    for dvo in aws_acm_certificate.atelier.domain_validation_options : dvo.domain_name => {
      name   = dvo.resource_record_name
      record = dvo.resource_record_value
      type   = dvo.resource_record_type
    }
  }

  zone_id = aws_route53_zone.atelier.zone_id
  name    = each.value.name
  type    = each.value.type
  ttl     = 60
  records = [each.value.record]

  allow_overwrite = true
}

resource "aws_acm_certificate_validation" "atelier" {
  certificate_arn         = aws_acm_certificate.atelier.arn
  validation_record_fqdns = [for r in aws_route53_record.acm_validation : r.fqdn]
}

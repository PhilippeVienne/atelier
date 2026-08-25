# Roles IAM pour deux composants installes hors Terraform (Helm classique,
# voir README.md "Ingress") : AWS Load Balancer Controller (cree l'ALB
# depuis les Ingress `ingressClassName: alb`) et external-dns (cree les
# enregistrements Route53 depuis les memes Ingress, sans intervention
# manuelle). Pod Identity (pas IRSA) : meme raisonnement que pour le driver
# EBS CSI (eks.tf) - le role n'a pas besoin de connaitre l'URL OIDC du
# cluster, l'association se fait par namespace+ServiceAccount.

data "aws_iam_policy_document" "pod_identity_trust" {
  count = var.enable_cluster ? 1 : 0

  statement {
    actions = ["sts:AssumeRole", "sts:TagSession"]
    effect  = "Allow"

    principals {
      type        = "Service"
      identifiers = ["pods.eks.amazonaws.com"]
    }
  }
}

# --- AWS Load Balancer Controller ------------------------------------------

resource "aws_iam_role" "alb_controller" {
  count = var.enable_cluster ? 1 : 0

  name               = "${var.cluster_name}-${var.environment}-alb-controller"
  assume_role_policy = data.aws_iam_policy_document.pod_identity_trust[0].json

  tags = {
    "atelier.dev/cluster" = var.cluster_name
  }
}

# Politique officielle du projet upstream, copiee telle quelle (voir
# policies/alb-controller-iam-policy.json - source et date de recuperation
# en tete de fichier) plutot que reecrite a la main : c'est la reference
# maintenue par kubernetes-sigs/aws-load-balancer-controller, la reecrire
# introduirait un risque de divergence silencieuse a chaque nouvelle version
# du controller.
resource "aws_iam_policy" "alb_controller" {
  count = var.enable_cluster ? 1 : 0

  name   = "${var.cluster_name}-${var.environment}-alb-controller"
  policy = file("${path.module}/policies/alb-controller-iam-policy.json")
}

resource "aws_iam_role_policy_attachment" "alb_controller" {
  count = var.enable_cluster ? 1 : 0

  role       = aws_iam_role.alb_controller[0].name
  policy_arn = aws_iam_policy.alb_controller[0].arn
}

resource "aws_eks_pod_identity_association" "alb_controller" {
  count = var.enable_cluster ? 1 : 0

  cluster_name    = module.eks[0].cluster_name
  namespace       = "kube-system"
  service_account = "aws-load-balancer-controller"
  role_arn        = aws_iam_role.alb_controller[0].arn
}

# --- external-dns -----------------------------------------------------------

resource "aws_iam_role" "external_dns" {
  count = var.enable_cluster ? 1 : 0

  name               = "${var.cluster_name}-${var.environment}-external-dns"
  assume_role_policy = data.aws_iam_policy_document.pod_identity_trust[0].json

  tags = {
    "atelier.dev/cluster" = var.cluster_name
  }
}

# Scope a la seule zone Route53 de ce domaine (var.route53_zone_id) pour les
# operations d'ecriture - ListHostedZones/ListResourceRecordSets restent
# necessairement "*" (external-dns doit lister toutes les zones du compte au
# demarrage pour retrouver celle qui correspond a son domaine, l'API Route53
# ne permet pas de filtrer une liste par nom cote serveur).
data "aws_iam_policy_document" "external_dns" {
  count = var.enable_cluster ? 1 : 0

  statement {
    sid       = "ChangeOwnZoneOnly"
    effect    = "Allow"
    actions   = ["route53:ChangeResourceRecordSets"]
    resources = ["arn:aws:route53:::hostedzone/${var.route53_zone_id}"]
  }

  statement {
    sid    = "ListAllZones"
    effect = "Allow"
    actions = [
      "route53:ListHostedZones",
      "route53:ListResourceRecordSets",
      "route53:ListTagsForResource",
    ]
    resources = ["*"]
  }
}

resource "aws_iam_policy" "external_dns" {
  count = var.enable_cluster ? 1 : 0

  name   = "${var.cluster_name}-${var.environment}-external-dns"
  policy = data.aws_iam_policy_document.external_dns[0].json
}

resource "aws_iam_role_policy_attachment" "external_dns" {
  count = var.enable_cluster ? 1 : 0

  role       = aws_iam_role.external_dns[0].name
  policy_arn = aws_iam_policy.external_dns[0].arn
}

resource "aws_eks_pod_identity_association" "external_dns" {
  count = var.enable_cluster ? 1 : 0

  cluster_name    = module.eks[0].cluster_name
  namespace       = "kube-system"
  service_account = "external-dns"
  role_arn        = aws_iam_role.external_dns[0].arn
}

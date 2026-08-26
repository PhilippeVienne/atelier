# Auto-unseal KMS pour OpenBao (voir charts/atelier/templates/infra/openbao-statefulset.yaml,
# bloc "seal awskms" conditionnel) : remplace le split Shamir manuel
# (`bao operator unseal` x3 a CHAQUE redemarrage du pod) par un
# dechiffrement automatique via cette cle - necessaire des qu'un pod
# OpenBao redemarre (upgrade de version, scale-down/up du node group,
# reschedule) sans intervention humaine. `bao operator init` reste
# necessaire une seule fois (genere alors des "recovery keys", pas des
# "unseal keys" - utilisees seulement pour des operations d'urgence type
# rotation, jamais pour demarrer).
resource "aws_kms_key" "openbao_unseal" {
  count = var.enable_cluster ? 1 : 0

  description             = "${var.cluster_name}-${var.environment} OpenBao auto-unseal"
  deletion_window_in_days = 7
  enable_key_rotation     = true

  tags = {
    "atelier.dev/cluster" = var.cluster_name
  }
}

resource "aws_kms_alias" "openbao_unseal" {
  count = var.enable_cluster ? 1 : 0

  name          = "alias/${var.cluster_name}-${var.environment}-openbao-unseal"
  target_key_id = aws_kms_key.openbao_unseal[0].key_id
}

# IRSA (pas Pod Identity, contrairement a alb-controller.tf/ebs-csi) : le
# SDK AWS embarque dans l'image openbao/openbao 2.0.0 (utilise par son
# wrapper de seal "awskms") rejette l'endpoint EKS Pod Identity avec
# "HTTP credential provider invalid endpoint host, 169.254.170.23, only
# loopback hosts are allowed" - constate empiriquement, ce SDK est trop
# ancien pour la variante EKS Pod Identity du flux de identifiants
# conteneur (lancee fin 2023, support SDK progressif). IRSA (federation
# OIDC + fichier de jeton projete, `AWS_WEB_IDENTITY_TOKEN_FILE`) est le
# mecanisme le plus ancien/universel, supporte par toutes les versions du
# SDK AWS - voir la ServiceAccount "atelier-openbao" (openbao-statefulset.yaml)
# pour l'annotation eks.amazonaws.com/role-arn correspondante.
data "aws_iam_policy_document" "openbao_unseal_trust" {
  count = var.enable_cluster ? 1 : 0

  statement {
    actions = ["sts:AssumeRoleWithWebIdentity"]
    effect  = "Allow"

    principals {
      type        = "Federated"
      identifiers = [module.eks[0].oidc_provider_arn]
    }

    condition {
      test     = "StringEquals"
      variable = "${replace(module.eks[0].cluster_oidc_issuer_url, "https://", "")}:aud"
      values   = ["sts.amazonaws.com"]
    }

    condition {
      test     = "StringEquals"
      variable = "${replace(module.eks[0].cluster_oidc_issuer_url, "https://", "")}:sub"
      values   = ["system:serviceaccount:${var.irsa_namespace}:${var.cluster_name}-openbao"]
    }
  }
}

resource "aws_iam_role" "openbao_unseal" {
  count = var.enable_cluster ? 1 : 0

  name               = "${var.cluster_name}-${var.environment}-openbao-unseal"
  assume_role_policy = data.aws_iam_policy_document.openbao_unseal_trust[0].json

  tags = {
    "atelier.dev/cluster" = var.cluster_name
  }
}

data "aws_iam_policy_document" "openbao_unseal" {
  count = var.enable_cluster ? 1 : 0

  statement {
    actions = [
      "kms:Encrypt",
      "kms:Decrypt",
      "kms:DescribeKey",
    ]
    resources = [aws_kms_key.openbao_unseal[0].arn]
  }
}

resource "aws_iam_policy" "openbao_unseal" {
  count = var.enable_cluster ? 1 : 0

  name   = "${var.cluster_name}-${var.environment}-openbao-unseal"
  policy = data.aws_iam_policy_document.openbao_unseal[0].json
}

resource "aws_iam_role_policy_attachment" "openbao_unseal" {
  count = var.enable_cluster ? 1 : 0

  role       = aws_iam_role.openbao_unseal[0].name
  policy_arn = aws_iam_policy.openbao_unseal[0].arn
}

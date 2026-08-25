# Role IRSA unique : le chart charts/atelier applique `cloudIdentity.annotations`
# telles quelles a TOUS les ServiceAccounts qu'il cree (controller,
# apiServer, pmEngine, init-jobs) - voir charts/atelier/templates/_helpers.tpl
# et docs/admin-guide.md section 4.1. Un seul role est donc necessaire ;
# la trust policy est restreinte au namespace de la release Helm mais
# accepte n'importe quel ServiceAccount de ce namespace (le chart ne
# permet pas de les distinguer).
# Depend du fournisseur OIDC du cluster : detruit/recree avec lui en mode
# up/down (var.enable_cluster, voir eks.tf/variables.tf). L'ARN du role
# reste toutefois identique (deterministe : compte + nom) d'un cycle a
# l'autre, donc `helm_values_snippet` n'a pas besoin d'etre regenere.
data "aws_iam_policy_document" "irsa_trust" {
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
      test     = "StringLike"
      variable = "${replace(module.eks[0].cluster_oidc_issuer_url, "https://", "")}:sub"
      values   = ["system:serviceaccount:${var.irsa_namespace}:*"]
    }
  }
}

resource "aws_iam_role" "atelier" {
  count = var.enable_cluster ? 1 : 0

  name               = "${var.cluster_name}-${var.environment}"
  assume_role_policy = data.aws_iam_policy_document.irsa_trust[0].json

  tags = {
    "atelier.dev/cluster" = var.cluster_name
  }
}

# Acces objet (pas de s3:*) : Get/Put/Delete + multipart (crates/api-server
# src/storage.rs) sur sessions/snapshots, plus l'acces necessaire au hook
# `s3-init-job` (ListBucket/CreateBucket, sans effet ici puisque Terraform
# a deja cree les 3 buckets - voir s3.tf) et a Forgejo (LFS) sur son propre
# bucket.
data "aws_iam_policy_document" "atelier_s3" {
  statement {
    sid    = "ListOwnBuckets"
    effect = "Allow"
    actions = [
      "s3:ListBucket",
      "s3:GetBucketLocation",
      "s3:CreateBucket",
    ]
    resources = [for b in aws_s3_bucket.this : b.arn]
  }

  statement {
    sid    = "ReadWriteObjects"
    effect = "Allow"
    actions = [
      "s3:GetObject",
      "s3:PutObject",
      "s3:DeleteObject",
      "s3:AbortMultipartUpload",
      "s3:ListMultipartUploadParts",
    ]
    resources = [for b in aws_s3_bucket.this : "${b.arn}/*"]
  }
}

resource "aws_iam_policy" "atelier_s3" {
  name   = "${var.cluster_name}-${var.environment}-s3"
  policy = data.aws_iam_policy_document.atelier_s3.json
}

resource "aws_iam_role_policy_attachment" "atelier_s3" {
  count = var.enable_cluster ? 1 : 0

  role       = aws_iam_role.atelier[0].name
  policy_arn = aws_iam_policy.atelier_s3.arn
}

# Role du driver CSI EBS (voir eks.tf, addon aws-ebs-csi-driver) - separe du
# role IRSA `atelier` ci-dessus (usage applicatif) : celui-ci est assume par
# les pods du controller CSI via EKS Pod Identity, pas par les ServiceAccounts
# du chart.
data "aws_iam_policy_document" "ebs_csi_driver_trust" {
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

resource "aws_iam_role" "ebs_csi_driver" {
  count = var.enable_cluster ? 1 : 0

  name               = "${var.cluster_name}-${var.environment}-ebs-csi-driver"
  assume_role_policy = data.aws_iam_policy_document.ebs_csi_driver_trust[0].json

  tags = {
    "atelier.dev/cluster" = var.cluster_name
  }
}

resource "aws_iam_role_policy_attachment" "ebs_csi_driver" {
  count = var.enable_cluster ? 1 : 0

  role       = aws_iam_role.ebs_csi_driver[0].name
  policy_arn = "arn:aws:iam::aws:policy/service-role/AmazonEBSCSIDriverPolicy"
}

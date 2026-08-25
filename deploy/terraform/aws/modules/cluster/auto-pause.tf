# Filet de securite cout : scale automatiquement le node group a 0 chaque
# jour (voir README.md "Trois paliers", palier "pause" - PAS "down" :
# le control plane EKS et la NAT Gateway continuent de tourner, mais les
# donnees en cluster - PVC - survivent, contrairement a enable_cluster =
# false). Aucun Lambda : EventBridge Scheduler peut appeler directement
# n'importe quelle API AWS ("universal target",
# arn:aws:scheduler:::aws-sdk:eks:updateNodegroupConfig) depuis fin 2023.
#
# Volontairement PAS de schedule symetrique de reprise (remettre
# node_desired_size/node_min_size a leurs valeurs habituelles reste un
# `terraform apply -var=...` manuel, voir README.md) : ce filet de securite
# est pense pour eviter d'oublier de couper, pas pour redemarrer tout seul
# un jour ou vous ne travaillez pas dessus.
resource "aws_scheduler_schedule" "auto_pause" {
  count = var.enable_cluster && var.auto_pause_enabled ? 1 : 0

  name       = "${var.cluster_name}-${var.environment}-auto-pause"
  group_name = "default"

  schedule_expression          = var.auto_pause_schedule
  schedule_expression_timezone = var.auto_pause_timezone

  flexible_time_window {
    mode = "OFF"
  }

  target {
    arn      = "arn:aws:scheduler:::aws-sdk:eks:updateNodegroupConfig"
    role_arn = aws_iam_role.auto_pause[0].arn

    # PascalCase, pas camelCase : contrairement a ce que documente la
    # reference REST de l'API EKS, ce target universel valide contre le
    # modele du SDK, qui attend "ClusterName"/"NodegroupName"/
    # "ScalingConfig" - erreur reelle rencontree en testant ("Request
    # payload is missing the following field(s): ClusterName,
    # NodegroupName") avant cette correction.
    input = jsonencode({
      ClusterName   = module.eks[0].cluster_name
      NodegroupName = local.node_group_name
      ScalingConfig = {
        MinSize     = 0
        MaxSize     = var.node_max_size
        DesiredSize = 0
      }
    })
  }
}

# `node_group_id` (sortie du module eks) reprend le format de
# `aws_eks_node_group.id` ("<cluster>:<node-group>"), le nom reel du node
# group (avec son suffixe genere par AWS) est apres le ":".
locals {
  node_group_name = var.enable_cluster ? split(":", module.eks[0].eks_managed_node_groups[var.cluster_name].node_group_id)[1] : null
}

data "aws_iam_policy_document" "auto_pause_trust" {
  count = var.enable_cluster && var.auto_pause_enabled ? 1 : 0

  statement {
    actions = ["sts:AssumeRole"]
    effect  = "Allow"

    principals {
      type        = "Service"
      identifiers = ["scheduler.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "auto_pause" {
  count = var.enable_cluster && var.auto_pause_enabled ? 1 : 0

  name               = "${var.cluster_name}-${var.environment}-auto-pause"
  assume_role_policy = data.aws_iam_policy_document.auto_pause_trust[0].json

  tags = {
    "atelier.dev/cluster" = var.cluster_name
  }
}

data "aws_iam_policy_document" "auto_pause_permissions" {
  count = var.enable_cluster && var.auto_pause_enabled ? 1 : 0

  statement {
    actions   = ["eks:UpdateNodegroupConfig"]
    effect    = "Allow"
    resources = [module.eks[0].eks_managed_node_groups[var.cluster_name].node_group_arn]
  }
}

resource "aws_iam_role_policy" "auto_pause" {
  count = var.enable_cluster && var.auto_pause_enabled ? 1 : 0

  name   = "eks-update-nodegroup-config"
  role   = aws_iam_role.auto_pause[0].id
  policy = data.aws_iam_policy_document.auto_pause_permissions[0].json
}

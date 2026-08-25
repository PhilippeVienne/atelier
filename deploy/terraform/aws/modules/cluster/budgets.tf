# Garde-fou cout complementaire a auto-pause.tf : celui-ci scale a 0 sans
# intervention humaine, celui-la alerte par email quand le cout reel
# depasse un seuil - utile si l'auto-pause echoue silencieusement (ex:
# ressource hors node group qui coute, changement de var.node_max_size
# oublie) ou si un cluster additionnel est cree hors de ce module.
#
# Scope par tag plutot que sur le compte entier : filtre sur
# "atelier.dev/cluster" = var.cluster_name, la meme tag key/value appliquee
# a (quasi) toutes les ressources de ce module - voir eks.tf/vpc.tf/
# database.tf/s3.tf/iam.tf. Ne couvre donc PAS les couts hors-tag (ex:
# transferts de donnees inter-AZ non tagues) - un budget par compte reste
# recommande en complement si plusieurs projets partagent le meme compte.
resource "aws_budgets_budget" "atelier" {
  count = var.budget_enabled ? 1 : 0

  name         = "${var.cluster_name}-${var.environment}"
  budget_type  = "COST"
  limit_amount = tostring(var.budget_limit_usd)
  limit_unit   = "USD"
  time_unit    = "MONTHLY"

  cost_filter {
    name = "TagKeyValue"
    # format() plutot qu'une interpolation directe "cluster$${var...}" :
    # la sequence "$${" est l'echappement Terraform pour un "${" litteral,
    # ce qui desactiverait l'interpolation ici (piege classique du format
    # de filtre "TagKeyValue" d'AWS Budgets, qui utilise lui-meme "$" comme
    # separateur cle/valeur).
    values = [format("user:atelier.dev/cluster$%s", var.cluster_name)]
  }

  notification {
    comparison_operator        = "GREATER_THAN"
    threshold                  = var.budget_alert_threshold_percent
    threshold_type             = "PERCENTAGE"
    notification_type          = "ACTUAL"
    subscriber_email_addresses = [var.budget_alert_email]
  }
}

# Miroir des variables de ../../modules/cluster (mêmes noms/types/valeurs
# par défaut) : la documentation détaillée et les validations vivent dans
# le module — ce fichier ne fait que déclarer l'interface du root pour
# permettre `-var`/`terraform.tfvars` à ce niveau. Un second environnement
# (live/prod/) redéclarerait les mêmes variables avec ses propres valeurs.

variable "region" {
  type    = string
  default = "eu-west-3"
}

variable "environment" {
  type    = string
  default = "dev"
}

variable "cluster_name" {
  type    = string
  default = "atelier"
}

variable "kubernetes_version" {
  type    = string
  default = "1.36"
}

variable "vpc_cidr" {
  type    = string
  default = "10.42.0.0/16"
}

variable "availability_zones" {
  type    = list(string)
  default = ["eu-west-3a", "eu-west-3b", "eu-west-3c"]
}

variable "node_availability_zone" {
  type    = string
  default = "eu-west-3a"
}


# Pas de valeur par defaut (voir modules/cluster/variables.tf) : force a
# fournir son IP admin explicitement via terraform.tfvars.
variable "admin_access_cidrs" {
  type = list(string)
}

variable "cluster_log_retention_days" {
  type    = number
  default = 14
}

variable "flow_log_retention_days" {
  type    = number
  default = 14
}

variable "enable_cluster" {
  type    = bool
  default = true
}

variable "node_instance_type" {
  type    = string
  default = "m7i.xlarge"
}

variable "node_desired_size" {
  type    = number
  default = 2
}

variable "node_min_size" {
  type    = number
  default = 1
}

variable "node_max_size" {
  type    = number
  default = 4
}

variable "node_disk_size_gb" {
  type    = number
  default = 100
}

variable "auto_pause_enabled" {
  type    = bool
  default = true
}

variable "auto_pause_schedule" {
  type    = string
  default = "cron(0 2 * * ? *)"
}

variable "auto_pause_timezone" {
  type    = string
  default = "Europe/Paris"
}

variable "db_engine_version" {
  type    = string
  default = "16.9"
}

variable "db_master_database" {
  type    = string
  default = "postgres"
}

variable "db_master_username" {
  type    = string
  default = "atelier_admin"
}

variable "db_min_acu" {
  type    = number
  default = 0
}

variable "db_max_acu" {
  type    = number
  default = 4
}

variable "db_auto_pause_seconds" {
  type    = number
  default = 300
}

variable "db_backup_retention_days" {
  type    = number
  default = 7
}

variable "db_deletion_protection" {
  type    = bool
  default = true
}

variable "db_secret_rotation_days" {
  type    = number
  default = 90
}

variable "s3_bucket_prefix" {
  type    = string
  default = "atelier"
}

variable "irsa_namespace" {
  type    = string
  default = "default"
}

variable "budget_enabled" {
  type    = bool
  default = true
}

variable "budget_limit_usd" {
  type    = number
  default = 50
}

variable "budget_alert_threshold_percent" {
  type    = number
  default = 80
}

# Pas de valeur par defaut : requis si budget_enabled = true (voir
# modules/cluster/variables.tf).
variable "budget_alert_email" {
  type    = string
  default = null
}

# Pas de valeur par defaut pour ces deux-la : nom de domaine et zone
# Cloudflare reels, a fournir via terraform.tfvars (gitignore, jamais
# commite - voir terraform.tfvars.example pour le format attendu).
variable "domain_name" {
  type = string
}

variable "cloudflare_zone_id" {
  type = string
}

variable "region" {
  description = "Region AWS de deploiement."
  type        = string
  default     = "eu-west-3"
}

variable "environment" {
  description = "Etiquette d'environnement (dev/staging/prod), utilisee dans les tags et le nom des ressources."
  type        = string
  default     = "dev"
}

variable "cluster_name" {
  description = "Nom du cluster EKS et prefixe des ressources associees."
  type        = string
  default     = "atelier"
}

variable "kubernetes_version" {
  description = "Version Kubernetes du cluster EKS (\"<major>.<minor>\")."
  type        = string
  default     = "1.33"
}

variable "vpc_cidr" {
  description = "Bloc CIDR du VPC dedie au cluster."
  type        = string
  default     = "10.42.0.0/16"
}

variable "availability_zones" {
  description = "Zones de disponibilite utilisees pour les sous-reseaux publics/prives (2 minimum recommande pour la haute disponibilite du control plane EKS)."
  type        = list(string)
  default     = ["eu-west-3a", "eu-west-3b", "eu-west-3c"]
}

variable "single_nat_gateway" {
  description = "true : une seule NAT Gateway partagee (moins couteux, single point of failure reseau sortant). false : une NAT Gateway par AZ (haute disponibilite, cout multiplie)."
  type        = bool
  default     = true
}

# --- Securite reseau/audit --------------------------------------------

variable "admin_access_cidrs" {
  description = "CIDR autorises a atteindre l'API server EKS publique (voir eks.tf, public_access_cidrs). Aucune valeur par defaut volontairement : force a fournir explicitement l'IP admin plutot que d'heriter du 0.0.0.0/0 par defaut du module EKS. Trouver la sienne avec `curl -s https://checkip.amazonaws.com`."
  type        = list(string)
}

variable "cluster_log_retention_days" {
  description = "Retention (jours) du CloudWatch Log Group des logs d'audit du control plane EKS (voir eks.tf, cluster_enabled_log_types)."
  type        = number
  default     = 14
}

variable "flow_log_retention_days" {
  description = "Retention (jours) du CloudWatch Log Group des VPC Flow Logs (voir vpc.tf, trafic REJECT uniquement)."
  type        = number
  default     = 14
}

# --- Mode up/down ---------------------------------------------------------
#
# Les postes couteux (control plane EKS ~$0.10/h facture meme a 0 noeud,
# node group EC2, NAT Gateway ~$0.048/h) sont detruits/recrees entierement
# via ce booleen plutot que scales a 0 : mettre les noeuds a 0 laisserait
# quand meme tourner (et facturer) le control plane + la NAT Gateway. Voir
# README.md "Mode up/down" pour le detail des couts et la procedure.
#
# Consequence assumee : `terraform apply -var="enable_cluster=false"`
# detruit le cluster et tout ce qu'il contenait (pods, volumes EBS des
# PVC, donc les donnees PostgreSQL/Forgejo/OpenBao en cluster non
# sauvegardees ailleurs) - seuls le VPC (sans NAT), les buckets S3, le
# role IAM et la zone Route53 survivent. Prevu pour un compte de test/dev
# reconstruit a la demande, pas pour un environnement dont les donnees
# en cluster doivent survivre a un arret.
variable "enable_cluster" {
  description = "false : detruit le cluster EKS, son node group et la NAT Gateway (poste de cout dominant) en gardant VPC/S3/Route53/IAM. true (defaut) : provisionne tout normalement."
  type        = bool
  default     = true
}

# --- Node group -------------------------------------------------------
#
# Firecracker (crates/vm-supervisor) a besoin d'un acces reel a /dev/kvm
# sur le noeud pour chaque pod parent de Workshop (voir
# docs/admin-guide.md, section 1.1). Les instances EC2 standard ne
# l'exposent pas : seules les instances .metal (materiel nu) ou, depuis
# fevrier 2026, une liste fermee d'instances Nitro non-metal avec la
# virtualisation imbriquee explicitement activee au lancement
# (cpu_options.nested_virtualization = "enabled") le permettent. Liste
# verifiee aupres de la documentation AWS EC2 "Use nested virtualization"
# (2026-08-25) : C8i, M8i, R8i, C8id, R8id, M8id, C8i-flex, R8i-flex,
# M8i-flex, X8i, C7i, R7i, M7i, C7i-flex, M7i-flex, I7i.
variable "node_instance_type" {
  description = "Type d'instance EC2 du node group. Doit appartenir a la liste des familles Nitro supportant la virtualisation imbriquee (nested_virtualization), sans quoi /dev/kvm ne sera jamais expose et les Workshops resteront bloques en Pending."
  type        = string
  default     = "m7i.xlarge"

  validation {
    condition = contains(
      ["c8i", "m8i", "r8i", "c8id", "r8id", "m8id", "c8i-flex", "r8i-flex", "m8i-flex", "x8i", "c7i", "r7i", "m7i", "c7i-flex", "m7i-flex", "i7i"],
      split(".", var.node_instance_type)[0]
    )
    error_message = "node_instance_type doit etre une famille Nitro supportant nested_virtualization (c8i/m8i/r8i/c8id/r8id/m8id/*-flex/x8i/c7i/r7i/m7i/i7i), ou une instance .metal geree separement (voir README.md pour la variante bare-metal)."
  }
}

variable "node_desired_size" {
  description = "Nombre de noeuds souhaite dans le node group managed."
  type        = number
  default     = 2
}

variable "node_min_size" {
  description = "Nombre minimum de noeuds (autoscaling)."
  type        = number
  default     = 1
}

variable "node_max_size" {
  description = "Nombre maximum de noeuds (autoscaling)."
  type        = number
  default     = 4
}

# --- Pause automatique (filet de securite cout) ----------------------------

variable "auto_pause_enabled" {
  description = "Scale automatiquement le node group a 0 selon auto_pause_schedule (voir auto-pause.tf) - filet de securite si vous oubliez de le faire manuellement. N'affecte que le node group (palier \"pause\") : le control plane EKS/NAT Gateway continuent de tourner, aucune donnee en cluster n'est perdue. Pas de reprise automatique symetrique - un `terraform apply` reste necessaire pour remonter les noeuds."
  type        = bool
  default     = true
}

variable "auto_pause_schedule" {
  description = "Expression cron EventBridge Scheduler (6 champs, syntaxe AWS - voir auto_pause_timezone pour le fuseau)."
  type        = string
  default     = "cron(0 2 * * ? *)"
}

variable "auto_pause_timezone" {
  description = "Fuseau horaire (nom IANA) dans lequel auto_pause_schedule est interprete."
  type        = string
  default     = "Europe/Paris"
}

variable "node_disk_size_gb" {
  description = "Taille du volume racine EBS de chaque noeud (Go). Les images rootfs/kernel des Workshops (crates/image-builder) et les couches de conteneurs consomment davantage que la valeur par defaut EKS."
  type        = number
  default     = 100
}

# --- Base de donnees (Aurora PostgreSQL Serverless v2) --------------------

variable "db_engine_version" {
  description = "Version moteur Aurora PostgreSQL. >= 16.8/15.12 pour pgvector 0.8.0, >= 16.3/15.7 pour l'auto-pause a 0 ACU (voir database.tf) - 16.9 satisfait les deux."
  type        = string
  default     = "16.9"
}

variable "db_master_database" {
  description = "Nom de la premiere base creee avec le cluster (les 6 bases applicatives du chart sont ensuite creees par db-init-job, comme avec le PostgreSQL auto-heberge)."
  type        = string
  default     = "postgres"
}

variable "db_master_username" {
  description = "Utilisateur maitre Aurora - a reporter dans postgresql.auth.adminUser (values.yaml). Le mot de passe, lui, est genere par AWS Secrets Manager (manage_master_user_password), jamais definis ici."
  type        = string
  default     = "atelier_admin"
}

variable "db_min_acu" {
  description = "Capacite minimale (ACU) du cluster Aurora Serverless v2. 0 (defaut) active l'auto-pause : le cluster se met en pause tout seul apres db_auto_pause_seconds d'inactivite et ne facture plus que le stockage - independant du mode up/down du cluster EKS (eks.tf)."
  type        = number
  default     = 0
}

variable "db_max_acu" {
  description = "Capacite maximale (ACU) du cluster Aurora Serverless v2. 1 ACU ~= 2 Go de RAM."
  type        = number
  default     = 4
}

variable "db_auto_pause_seconds" {
  description = "Delai d'inactivite (secondes, 300-86400) avant mise en pause automatique quand db_min_acu = 0. Ignore si db_min_acu > 0."
  type        = number
  default     = 300
}

variable "db_backup_retention_days" {
  description = "Retention (jours) des sauvegardes automatiques Aurora. Independant du mode up/down (var.enable_cluster) : ce cluster n'est jamais detruit/recree par les paliers up/down/pause."
  type        = number
  default     = 7
}

variable "db_deletion_protection" {
  description = "true (defaut) : empeche un `terraform destroy`/replace accidentel de supprimer le cluster Aurora. A repasser a `false` explicitement (`-var`) avant une destruction volontaire, voir database.tf."
  type        = bool
  default     = true
}

variable "db_secret_rotation_days" {
  description = "Frequence (jours) de rotation automatique native RDS du secret admin Aurora (voir database.tf, aws_secretsmanager_secret_rotation)."
  type        = number
  default     = 90
}

# --- S3 -----------------------------------------------------------------

variable "s3_bucket_prefix" {
  description = "Prefixe applique aux 3 buckets S3 attendus par le chart Helm (s3Storage.buckets.*) pour garantir un nommage global unique. Les noms finaux sont <prefix>-sessions, <prefix>-snapshots, <prefix>-forgejo-lfs-attachments."
  type        = string
  default     = "atelier"
}

# --- Budget (garde-fou cout, complementaire a l'auto-pause) --------------
#
# Scope par tag (atelier.dev/cluster = var.cluster_name) plutot que sur le
# cout total du compte : un budget par cluster/environnement, pertinent
# si un jour live/prod/ partage le meme compte AWS que live/dev/. Alerte
# seulement (aws-sdk:budgets, pas d'action automatique) : complementaire
# au filet reactif d'auto-pause.tf, pas un remplacement.

variable "budget_enabled" {
  description = "Active un AWS Budget scope sur les ressources taguees atelier.dev/cluster=var.cluster_name, avec alerte email au-dela d'un seuil."
  type        = bool
  default     = true
}

variable "budget_limit_usd" {
  description = "Plafond mensuel (USD) du budget, sert de reference pour le seuil d'alerte (voir budget_alert_threshold_percent)."
  type        = number
  default     = 50
}

variable "budget_alert_threshold_percent" {
  description = "Pourcentage du budget declenchant l'alerte email (sur le cout reel, pas previsionnel)."
  type        = number
  default     = 80
}

variable "budget_alert_email" {
  description = "Adresse email notifiee au depassement du seuil. Requis si budget_enabled = true."
  type        = string
  default     = null

  validation {
    condition     = !var.budget_enabled || var.budget_alert_email != null
    error_message = "budget_alert_email est requis quand budget_enabled = true."
  }
}

# --- IRSA -----------------------------------------------------------------

variable "irsa_namespace" {
  description = "Namespace Kubernetes dans lequel le chart charts/atelier est installe (Release.Namespace) - restreint le champ d'application de la trust policy IAM du role IRSA."
  type        = string
  default     = "default"
}

# --- DNS ------------------------------------------------------------------
#
# Ce module ne cree AUCUNE ressource DNS (voir modules/dns) : domain_name
# n'est utilise ici que comme chaine pour construire la section `domains:`
# de helm_values_snippet, rien de plus. Le root (live/dev/) doit passer la
# meme valeur aux deux modules.
variable "domain_name" {
  description = "Domaine racine des 4 Ingress du chart (docs/admin-guide.md section 2 : auth./git./app./api.<domain_name>), utilise uniquement pour generer helm_values_snippet - doit correspondre a la valeur passee a modules/dns. Aucune valeur par defaut : nom de domaine reel, a fournir via live/<env>/terraform.tfvars (gitignore, jamais commite)."
  type        = string
}

# --- Images (ECR, voir modules/ecr) ----------------------------------------
#
# Ce module ne cree AUCUN depot ECR (voir modules/ecr) : ecr_registry n'est
# utilise ici que comme chaine pour construire helm_values_snippet (images
# des 5 Deployments geres par le chart + ATELIER_COMPONENT_IMAGE_REGISTRY
# pour les 5 images injectees directement par le controller, voir
# crates/controller/src/reconcile.rs). Le root doit passer
# module.ecr.registry ici - voir deploy/terraform/aws/mirror-images.sh pour
# alimenter effectivement ces depots avant le premier helm install.
variable "ecr_registry" {
  description = "Hote du registre ECR (<compte>.dkr.ecr.<region>.amazonaws.com, sans nom de depot - voir modules/ecr/outputs.tf, output \"registry\")."
  type        = string
}

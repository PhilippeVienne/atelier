# Aurora PostgreSQL Serverless v2 (engine_mode "provisioned" + un
# db.serverless, pas l'ancien "serverless" v1) plutot que le
# StatefulSet `pgvector/pgvector:pg16` embarque par le chart - branche via
# `postgresql.external.*` (charts/atelier/values.yaml), deja prevu pour ce
# cas exact (y compris `iamAuthEnabled`, non utilise ici : authentification
# par mot de passe via le secret gere ci-dessous, plus simple pour un
# premier branchement).
#
# pgvector : pleinement supporte (v0.8.0) a partir des versions Aurora
# PostgreSQL 13.20/14.17/15.12/16.8 (verifie aupres de la doc AWS
# "Announcing pgvector 0.8.0 support in Aurora PostgreSQL", 2026-08-25).
# La migration `services/pm-engine/migrations/20260824000000_init_pm_engine.sql`
# execute deja `CREATE EXTENSION IF NOT EXISTS vector` elle-meme - aucun
# changement applicatif necessaire, seule la version moteur ci-dessous
# compte.
#
# Auto-pause a 0 ACU (voir README.md "Base de donnees") : necessite au
# moins 13.15/14.12/15.7/16.3. var.db_engine_version (16.9 par defaut)
# satisfait les deux exigences a la fois.
#
# Contrairement au cluster EKS (mode up/down, eks.tf), ce cluster Aurora
# n'est PAS conditionne par var.enable_cluster : il survit aux cycles
# up/down du cluster Kubernetes (auto-pause gere deja son cout a l'inactivite,
# et detruire/recreer une base de donnees a chaque cycle serait destructif
# pour de vraies donnees).
resource "aws_db_subnet_group" "atelier" {
  name       = "${var.cluster_name}-${var.environment}"
  subnet_ids = module.vpc.private_subnets

  tags = {
    "atelier.dev/cluster" = var.cluster_name
  }
}

resource "aws_security_group" "atelier_db" {
  name        = "${var.cluster_name}-${var.environment}-db"
  description = "Acces PostgreSQL (5432) depuis le VPC du cluster Atelier"
  vpc_id      = module.vpc.vpc_id

  ingress {
    description = "PostgreSQL depuis le VPC (noeuds EKS)"
    from_port   = 5432
    to_port     = 5432
    protocol    = "tcp"
    cidr_blocks = [var.vpc_cidr]
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }

  tags = {
    "atelier.dev/cluster" = var.cluster_name
  }
}

resource "aws_rds_cluster" "atelier" {
  cluster_identifier = "${var.cluster_name}-${var.environment}"
  engine             = "aurora-postgresql"
  engine_version     = var.db_engine_version

  database_name   = var.db_master_database
  master_username = var.db_master_username
  # Mot de passe genere et gere par AWS Secrets Manager avec rotation
  # native - jamais ecrit dans le state ni dans un fichier de ce module.
  # Recupere via l'output sensible db_admin_password si besoin de le
  # coller dans values.yaml (postgresql.auth.adminPassword).
  manage_master_user_password = true

  db_subnet_group_name   = aws_db_subnet_group.atelier.name
  vpc_security_group_ids = [aws_security_group.atelier_db.id]

  storage_encrypted = true

  serverlessv2_scaling_configuration {
    min_capacity             = var.db_min_acu
    max_capacity             = var.db_max_acu
    seconds_until_auto_pause = var.db_min_acu == 0 ? var.db_auto_pause_seconds : null
  }

  # Compte de test : simplifie la destruction (pas de snapshot final a
  # gerer). A retirer (mettre `false` + `final_snapshot_identifier`) des
  # que ce cluster porte de vraies donnees a proteger.
  skip_final_snapshot = true
  apply_immediately   = true

  tags = {
    "atelier.dev/cluster" = var.cluster_name
  }
}

resource "aws_rds_cluster_instance" "atelier" {
  cluster_identifier = aws_rds_cluster.atelier.id
  engine             = aws_rds_cluster.atelier.engine
  engine_version     = aws_rds_cluster.atelier.engine_version
  instance_class     = "db.serverless"

  tags = {
    "atelier.dev/cluster" = var.cluster_name
  }
}

data "aws_secretsmanager_secret_version" "atelier_db_admin" {
  secret_id = aws_rds_cluster.atelier.master_user_secret[0].secret_arn
}

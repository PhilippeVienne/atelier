module "dns" {
  source = "../../modules/dns"

  domain_name        = var.domain_name
  cloudflare_zone_id = var.cloudflare_zone_id
  cluster_name       = var.cluster_name
}

module "ecr" {
  source = "../../modules/ecr"

  cluster_name = var.cluster_name
}

module "cluster" {
  source = "../../modules/cluster"

  region             = var.region
  environment        = var.environment
  cluster_name       = var.cluster_name
  kubernetes_version = var.kubernetes_version

  vpc_cidr           = var.vpc_cidr
  availability_zones = var.availability_zones
  single_nat_gateway = var.single_nat_gateway

  enable_cluster = var.enable_cluster

  node_instance_type = var.node_instance_type
  node_desired_size  = var.node_desired_size
  node_min_size      = var.node_min_size
  node_max_size      = var.node_max_size
  node_disk_size_gb  = var.node_disk_size_gb

  auto_pause_enabled  = var.auto_pause_enabled
  auto_pause_schedule = var.auto_pause_schedule
  auto_pause_timezone = var.auto_pause_timezone

  db_engine_version     = var.db_engine_version
  db_master_database    = var.db_master_database
  db_master_username    = var.db_master_username
  db_min_acu            = var.db_min_acu
  db_max_acu            = var.db_max_acu
  db_auto_pause_seconds = var.db_auto_pause_seconds

  s3_bucket_prefix = var.s3_bucket_prefix
  irsa_namespace   = var.irsa_namespace

  # module.dns.domain_name / module.ecr.registry plutot que des var.*
  # directement : memes valeurs, mais rend explicite que modules/cluster ne
  # fait que consommer des chaines produites ailleurs, sans dependance
  # fonctionnelle aux modules dns/ecr (voir modules/cluster/variables.tf).
  domain_name  = module.dns.domain_name
  ecr_registry = module.ecr.registry
}

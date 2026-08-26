# Sous-reseaux prives (noeuds EKS) + publics (NAT Gateway, futurs
# Load Balancers d'Ingress). Tags "kubernetes.io/..." requis par
# vpc-cni/l'auto-decouverte de subnets par les controleurs AWS
# (aws-load-balancer-controller, cluster-autoscaler) si ajoutes plus tard.
module "vpc" {
  source  = "terraform-aws-modules/vpc/aws"
  version = "~> 6.0"

  name = "${var.cluster_name}-${var.environment}"
  cidr = var.vpc_cidr

  azs             = var.availability_zones
  private_subnets = [for i, az in var.availability_zones : cidrsubnet(var.vpc_cidr, 4, i)]
  public_subnets  = [for i, az in var.availability_zones : cidrsubnet(var.vpc_cidr, 4, i + length(var.availability_zones))]

  # Geree hors module (voir nat-gateway.tf) : NAT Gateway en mode "regional"
  # (voir README.md "Reseau") plutot que le NAT zonal que ce module sait
  # creer nativement (pas encore expose par terraform-aws-modules/vpc a la
  # version utilisee ici).
  enable_nat_gateway   = false
  enable_dns_hostnames = true
  enable_dns_support   = true

  public_subnet_tags = {
    "kubernetes.io/role/elb"                    = "1"
    "kubernetes.io/cluster/${var.cluster_name}" = "shared"
  }
  private_subnet_tags = {
    "kubernetes.io/role/internal-elb"           = "1"
    "kubernetes.io/cluster/${var.cluster_name}" = "shared"
  }

  # REJECT seulement (pas ALL) : detecte les tentatives bloquees (scans,
  # regles de securite mal configurees) sans payer/stocker le volume du
  # trafic normal (mirroring d'images, S3, Aurora) qu'ACCEPT loggerait en
  # continu. Voir README.md "Securite".
  enable_flow_log                      = true
  flow_log_traffic_type                = "REJECT"
  flow_log_destination_type            = "cloud-watch-logs"
  create_flow_log_cloudwatch_log_group = true
  create_flow_log_cloudwatch_iam_role  = true
  flow_log_max_aggregation_interval    = 600
  flow_log_cloudwatch_log_group_retention_in_days = var.flow_log_retention_days

  tags = {
    "atelier.dev/cluster" = var.cluster_name
  }
}

# Sous-reseau prive de var.node_availability_zone (voir variables.tf) : le
# node group (eks.tf) y est exclusivement lance, contrairement au control
# plane/DB subnet group qui, eux, couvrent var.availability_zones en
# entier (contrainte AWS, pas un choix).
locals {
  node_subnet_ids = [module.vpc.private_subnets[index(var.availability_zones, var.node_availability_zone)]]
}

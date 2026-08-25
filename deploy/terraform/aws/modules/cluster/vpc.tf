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

  # Coupee en mode "down" (var.enable_cluster = false) : sans noeud EKS a
  # faire sortir vers Internet, la NAT Gateway ($0.048/h eu-west-3, facturee
  # meme inactive) ne sert plus a rien - voir variables.tf/README.md.
  enable_nat_gateway   = var.enable_cluster
  single_nat_gateway   = var.single_nat_gateway
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

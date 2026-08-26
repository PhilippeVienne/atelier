# NAT Gateway en mode "regional" (AWS, annonce du 19/11/2025) plutot que le
# mode "zonal" traditionnel expose par module.vpc (enable_nat_gateway=false
# dans vpc.tf) : une seule ressource qui s'etend/se retracte automatiquement
# sur les AZ ou des ENI sont presents, sans avoir besoin d'un subnet public
# dedie ni de gerer une route table par AZ. Elimine le point de defaillance
# unique du mode "single_nat_gateway" (toutes les AZ dependaient d'une seule
# AZ zonale) tout en restant a une seule ressource facturee (~$35/mois,
# meme ordre de grandeur qu'une NAT zonale unique - voir README.md
# "Estimation de couts").
#
# Coupee en mode "down" (var.enable_cluster = false), meme raisonnement que
# l'ancienne NAT zonale : sans noeud EKS a faire sortir vers Internet, elle
# ne sert plus a rien.
resource "aws_nat_gateway" "regional" {
  count = var.enable_cluster ? 1 : 0

  vpc_id            = module.vpc.vpc_id
  connectivity_type = "public"
  availability_mode = "regional"

  tags = {
    Name                   = "${var.cluster_name}-${var.environment}"
    "atelier.dev/cluster"  = var.cluster_name
  }
}

# Route 0.0.0.0/0 -> NAT regionale sur CHAQUE route table privee geree par
# module.vpc (une par AZ de var.availability_zones, meme si un seul node
# group EC2 n'en utilise reellement qu'une - voir var.node_availability_zone) :
# la NAT regionale n'a pas besoin d'une route par AZ distincte (un seul ID
# de passerelle, voir README.md "Reseau"), mais chaque route table doit
# tout de meme pointer dessus explicitement.
resource "aws_route" "private_nat_gateway_regional" {
  count = var.enable_cluster ? length(module.vpc.private_route_table_ids) : 0

  route_table_id          = module.vpc.private_route_table_ids[count.index]
  destination_cidr_block  = "0.0.0.0/0"
  nat_gateway_id          = aws_nat_gateway.regional[0].id
}

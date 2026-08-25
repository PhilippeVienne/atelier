# Reexporte tel quel chaque output du module - voir
# ../../modules/cluster/outputs.tf pour la description detaillee de
# chacun.

output "enabled" {
  value = module.cluster.enabled
}

output "cluster_name" {
  value = module.cluster.cluster_name
}

output "auto_pause_schedule_arn" {
  value = module.cluster.auto_pause_schedule_arn
}

output "region" {
  value = module.cluster.region
}

output "cluster_endpoint" {
  value = module.cluster.cluster_endpoint
}

output "configure_kubectl" {
  value = module.cluster.configure_kubectl
}

output "irsa_role_arn" {
  value = module.cluster.irsa_role_arn
}

output "s3_buckets" {
  value = module.cluster.s3_buckets
}

output "s3_endpoint" {
  value = module.cluster.s3_endpoint
}

output "route53_zone_id" {
  value = module.dns.zone_id
}

output "route53_name_servers" {
  value = module.dns.name_servers
}

output "db_endpoint" {
  value = module.cluster.db_endpoint
}

output "db_reader_endpoint" {
  value = module.cluster.db_reader_endpoint
}

output "db_admin_password" {
  value     = module.cluster.db_admin_password
  sensitive = true
}

output "helm_values_snippet" {
  value     = module.cluster.helm_values_snippet
  sensitive = true
}

output "ecr_registry" {
  value = module.ecr.registry
}

output "ecr_repository_urls" {
  value = module.ecr.repository_urls
}

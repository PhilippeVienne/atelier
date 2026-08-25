output "enabled" {
  description = "Reflet de var.enable_cluster - false signifie que le cluster/node group/NAT Gateway sont actuellement detruits (mode down, voir README.md)."
  value       = var.enable_cluster
}

output "cluster_name" {
  description = "Nom du cluster EKS (null en mode down)."
  value       = try(module.eks[0].cluster_name, null)
}

output "auto_pause_schedule_arn" {
  description = "ARN du schedule EventBridge de pause automatique du node group (null si auto_pause_enabled=false ou mode down)."
  value       = try(aws_scheduler_schedule.auto_pause[0].arn, null)
}

output "region" {
  description = "Region AWS du cluster."
  value       = var.region
}

output "cluster_endpoint" {
  description = "Endpoint de l'API server EKS (null en mode down)."
  value       = try(module.eks[0].cluster_endpoint, null)
}

output "configure_kubectl" {
  description = "Commande pour configurer kubectl/helm contre ce cluster (null en mode down)."
  value       = var.enable_cluster ? "aws eks update-kubeconfig --region ${var.region} --name ${module.eks[0].cluster_name}" : null
}

output "irsa_role_arn" {
  description = "ARN du role IRSA a coller dans values.yaml : cloudIdentity.provider=\"aws\" / cloudIdentity.annotations.\"eks.amazonaws.com/role-arn\" (null en mode down - le role est recree a l'identique, meme ARN, au prochain \"up\")."
  value       = try(aws_iam_role.atelier[0].arn, null)
}

output "s3_buckets" {
  description = "Noms des buckets crees, a reporter dans s3Storage.buckets.* (values.yaml)."
  value = {
    sessions  = aws_s3_bucket.this["sessions"].id
    snapshots = aws_s3_bucket.this["snapshots"].id
    forgejo   = aws_s3_bucket.this["forgejo"].id
  }
}

output "s3_endpoint" {
  description = "Endpoint a renseigner dans s3Storage.external.endpoint."
  value       = "https://s3.${var.region}.amazonaws.com"
}

output "db_endpoint" {
  description = "Endpoint d'ecriture du cluster Aurora, a reporter dans postgresql.external.host (values.yaml)."
  value       = aws_rds_cluster.atelier.endpoint
}

output "db_reader_endpoint" {
  description = "Endpoint de lecture du cluster Aurora (non utilise par le chart actuellement, expose pour reference)."
  value       = aws_rds_cluster.atelier.reader_endpoint
}

output "db_admin_password" {
  description = "Mot de passe genere par Secrets Manager pour db_master_username, a coller dans postgresql.auth.adminPassword (values.yaml). Sensible : n'apparait jamais dans les logs `terraform apply`/`plan`, uniquement via `terraform output -raw db_admin_password`."
  value       = jsondecode(data.aws_secretsmanager_secret_version.atelier_db_admin.secret_string)["password"]
  sensitive   = true
}

output "helm_values_snippet" {
  description = "Extrait values.yaml pret a fusionner (helm install -f) une fois ce module applique. Non pertinent en mode down (pas de cluster ou l'installer). Sensible (contient le mot de passe Aurora) : recuperer via `terraform output -raw helm_values_snippet`."
  sensitive   = true
  value       = <<-EOT
    domains:
      keycloak: "auth.${var.domain_name}"
      forgejo: "git.${var.domain_name}"
      dashboard: "app.${var.domain_name}"
      apiServer: "api.${var.domain_name}"

    cloudIdentity:
      provider: "aws"
      annotations:
        eks.amazonaws.com/role-arn: "${try(aws_iam_role.atelier[0].arn, "")}"

    # ALB Controller + ACM + external-dns (installes via Helm hors Terraform,
    # voir README.md "Ingress") plutot que ingress-nginx/cert-manager : cree
    # un seul ALB partage pour les 4 sous-domaines (group.name), TLS termine
    # par le certificat ACM deja valide par modules/dns (pas de secret k8s a
    # gerer), enregistrements Route53 crees automatiquement par external-dns
    # a partir du champ "host" de chaque Ingress (pas d'annotation
    # external-dns.alpha.kubernetes.io/hostname necessaire).
    tls:
      enabled: false
    ingress:
      className: "alb"
      annotations:
        alb.ingress.kubernetes.io/scheme: "internet-facing"
        alb.ingress.kubernetes.io/target-type: "ip"
        alb.ingress.kubernetes.io/certificate-arn: "${var.acm_certificate_arn}"
        alb.ingress.kubernetes.io/group.name: "${var.cluster_name}"
        alb.ingress.kubernetes.io/listen-ports: '[{"HTTP": 80}, {"HTTPS": 443}]'
        alb.ingress.kubernetes.io/ssl-redirect: "443"

    postgresql:
      # true est requis meme en mode externe : ce flag conditionne aussi
      # db-init-job/db-migrate-job/keycloak/forgejo/litellm, pas seulement
      # le StatefulSet (lui court-circuite par external.enabled ci-dessous,
      # voir charts/atelier/templates/infra/postgresql-statefulset.yaml).
      enabled: true
      auth:
        adminUser: "${var.db_master_username}"
        adminPassword: "${jsondecode(data.aws_secretsmanager_secret_version.atelier_db_admin.secret_string)["password"]}"
      external:
        enabled: true
        host: "${aws_rds_cluster.atelier.endpoint}"
        port: ${aws_rds_cluster.atelier.port}
        sslMode: "require"
        iamAuthEnabled: false

    s3Storage:
      rustfs:
        enabled: false
      external:
        enabled: true
        endpoint: "https://s3.${var.region}.amazonaws.com"
        region: "${var.region}"
        forcePathStyle: false
      buckets:
        sessions: "${aws_s3_bucket.this["sessions"].id}"
        snapshots: "${aws_s3_bucket.this["snapshots"].id}"
        forgejo: "${aws_s3_bucket.this["forgejo"].id}"

    # Images depuis ECR (voir modules/ecr) plutot que Docker Hub/quay.io/
    # ghcr.io/codeberg.org - a alimenter au prealable via
    # deploy/terraform/aws/mirror-images.sh, sans quoi ces pulls echouent.
    # Tags INCHANGES (memes versions que charts/atelier/values.yaml) : seul
    # l'hote du registre change.
    controller:
      image:
        repository: "${var.ecr_registry}/atelier-controller"
      env:
        # Images injectees directement par le controller dans les pods
        # Workshop (net-proxy/identity-proxy/vm-supervisor/mcp-gateway,
        # Job image-builder) - PAS gerees par ce chart, voir
        # crates/controller/src/reconcile.rs. Hote seul (pas de sous-chemin
        # "/atelier" : les depots ECR portent deja le prefixe "atelier-"
        # dans leur nom, voir modules/ecr/variables.tf).
        ATELIER_COMPONENT_IMAGE_REGISTRY: "${var.ecr_registry}"
    apiServer:
      image:
        repository: "${var.ecr_registry}/atelier-api-server"
    dashboard:
      image:
        repository: "${var.ecr_registry}/atelier-dashboard"
    pmEngine:
      image:
        repository: "${var.ecr_registry}/atelier-pm-engine"
    kvmDevicePlugin:
      image:
        repository: "${var.ecr_registry}/atelier-kvm-device-plugin"
    keycloak:
      image:
        repository: "${var.ecr_registry}/mirror/keycloak"
    forgejo:
      image:
        repository: "${var.ecr_registry}/mirror/forgejo"
    openbao:
      image:
        repository: "${var.ecr_registry}/mirror/openbao"
    litellm:
      image:
        repository: "${var.ecr_registry}/mirror/litellm"
    redis:
      image:
        repository: "${var.ecr_registry}/mirror/redis"
    registry:
      image:
        repository: "${var.ecr_registry}/mirror/registry"
    initJobs:
      dbInit:
        # Sans ce flag, db-init-job ne tourne JAMAIS quand
        # postgresql.external.enabled est vrai (garde-fou du chart contre
        # une base externe partagee/non possedee par cette automatisation -
        # voir charts/atelier/values.yaml). Ce cluster Aurora est cree ET
        # possede entierement par ce module Terraform, jamais partage :
        # l'activer ici est sans risque et necessaire, sans quoi les 6
        # bases applicatives ne seraient jamais creees.
        runAgainstExternal: true
        image:
          repository: "${var.ecr_registry}/mirror/postgres"
      keycloakInit:
        image:
          repository: "${var.ecr_registry}/mirror/keycloak"
      openbaoInit:
        # Desactive pour le premier `helm install`/upgrade uniquement :
        # OpenBao demarre scelle (docs/admin-guide.md section 7.2) et
        # l'init/unseal est deliberement manuel, jamais automatise dans un
        # hook - ce Job echoue donc systematiquement tant que
        # openbao.rootTokenSecretName ne pointe pas vers un Secret cree a la
        # main apres l'unseal. Repasser a `true` (ou retirer cette ligne)
        # dans un `helm upgrade` ulterieur une fois ce Secret cree.
        enabled: false
        image:
          repository: "${var.ecr_registry}/mirror/openbao"
      s3Init:
        # s3-init-job authentifie toujours `mc` via le secret
        # cloudIdentity.fallbackSecretName, y compris quand cloudIdentity.provider
        # = "aws" (IRSA ci-dessus) - ce secret n'est cense exister qu'en mode
        # provider "none" (voir charts/atelier/values.yaml), et `mc` ne sait de
        # toute facon pas s'authentifier via IRSA (necessite une cle d'acces/
        # secrete explicite). Desactive : les 3 buckets sont deja crees par ce
        # module (s3.tf), le job serait de toute facon un no-op idempotent.
        enabled: false
        image:
          repository: "${var.ecr_registry}/mirror/minio-mc"
  EOT
}

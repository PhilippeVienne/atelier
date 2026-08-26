# Mode up/down (var.enable_cluster, voir variables.tf) : le cluster entier
# (control plane + node group, poste de cout dominant) est detruit/recree
# via `count` plutot que scale a 0, qui laisserait le control plane EKS
# ($0.10/h, facture meme sans noeud) continuer a tourner.
module "eks" {
  count = var.enable_cluster ? 1 : 0

  source  = "terraform-aws-modules/eks/aws"
  version = "~> 21.0"

  name               = var.cluster_name
  kubernetes_version = var.kubernetes_version

  vpc_id     = module.vpc.vpc_id
  subnet_ids = module.vpc.private_subnets

  # Restreint aux CIDR admin (voir var.admin_access_cidrs) plutot que
  # 0.0.0.0/0 (defaut du module si non precise) : l'API server reste
  # joignable depuis Internet (necessaire pour `helm install` depuis un
  # poste hors VPC, voir README.md), mais seulement depuis les IP
  # explicitement autorisees.
  endpoint_public_access        = true
  endpoint_private_access       = true
  endpoint_public_access_cidrs  = var.admin_access_cidrs

  # Audit de l'API server (voir README.md "Securite") : `audit` est le plus
  # important (qui a fait quoi), les autres aident au diagnostic. Cree
  # automatiquement le CloudWatch Log Group associe.
  enabled_log_types                      = ["api", "audit", "authenticator", "controllerManager", "scheduler"]
  cloudwatch_log_group_retention_in_days = var.cluster_log_retention_days

  # Le compte/role Terraform obtient l'acces admin via un Access Entry EKS
  # natif (remplace l'ancienne aws-auth ConfigMap) - necessaire pour pouvoir
  # ensuite executer `helm install charts/atelier` (etape manuelle separee,
  # voir README.md) depuis le meme poste.
  enable_cluster_creator_admin_permissions = true

  # IRSA : cree le fournisseur OIDC du cluster, consomme par iam.tf pour la
  # trust policy du role unique `cloudIdentity.annotations` (voir
  # docs/admin-guide.md section 4.1 - ce chart applique le meme role a tous
  # les ServiceAccounts qu'il cree, pas un role distinct par composant).
  enable_irsa = true

  # `before_compute = true` sur vpc-cni/kube-proxy : sans ca, le module cree
  # ces addons via `aws_eks_addon.this`, qui porte un `depends_on` implicite
  # sur le node group (voir source du module) - deadlock reel rencontre en
  # test (2026-08-25) : le node group n'atteint jamais `ACTIVE` (attend que
  # ses noeuds passent Ready) tant que vpc-cni n'est pas installe, mais
  # vpc-cni n'est cree qu'apres le node group. `before_compute` route ces
  # deux addons via `aws_eks_addon.before_compute`, sans cette dependance :
  # ils sont prets avant meme que les instances EC2 ne demarrent. `coredns`
  # reste un addon normal (ses pods ont reellement besoin d'un noeud pour
  # etre planifies).
  addons = {
    coredns = {}
    kube-proxy = {
      before_compute = true
    }
    vpc-cni = {
      before_compute = true
    }
    eks-pod-identity-agent = {}
    # Sans cet addon, la StorageClass "gp2" par defaut du cluster (provisioner
    # in-tree "kubernetes.io/aws-ebs") ne fonctionne pas : depuis la migration
    # CSI (EKS 1.23+), ces requetes sont interceptees et routees vers le
    # driver CSI EBS, absent si non installe explicitement - constate
    # empiriquement (PVC openbao/redis/forgejo bloques "Pending", evenement
    # "pod has unbound immediate PersistentVolumeClaims") lors du premier
    # `helm install` sur ce cluster. Pod Identity (pas IRSA) : plus simple, le
    # role n'a pas besoin de connaitre l'URL OIDC du cluster.
    aws-ebs-csi-driver = {
      pod_identity_association = [{
        role_arn        = aws_iam_role.ebs_csi_driver[0].arn
        service_account = "ebs-csi-controller-sa"
      }]
    }
  }

  eks_managed_node_groups = {
    (var.cluster_name) = {
      instance_types = [var.node_instance_type]
      ami_type       = "AL2023_x86_64_STANDARD"

      # Une seule AZ (voir var.node_availability_zone/vpc.tf local.node_subnet_ids),
      # different de var.subnet_ids par defaut du module (toutes les AZ du
      # cluster) : affinite des volumes EBS des composants avec etat, voir
      # variables.tf pour le detail du compromis.
      subnet_ids = local.node_subnet_ids

      min_size     = var.node_min_size
      max_size     = var.node_max_size
      desired_size = var.node_desired_size

      block_device_mappings = {
        root = {
          device_name = "/dev/xvda"
          ebs = {
            volume_size = var.node_disk_size_gb
            volume_type = "gp3"
            # Cle geree AWS (aws/ebs) par defaut, pas de CMK dediee : voir
            # README.md "Securite" pour le compromis (pas d'exigence
            # conformite type SOC2 sur ce compte de test actuellement).
            encrypted = true
          }
        }
      }

      # Active la virtualisation imbriquee au niveau de l'hyperviseur Nitro
      # (L0) - condition necessaire mais pas suffisante : le noyau du
      # noeud doit encore charger le module kvm_intel pour que /dev/kvm
      # apparaisse (voir cloudinit_pre_nodeadm ci-dessous). Non verifie
      # empiriquement contre un vrai cluster EKS dans cette session (aucun
      # compte AWS disponible) - a valider avec `ls -la /dev/kvm` sur un
      # noeud reel avant de compter dessus en production.
      cpu_options = {
        nested_virtualization = "enabled"
      }

      cloudinit_pre_nodeadm = [
        {
          content_type = "text/x-shellscript"
          content      = <<-EOT
            #!/bin/bash
            set -euo pipefail
            modprobe kvm_intel
            echo kvm_intel > /etc/modules-load.d/kvm.conf
            cat <<'UDEV' > /etc/udev/rules.d/60-atelier-kvm.rules
            SUBSYSTEM=="misc", KERNEL=="kvm", GROUP="kvm", MODE="0666"
            UDEV
            udevadm control --reload-rules
            udevadm trigger --name-match=kvm || true
          EOT
        }
      ]

      labels = {
        "atelier.dev/kvm" = "true"
      }
    }
  }

  tags = {
    "atelier.dev/cluster" = var.cluster_name
  }
}

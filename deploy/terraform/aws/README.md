# Module Terraform AWS — infrastructure du cluster Atelier

Provisionne l'infrastructure AWS necessaire a un deploiement de
`charts/atelier` (voir `docs/admin-guide.md`) : VPC, cluster EKS avec un
node group compatible KVM (Firecracker), role IAM IRSA, les 3 buckets S3
attendus par le chart, un cluster Aurora PostgreSQL Serverless v2
(`modules/cluster/database.tf`) a la place du StatefulSet embarque,
(module separe, `modules/dns/`) une zone Route53 pour `var.domain_name`
avec sa delegation NS automatique cote Cloudflare, et (module separe,
`modules/ecr/`) un depot ECR par image consommee par le chart/le
controller - toutes tirees a l'origine de registres publics
(Docker Hub/quay.io/ghcr.io/codeberg.org), alimentes ensuite par
`mirror-images.sh`. **N'installe pas le chart Helm lui-meme** — c'est une
etape manuelle separee, une fois ce module applique (voir "Etape 6"
ci-dessous).

> Pas de certificats ACM ici — le chart documente `tls.certManager`
> (Let's Encrypt via cert-manager) comme solution independante du cloud,
> conservee telle quelle.
>
> **Non applique/verifie contre un vrai compte AWS** au moment de l'ecriture
> de ce module (aucun credential AWS disponible dans l'environnement
> l'ayant genere). Les noms de variables des modules communautaires
> utilises (`terraform-aws-modules/vpc`, `terraform-aws-modules/eks`) ont
> ete verifies contre leur source au 2026-08-25, mais un `terraform plan`
> attentif avant le premier `apply` reste indispensable.

## Structure

```text
deploy/terraform/aws/
├── bootstrap.sh         script bash (AWS CLI) : backend d'etat (bucket
│                        S3, verrouillage natif use_lockfile, pas de
│                        DynamoDB), pas de Terraform ici (voir Etape 1)
├── mirror-images.sh     script bash (crane) : alimente les depots ECR
│                        crees par modules/ecr (voir Etape 4)
├── modules/
│   ├── cluster/         module reutilisable : VPC/EKS/IAM/S3/Aurora
│   ├── dns/             module reutilisable : zone Route53 + delegation
│   │                    Cloudflare - cycle de vie independant du cluster
│   │                    (voir "Base de donnees"/"Trois paliers" : ce
│   │                    module n'est jamais detruit par enable_cluster)
│   └── ecr/             module reutilisable : un depot par image - meme
│                        cycle de vie independant, voir modules/dns/
└── live/dev/            root Terraform reel : configure providers +
                         backend, appelle modules/cluster, modules/dns ET
                         modules/ecr (toutes les commandes `terraform
                         init/plan/apply` ci-dessous s'executent depuis ce
                         repertoire). terraform.tfvars et backend.hcl y
                         sont generes localement (gitignores, jamais
                         commites - voir terraform.tfvars.example pour le
                         format attendu)
```

Aucun des trois modules ne contient de bloc `provider{}`/`backend{}` -
uniquement `live/dev/versions.tf`. Ajouter un second environnement
(`live/prod/`, autre compte/region) ne demande de dupliquer que
`live/dev/` (5 petits fichiers) — jamais `modules/`.

## Prerequis

- Terraform >= 1.11, AWS CLI configure (`aws sts get-caller-identity` doit
  repondre) avec des droits suffisants (VPC, EKS, IAM, S3, ECR).
- Le compte/role utilise pour `terraform apply` doit pouvoir gerer des
  quotas EC2 suffisants pour le type d'instance choisi (voir
  `node_instance_type`).
- Un jeton d'API Cloudflare avec le scope minimal `Zone:DNS:Edit` sur la
  zone parente (`exemple.com`), expose **uniquement** via variable
  d'environnement, jamais dans un fichier de ce repertoire :
  ```bash
  export CLOUDFLARE_API_TOKEN="..."
  ```
  Le provider `cloudflare` (`live/dev/versions.tf`) le lit automatiquement. Si ce
  jeton a ete partage/colle en clair quelque part (chat, log), le
  regenerer par prudence une fois ce module applique.

## 1. Bootstrap du backend d'etat (une seule fois par compte/environnement)

Script bash (AWS CLI), pas de Terraform : evite le probleme de l'oeuf et
de la poule (un Terraform ne peut pas gerer le bucket qui hebergera son
propre state). Depuis `deploy/terraform/aws/` (pas `live/dev/`) :

```bash
./bootstrap.sh eu-west-3 dev
```

Idempotent (ne recree rien si deja present). Nom de bucket derive de l'ID
de compte AWS courant (`tf-state-<account-id>-atelier`, via `aws sts
get-caller-identity`) — pas de suffixe a choisir/documenter a part. Ecrit
`live/dev/backend.hcl` (gitignore, contient un nom de bucket reel)
et, s'il n'existe pas encore, `live/dev/terraform.tfvars` (copie de
`terraform.tfvars.example`, egalement gitignore).

## 2. Configuration

```bash
cd live/dev
$EDITOR terraform.tfvars
# domain_name/cloudflare_zone_id (obligatoires, aucune valeur par defaut
# dans le code - voir modules/dns/variables.tf), region, cluster_name,
# irsa_namespace, s3_bucket_prefix (doit etre globalement unique sur AWS),
# node_instance_type...
```

**`node_instance_type`** — Firecracker exige `/dev/kvm`. Les instances EC2
standard ne l'exposent pas. Deux options :

- Une famille Nitro supportant la virtualisation imbriquee (valeur par
  defaut `m7i.xlarge`) — liste complete et a jour dans le commentaire de
  `modules/cluster/variables.tf`. Le module (`modules/cluster/eks.tf`)
  active `cpu_options.nested_virtualization = "enabled"` et charge le
  module noyau `kvm_intel` via `cloudinit_pre_nodeadm`.
- Une instance `.metal` (ex: `m5.metal`) — bare-metal, KVM natif sans
  configuration supplementaire, mais `cpu_options.nested_virtualization`
  n'a pas de sens dessus : retirer ce bloc dans `modules/cluster/eks.tf`
  si vous basculez sur `.metal` (la validation de `node_instance_type`
  refusera sinon la valeur, `.metal` n'etant pas dans la liste des
  familles nested-virt).

Dans les deux cas, **verifier apres le premier `apply`** :

```bash
kubectl debug node/<nom-du-noeud> -it --image=busybox -- ls -la /dev/kvm
```

## 3. Application

```bash
terraform init -backend-config=backend.hcl
terraform plan
terraform apply
```

## 4. Alimenter les depots ECR (images)

`modules/ecr` cree les depots (vides) — encore aucune image dedans a ce
stade. `mirror-images.sh` (racine `deploy/terraform/aws/`) les alimente
par copie registre-a-registre (`crane copy`, sans docker local pour cette
partie) :

```bash
cd ..   # retour a deploy/terraform/aws/ si vous etiez dans live/dev
./mirror-images.sh eu-west-3
```

Deux sources, aucun build necessaire ici : les dependances tierces
(Postgres/Keycloak/Forgejo/OpenBao/LiteLLM/Redis/`mc`) depuis leur
registre public d'origine, et les 10 images de composants Atelier depuis
`ghcr.io/philippevienne/atelier-*` — deja publiees a chaque push sur
`main` par `.github/workflows/docker-ghcr.yml`, pas de rebuild local. Les
5 images injectees directement par le controller dans les pods Workshop
(net-proxy/identity-proxy/vm-supervisor/mcp-gateway/image-builder — voir
`ATELIER_COMPONENT_IMAGE_REGISTRY`, `crates/controller/src/reconcile.rs`)
sont re-taguees `:dev` au moment de la copie : c'est le tag fixe que le
controller demande, quel que soit le tag publie sur ghcr.io.

A relancer apres chaque mise a jour voulue des images (ce script ne suit
aucune version automatiquement, il recopie `:latest`/les tags fixes
ci-dessus a l'instant T).

## 5. Delegation DNS cote Cloudflare

`var.domain_name` (ex: `atelier.exemple.com`) est un sous-domaine de
`exemple.com`, dont la zone racine reste geree dans Cloudflare
(`var.cloudflare_zone_id`). `modules/dns/cloudflare.tf` cree
automatiquement les 4 enregistrements NS de delegation (un par serveur de
noms Route53, mode "DNS only" — un enregistrement NS ne peut pas etre
proxy) des le premier `terraform apply` : rien a faire a la main.
Verifier la propagation :

```bash
terraform output route53_name_servers   # ce que Cloudflare doit servir
dig NS atelier.exemple.com                # ce qui est reellement propage
```

Route53 ne contient pour l'instant que la zone elle-meme (aucun
enregistrement A/ALIAS) : les 4 sous-domaines (`auth.`/`git.`/`app.`/
`api.atelier.exemple.com`, voir `helm_values_snippet` ci-dessous) ne
resoudront qu'une fois l'Ingress Controller installe (etape 6) et son
adresse connue — a ajouter alors dans la zone (`terraform output
route53_zone_id`), soit manuellement, soit via `external-dns` en cluster
(hors perimetre de ce module).

## 6. Brancher le chart Helm sur cette infrastructure

```bash
terraform output configure_kubectl   # puis executer la commande affichee
terraform output -raw irsa_role_arn
terraform output -raw helm_values_snippet > aws-values.yaml
helm upgrade --install atelier ../../../../../charts/atelier \
  --namespace <irsa_namespace> --create-namespace \
  -f aws-values.yaml
```

`irsa_namespace` (variable de ce module) doit correspondre exactement au
`--namespace` utilise ici : la trust policy du role IAM (`modules/cluster/iam.tf`) restreint
`sts:AssumeRoleWithWebIdentity` aux ServiceAccounts de ce seul namespace.

## 7. Ingress : ALB Controller + external-dns

`modules/cluster/alb-controller.tf`/`modules/dns/acm.tf` ne creent que les
roles IAM (Pod Identity) et le certificat ACM - les deux controllers
eux-memes s'installent via Helm classique (pas de provider Helm/Kubernetes
dans ce module Terraform, voir choix d'architecture en tete de ce fichier),
une seule fois par cluster (survit aux paliers pause/down/up, a reinstaller
apres un `enable_cluster=false` -> `true` complet) :

```bash
helm repo add eks https://aws.github.io/eks-charts
helm repo add external-dns https://kubernetes-sigs.github.io/external-dns/
helm repo update eks external-dns

VPC_ID=$(aws eks describe-cluster --name <cluster_name> --region <region> \
  --query 'cluster.resourcesVpcConfig.vpcId' --output text)

helm install aws-load-balancer-controller eks/aws-load-balancer-controller \
  -n kube-system \
  --set clusterName=<cluster_name> \
  --set serviceAccount.create=true \
  --set serviceAccount.name=aws-load-balancer-controller \
  --set region=<region> \
  --set vpcId="$VPC_ID"

helm install external-dns external-dns/external-dns \
  -n kube-system \
  --set serviceAccount.create=true \
  --set serviceAccount.name=external-dns \
  --set provider.name=aws \
  --set 'env[0].name=AWS_DEFAULT_REGION' \
  --set 'env[0].value=<region>' \
  --set txtOwnerId=<cluster_name>-<environment> \
  --set 'domainFilters[0]=<domain_name>' \
  --set policy=sync
```

`helm_values_snippet` (voir `outputs.tf`) pose deja `ingress.className: "alb"`
et les annotations `alb.ingress.kubernetes.io/*` necessaires (un seul ALB
partage entre les 4 Ingress via `group.name`, certificat ACM, redirection
HTTP->HTTPS) - `external-dns` cree ensuite automatiquement les
enregistrements Route53 a partir du champ `host` de chaque Ingress, aucune
action manuelle supplementaire.

**Piege PVC/AZ** : un volume EBS est zonal (cree dans l'AZ du noeud qui l'a
demande la premiere fois). Si le node group est renouvele (upgrade de
version, remplacement d'instance) et qu'aucun noeud restant ne se trouve
dans cette AZ, le pod reste `Pending` ("didn't match PersistentVolume's node
affinity") - constate empiriquement sur `atelier-forgejo` apres l'upgrade
1.33->1.34 (2 noeuds sur 3 AZ possibles). Pour un volume sans donnees a
proteger, `kubectl delete pvc <nom>` (puis `helm upgrade` pour recreer le
Deployment) suffit ; pour un volume avec donnees reelles, il faudrait migrer
les donnees vers un nouveau volume dans la bonne AZ avant de supprimer
l'ancien.

## Base de donnees : Aurora PostgreSQL Serverless v2

`modules/cluster/database.tf` remplace le StatefulSet `pgvector/pgvector:pg16` embarque
par le chart par un cluster Aurora Serverless v2, branche via
`postgresql.external.*` (deja prevu par `charts/atelier/values.yaml`,
y compris `iamAuthEnabled` — non utilise ici, authentification par mot de
passe pour un premier branchement). `postgresql.enabled` doit rester
`true` meme en mode externe (voir commentaire dans `modules/cluster/outputs.tf`) : ce
booleen conditionne aussi `db-init-job`/`db-migrate-job`/Keycloak/Forgejo/
LiteLLM, pas seulement le StatefulSet, qui lui est court-circuite par
`external.enabled`.

**`initJobs.dbInit.runAgainstExternal: true`** (dans `helm_values_snippet`,
egalement necessaire) : par defaut, le chart **desactive entierement**
`db-init-job` des que `postgresql.external.enabled` est vrai — garde-fou
contre un `CREATE DATABASE` non sollicite sur une base externe
potentiellement partagee/geree ailleurs (bug trouve en verifiant cette
liste d'images : la premiere version de ce module ne positionnait pas ce
flag, les 6 bases applicatives n'auraient donc jamais ete creees sur
Aurora). Sans risque ici puisque ce cluster Aurora est cree ET possede
entierement par ce module, jamais partage. Une fois ce flag pose, les 6
bases (`atelier_apiserver`, `atelier_controller`, `atelier_pm`, ...) sont
creees exactement comme avec le PostgreSQL auto-heberge, seul l'hote
change (voir `charts/atelier/templates/jobs/db-init-job.yaml`, verifie via
`helm template` avec/sans le flag).

**pgvector** : pleinement supporte (extension `vector` 0.8.0) a partir des
versions Aurora PostgreSQL 13.20/14.17/15.12/16.8+ (`var.db_engine_version`,
defaut `16.9`). La migration
`services/pm-engine/migrations/20260824000000_init_pm_engine.sql` execute
deja `CREATE EXTENSION IF NOT EXISTS vector` elle-meme — **aucun
changement applicatif necessaire**, l'index `ivfflat` existant fonctionne
a l'identique sur Aurora.

**Auto-pause a 0 ACU** (`var.db_min_acu = 0` par defaut) : le cluster se
met en pause tout seul apres `var.db_auto_pause_seconds` (300s par defaut)
sans connexion active, et ne facture plus alors que le stockage. C'est
**independant du mode up/down du cluster EKS** (`var.enable_cluster`) —
contrairement a celui-ci, ce cluster Aurora n'est jamais detruit par ce
module : il persiste (et gere lui-meme son cout a l'inactivite) meme
pendant un "down" complet du cluster Kubernetes. Consequence notable :
passer en mode "down" ne fait donc plus perdre les donnees PostgreSQL, a
la difference du StatefulSet en cluster (Forgejo/OpenBao/Redis, eux,
restent en cluster et donc perdus en mode "down" — seul PostgreSQL en
beneficie ici).

Premiere connexion apres une pause : ~15s de latence (jusqu'a ~30s si
pause de plus de 24h) — prevoir un timeout client en consequence (deja le
cas pour la plupart des pools de connexions `sqlx`/`asyncpg`).

**Mot de passe** : genere par AWS Secrets Manager
(`manage_master_user_password = true`), jamais defini dans ce module.
`terraform output -raw db_admin_password` le recupere si besoin
individuellement ; `helm_values_snippet` (sortie sensible) l'inclut deja.

## Trois paliers : up / pause / down

Deux facons d'economiser entre deux sessions de travail, du moins violent
au plus radical. Les deux n'exigent aucun changement de code, seulement
des `-var` differents au meme `terraform apply`.

### Palier "pause" (recommande entre deux sessions) — scaler le node group a 0

```bash
terraform apply -var="node_desired_size=0" -var="node_min_size=0"
# ... plus tard ...
terraform apply -var="node_desired_size=2" -var="node_min_size=1"   # valeurs habituelles
```

Redimensionne l'ASG existant en place (pas de destruction/recreation du
node group ni du cluster). Le control plane EKS et la NAT Gateway
continuent de tourner (et d'etre factures), mais **toutes les donnees
persistantes survivent intactes** : un PVC/volume EBS n'est jamais
supprime quand le noeud qui l'utilisait disparait, il est juste
"detache" jusqu'a ce qu'un nouveau noeud le remonte au prochain scale-up
— PostgreSQL/Forgejo/OpenBao/Redis en cluster retrouvent exactement leur
etat. Aucun `helm uninstall`/`helm install` a refaire : les pods restent
`Pending` (faute de noeud) pendant la pause, puis se replanifient tout
seuls des que l'ASG remonte.

### Filet de securite : pause automatique quotidienne

`auto_pause_enabled` (`true` par defaut, `auto-pause.tf`) programme le
palier "pause" ci-dessus **automatiquement** chaque jour a
`auto_pause_schedule` (defaut `cron(0 2 * * ? *)`, fuseau
`auto_pause_timezone`, defaut `Europe/Paris`) via EventBridge Scheduler -
aucun Lambda : un "universal target" appelle directement
`eks:UpdateNodegroupConfig`. Sans reprise automatique symetrique par
conception : un oubli ne doit couter que la nuit qui suit, pas bloquer une
reprise de travail le lendemain qui reste un `terraform apply` volontaire
(voir palier "pause" ci-dessus).

**Piege reel rencontre en testant** : le target universel EventBridge
Scheduler valide l'`Input` contre le modele du SDK AWS, qui pour EKS
attend des cles en `PascalCase` (`ClusterName`, `NodegroupName`,
`ScalingConfig` avec `MinSize`/`MaxSize`/`DesiredSize`) — **pas** le
camelCase documente par la reference REST de l'API
(`UpdateNodegroupConfig`). Une premiere version avec des cles camelCase
echouait systematiquement (`ValidationException: ... missing ... ClusterName,
NodegroupName`), corrige dans `auto-pause.tf`.

Verifie empiriquement (2026-08-25) par un declenchement reel (expression
`at()` a +2 minutes, hors Terraform) : le node group est bien passe a
`minSize=0/desiredSize=0`, les 2 noeuds sont sortis `NotReady` puis ont
disparu, confirmant que le role IAM/la permission/le format de charge
utile fonctionnent de bout en bout — pas seulement que
`aws_scheduler_schedule` a ete cree sans erreur cote Terraform.

### Palier "down" (compte de test pur, entre deux demos espacees) — destruction complete

```bash
terraform apply -var="enable_cluster=false"   # detruit cluster + node group + NAT Gateway
terraform apply -var="enable_cluster=true"    # recree tout (nouveau cluster, pas un redemarrage)
```

**Ce qui survit** (cout residuel proche de 0) : VPC (sans NAT, gratuit),
les 3 buckets S3 (donnees conservees), le role IAM (memes permissions,
meme ARN au retour), la zone Route53 et sa delegation Cloudflare (pas
besoin de re-propager les NS a chaque cycle).

**Ce qui ne survit pas** : le cluster Kubernetes lui-meme et tout ce
qu'il contenait — PostgreSQL/Forgejo/OpenBao/Redis en cluster, Workshops
actifs, volumes EBS des PVC (detruits avec le cluster, contrairement au
palier "pause"). `helm install` est a refaire integralement a chaque
"up" ; seules les donnees deja evacuees vers S3 (archives de session,
snapshots) survivent. A reserver a un compte purement demonstratif dont
les donnees en cluster n'ont pas besoin de persister d'une session a
l'autre.

### Estimation de couts (eu-west-3, ordres de grandeur mi-2026, hors taxes)

Prix non verifies contre la AWS Pricing Calculator au moment precis de
l'usage — a reconfirmer avant un engagement long, notamment le prix
horaire de l'instance qui varie par famille/taille/region.

| Poste | "up" (config par defaut) | "pause" (noeuds a 0) | "down" |
|---|---|---|---|
| Control plane EKS | ~$0.10/h ≈ $73/mois | $73/mois (inchange) | $0 (detruit) |
| 2× `m7i.xlarge` (on-demand) | ~$0.24/h chacune ≈ $350/mois | $0 | $0 |
| NAT Gateway (1, `single_nat_gateway`) | ~$0.048/h ≈ $35/mois + trafic | $35/mois (inchange) | $0 (detruit) |
| EBS gp3, 2×100 Go | ~$18/mois | $18/mois (volumes conserves) | $0 (detruit) |
| 3 buckets S3 | quelques cents | identique | identique |
| Zone Route53 | ~$0.50/mois + requetes | identique | identique |
| Aurora Serverless v2 (voir note) | ~$0-15/mois en usage test | identique | identique |
| **Total approximatif** | **~$475-495/mois** | **~$126-141/mois** | **~$1-16/mois** |
| Donnees en cluster (PVC) | — | **conservees** | **perdues** |
| Donnees PostgreSQL (Aurora) | — | **toujours conservees** | **toujours conservees** |

Le node group est aussi le poste le plus reductible en amont, sans meme
passer en pause/down : `node_desired_size = 1` (au lieu de `2`) coupe deja
quasiment $175/mois sur la config "up" ci-dessus.

**Note Aurora** : ~$0.16/ACU-heure (estimation eu-west-3) + stockage
(~$0.11/Go-mois) + I/O. Ligne volontairement large : avec l'auto-pause
active par defaut (`db_min_acu = 0`), le cout reel depend entierement du
temps effectivement passe en pause, invisible depuis ce module — un
compte de test peu sollicite s'approche du bas de la fourchette
(quelques dollars, stockage seul), un usage continu a `db_max_acu = 4`
s'en approcherait du haut (~$460/mois, a ne pas confondre avec la
fourchette ci-dessus qui suppose un usage test intermittent). Ce poste
est **independant des 3 paliers** ci-dessus (voir section "Base de
donnees").

## Securite

Audit realise le 2026-08-25 (Well-Architected, manuellement — le skill
`aws-security` de l'Agent Toolkit AWS n'etait pas encore charge dans la
session). Corrections appliquees :

- **API server EKS** : `public_access_cidrs` restreint a
  `var.admin_access_cidrs` (voir `terraform.tfvars`) au lieu du
  `0.0.0.0/0` par defaut du module. A mettre a jour si l'IP admin change
  (box residentielle) : `curl -s https://checkip.amazonaws.com`.
- **Volumes EBS des noeuds** : chiffres (`encrypted = true`, cle geree AWS
  `aws/ebs`) — voir `modules/cluster/eks.tf`.
- **Audit du control plane EKS** : `cluster_enabled_log_types` (api,
  audit, authenticator, controllerManager, scheduler) vers CloudWatch
  Logs, retention `var.cluster_log_retention_days` (14 jours par defaut).
- **VPC Flow Logs** : trafic `REJECT` uniquement (detection de scans/regles
  mal configurees sans payer le volume du trafic normal), retention
  `var.flow_log_retention_days`.
- **Security group Aurora** : plus d'egress `0.0.0.0/0` (aucune regle
  egress = tout bloque en sortie, Aurora n'a besoin d'aucun flux sortant).
- **Sauvegardes Aurora** : `backup_retention_period` porte a
  `var.db_backup_retention_days` (7 jours par defaut, contre 1 jour par
  defaut AWS), fenetres de maintenance/backup explicites.
- **Suppression accidentelle Aurora** : `deletion_protection` actif par
  defaut (`var.db_deletion_protection`). Pour une destruction volontaire :
  `terraform apply -var="db_deletion_protection=false"` avant
  `terraform destroy`.
- **Rotation du secret Aurora** : rotation native RDS (pas de Lambda a
  fournir) tous les `var.db_secret_rotation_days` (90 jours par defaut).
- **Alerte de cout** (`modules/cluster/budgets.tf`) : `aws_budgets_budget`
  scope par tag `atelier.dev/cluster`, alerte email
  (`var.budget_alert_email`) au-dela de `var.budget_alert_threshold_percent`
  (80% par defaut) de `var.budget_limit_usd` (50$ par defaut). Complementaire
  au filet reactif d'auto-pause (voir "Filet de securite" ci-dessus) —
  celui-ci alerte, l'auto-pause agit.

**Decision annulee : cle KMS dediee (CMK)** pour S3/Aurora/EBS a la place
des cles gerees AWS par defaut (`aws/s3`, `aws/rds`, `aws/ebs`). Ecarte
car cout operationnel (gestion de la politique de cle, rotation, IAM
supplementaire) sans benefice fonctionnel pour un compte de test sans
exigence de conformite (SOC2, HDS, etc.). A reconsiderer si une telle
exigence apparait.

## Destruction

```bash
terraform destroy   # depuis live/dev/
```

Le bucket de state (cree par `bootstrap.sh`) n'est **jamais** detruit
automatiquement — script volontairement a sens unique (pas de
`bootstrap.sh --destroy`), pour eviter de perdre le state d'un
environnement encore actif par erreur. Le supprimer a la main si
reellement voulu, une fois `terraform destroy` termine :

```bash
aws s3 rb "s3://tf-state-<account-id>-atelier" --force
```

Les buckets S3 applicatifs (`modules/cluster/s3.tf`, buckets sessions/
snapshots/forgejo) n'ont pas de `prevent_destroy` : `terraform destroy`
les supprime avec le reste.

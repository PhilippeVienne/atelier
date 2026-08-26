# Guide Administrateur — Chart Helm `atelier`

> Ce guide couvre le deploiement, la configuration et l'exploitation du
> chart Helm monolithique `charts/atelier` (Jalon M6). Il complete
> [`docs/specs/02-helm-deployment-admin-doc.md`](specs/02-helm-deployment-admin-doc.md)
> (decisions d'architecture) avec des procedures d'exploitation concretes.
> Statut de verification empirique detaille dans
> [`docs/PROGRESS.md`](PROGRESS.md).

## 1. Prerequis

### 1.1. KVM : bare-metal ou virtualisation imbriquee (cloud)

Chaque pod parent d'un `Workshop` heberge une microVM Firecracker, qui a
besoin d'un acces a `/dev/kvm` sur le noeud. Deux cas :

- **Bare-metal (recommande)** : le noeud Kubernetes tourne directement sur
  du materiel physique avec la virtualisation active dans le BIOS/UEFI
  (Intel VT-x ou AMD-V). Verifier avec `kvm-ok` (paquet `cpu-checker`) ou
  `ls /dev/kvm`.
- **Cloud avec virtualisation imbriquee (nested virtualization)** : les
  microVMs Firecracker tournent alors comme des VMs de "niveau 2" a
  l'interieur d'une VM cloud de "niveau 1". Support par fournisseur :
  - **AWS** : instances `.metal` (materiel nu, pas de nested virt
    necessaire) ou instances nitro compatibles nested virt (`m5.metal`,
    `i3.metal`...) — voir la documentation AWS sur le nested virtualization
    KVM-on-KVM.
  - **GCP** : activer `enable-nested-virtualization` a la creation de la VM
    (necessite une image Compute Engine avec le flag de licence nested
    virt, et un type de machine supportant AVX/VMX exposees a l'invite).
  - **Azure** : les series `Dv3`/`Ev3` et plus recentes exposent
    generalement les instructions VMX a l'invite ; verifier avec
    `cat /proc/cpuinfo | grep vmx` a l'interieur de la VM cloud.
  - **Bare-metal cloud** : AWS `.metal`, GCP "Bare Metal Solution", et
    equivalents OVH/Scaleway restent l'option la plus fiable si le nested
    virt du fournisseur choisi s'avere instable.

Le DaemonSet `kvm-device-plugin` (`templates/infra/kvm-device-plugin-daemonset.yaml`,
`kvmDevicePlugin.enabled: true`) expose ensuite `/dev/kvm` comme ressource
Kubernetes standard (`atelier.dev/kvm`) consommee par
`spec.resources.disk`/`cpu` du `Workshop`. Sur un cluster sans acces KVM du
tout (ex: environnement de CI pur), desactiver ce composant
(`kvmDevicePlugin.enabled: false`) : les Workshops resteront alors bloques
en `Pending`, ce qui est le comportement attendu.

### 1.2. Cluster Kubernetes

- Version testee empiriquement : **kind** (Kubernetes 1.29+, voir
  `docs/PROGRESS.md`). Tout cluster conforme (EKS, GKE, AKS, k3s...)
  supportant les CRD `apiextensions.k8s.io/v1` et `batch/v1` `Job` convient.
- Un StorageClass par defaut (ou explicitement configure via
  `global.storageClassName`/`<composant>.persistence.storageClassName`)
  est necessaire pour PostgreSQL, Forgejo, OpenBao, Redis et RustFS (sauf
  si `persistence.enabled: false`, auquel cas `emptyDir` est utilise — a
  ne jamais faire en production, les donnees sont perdues au redemarrage
  du pod).
- Un Ingress Controller (voir section 3).

## 2. Les 4 Domaines DNS

Ce chart expose 4 Ingress dedies, chacun avec son propre domaine
(`values.yaml`, cle `domains`) :

| Cle              | Usage                                      | Exemple                     |
|------------------|---------------------------------------------|------------------------------|
| `domains.keycloak`  | Connexion OIDC (dashboard + validation JWT api-server) | `auth.exemple.com` |
| `domains.forgejo`   | Depots Git internes (100% HTTPS, pas de SSH) | `git.exemple.com`          |
| `domains.dashboard` | Interface web (Next.js)                    | `app.exemple.com`            |
| `domains.apiServer` | API REST + WebSocket + serveur MCP externe | `api.exemple.com`            |

Chacun requiert un enregistrement DNS (A/AAAA ou CNAME) pointant vers
l'adresse de l'Ingress Controller (`kubectl get svc -n <namespace-ingress>`).
Avec `tls.certManager.enabled: true`, un certificat Let's Encrypt (ou
l'autorite configuree via `tls.certManager.issuer`) est demande separement
pour chacun des 4 domaines — le DNS doit donc etre en place *avant*
l'installation si un challenge HTTP-01 est utilise (DNS-01 tolere un
decalage).

## 3. Choix du Controleur d'Ingress

Les annotations par defaut de `apiServer.ingress.annotations` et
`dashboard.ingress.annotations` (timeouts etendus, `websocket-services`)
sont ecrites pour **ingress-nginx** (`ingress.className: "nginx"` par
defaut), le controleur le plus repandu et celui explicitement documente
dans `docs/specs/02-helm-deployment-admin-doc.md`.

**Support WebSocket** : ingress-nginx (comme la plupart des controleurs
modernes — Traefik, Contour, HAProxy Ingress) relaie nativement une requete
`Upgrade: websocket` sans configuration particuliere ; le point d'attention
reel est le **timeout d'inactivite**, qui coupe silencieusement une session
shell (`exec_in_workshop`, terminal navigateur) restee inactive au dela de
la valeur par defaut du controleur (60s chez ingress-nginx). D'ou
`nginx.ingress.kubernetes.io/proxy-read-timeout`/`proxy-send-timeout` a
`3600` sur `apiServer` et `dashboard`.

**Autre controleur (Traefik, Contour, HAProxy...)** : mettre
`ingress.className` sur la classe voulue et **remplacer**
`apiServer.ingress.annotations`/`dashboard.ingress.annotations` par les
annotations equivalentes. Exemples :

```yaml
# Traefik (verifie fonctionnel dans le test de fumee de ce chart, sans
# annotations dediees : les valeurs par defaut de Traefik n'ont pas coupe
# la session lors du test) :
ingress:
  className: "traefik"
apiServer:
  ingress:
    annotations:
      traefik.ingress.kubernetes.io/router.middlewares: "atelier-long-timeout@kubernetescrd"
```

## 4. Identites Cloud Natives (Zero Secret Statique)

`cloudIdentity.provider` (`none`/`aws`/`gcp`/`azure`) documente
l'intention ; `cloudIdentity.annotations` est le mecanisme reel — ces
annotations sont fusionnees sur **tous** les ServiceAccounts crees par ce
chart (`controller`, `apiServer`, `pmEngine`, le pool `init-jobs`).

### 4.1. AWS EKS — IRSA / EKS Pod Identity / `sts:AssumeRole`

```yaml
cloudIdentity:
  provider: "aws"
  annotations:
    eks.amazonaws.com/role-arn: "arn:aws:iam::123456789012:role/atelier-controller"
```

Le role IAM cible doit avoir une politique de confiance (`trust policy`)
autorisant le fournisseur OIDC du cluster EKS a assumer ce role pour le
sujet `system:serviceaccount:<namespace>:<nom-du-serviceaccount>` (un role
par ServiceAccount si les permissions doivent differer — typiquement,
`s3Init`/RustFS a besoin d'ecrire dans S3, `controller`/`apiServer` n'en
ont pas besoin par defaut). Pour `s3Storage.external.enabled: true` avec
`assumeRoleArn` renseigne, le SDK AWS du composant (ou le client `mc` du
Job `s3-init`) assume ce role via IRSA sans jamais manipuler de cle d'acces
statique.

### 4.2. GCP GKE — Workload Identity Federation

```yaml
cloudIdentity:
  provider: "gcp"
  annotations:
    iam.gke.io/gcp-service-account: "atelier-apiserver@mon-projet.iam.gserviceaccount.com"
```

Prerequis cote GCP : `gcloud iam service-accounts add-iam-policy-binding`
liant le compte de service Kubernetes (`<namespace>/<nom-du-serviceaccount>`)
au compte de service GCP via le role `roles/iam.workloadIdentityUser`.

### 4.3. Azure AKS — Microsoft Entra Workload ID

```yaml
cloudIdentity:
  provider: "azure"
  annotations:
    azure.workload.identity/client-id: "11111111-2222-3333-4444-555555555555"
```

Necessite egalement le label `azure.workload.identity/use: "true"` sur les
pods (a ajouter via `<composant>.podLabels` si expose, ou en surchargeant
le template localement — non expose comme valeur separee dans la version
actuelle du chart, voir section 8 "Limites Connues").

### 4.4. Fallback : Secrets statiques (`provider: "none"`)

Sans identite cloud, `cloudIdentity.fallbackSecretName` (nom d'un Secret
Kubernetes `Opaque` a creer manuellement AVANT `helm install`, jamais genere
par ce chart) doit porter les cles attendues par chaque integration externe
active — voir `s3-init-job.yaml` pour l'exemple concret (cles `accessKeyId`/
`secretAccessKey` utilisees quand `s3Storage.external.enabled: true`).

## 5. Stockage S3 Multi-Cloud

`s3Storage.rustfs.enabled: true` (par defaut) deploie un backend S3
embarque (RustFS, 100% Rust) — adapte a un premier deploiement ou un usage
on-premise sans dependance cloud. Pour un backend externe :

```yaml
s3Storage:
  rustfs:
    enabled: false
  external:
    enabled: true
    endpoint: "https://s3.eu-west-1.amazonaws.com"   # ou equivalent GCS/Azure Blob (API S3-compatible)
    region: "eu-west-1"
    assumeRoleArn: "arn:aws:iam::123456789012:role/atelier-s3"  # AWS uniquement, vide sinon
    forcePathStyle: false   # true pour la plupart des backends S3-compatibles hors AWS (GCS, MinIO, RustFS distant)
```

`s3-init-job.yaml` (hook `post-install`) cree alors les 3 buckets
(`s3Storage.buckets.sessions`/`snapshots`/`forgejo`) contre ce endpoint via
`mc` (client S3-compatible), en utilisant soit les cles du Secret
`cloudIdentity.fallbackSecretName` (provider `none`), soit une identite
cloud native (IRSA/Workload Identity) une fois branchee dans l'image du Job
— voir "Limites Connues" (section 8) pour l'etat exact de ce branchement.

**GCS** : utiliser l'endpoint interop S3 de GCS
(`https://storage.googleapis.com`) avec `forcePathStyle: true` et des cles
HMAC generees via `gcloud storage hmac create`.

**Azure Blob Storage** : ne parle pas nativement le protocole S3 ; utiliser
soit un endpoint MinIO Gateway devant Azure Blob (deploiement separe, hors
perimetre de ce chart), soit un fork RustFS/adaptateur compatible Azure.

## 6. Sequencement au Premier Demarrage

Les 5 Jobs d'initialisation (`templates/jobs/*.yaml`) sont **tous** des
hooks Helm `post-install,post-upgrade` (pas `pre-install`) — decision prise
apres un test de deploiement reel qui a revele une dependance circulaire :
un hook `pre-install` s'execute avant que Helm ne cree la moindre ressource
normale du chart (StatefulSet `postgresql`, ServiceAccounts...), rendant
impossible d'attendre une base de donnees qui n'existe pas encore. Voir le
commentaire dans `templates/jobs/db-init-job.yaml` pour le detail verifie
empiriquement.

Consequence acceptee : `controller`, `api-server`, `keycloak` et `litellm`
demarrent **en parallele** de leurs dependances (PostgreSQL, bases de
donnees applicatives). Sur une premiere installation, il est normal
d'observer un `CrashLoopBackOff` transitoire de quelques dizaines de
secondes a quelques minutes pendant que :

1. `db-init-job` cree les 6 bases + le role `atelier_migrator` ;
2. `controller`/`api-server`, deja en boucle de redemarrage, finissent par
   trouver leur base prete et executent alors leur propre `sqlx::migrate!`
   embarque (suivi standard via la table `_sqlx_migrations`, jamais rejoue
   une fois applique) avant de demarrer normalement.

`db-migrate-job` (`initJobs.dbMigrate.enabled`, **desactive par defaut**)
executait auparavant le meme contenu SQL via `psql -f` brut, SANS aucun
suivi des migrations deja appliquees — verifie empiriquement lors du test de
deploiement reel de ce chart (Jalon M6) : un simple `helm upgrade` (meme
sans changement) le rejouait entierement et echouait systematiquement
("relation ... already exists"), en plus de dupliquer inutilement le
travail deja fait par `sqlx::migrate!` cote binaires. Laisse desactive tant
qu'il n'a pas ete reecrit pour reellement suivre l'etat d'application (ex:
`sqlx-cli migrate run`, qui partage la meme table `_sqlx_migrations`).

Ordre garanti entre les hooks post-install via `helm.sh/hook-weight`
(`db-init` = -11/-10, `db-migrate` = -6/-5, `keycloak-init`/`openbao-init`/
`s3-init` = 0/1) : la base de donnees est toujours prete avant que
`keycloak-init` ne tente de creer le Realm.

`kubectl get pods -w` pendant les premieres minutes suivant `helm install`
est la commande de diagnostic recommandee ; un pod qui ne se stabilise
**jamais** (plus de 5-10 minutes de `CrashLoopBackOff`) indique un probleme
reel (identifiants PostgreSQL incorrects, image absente...), a distinguer
du sequencement normal decrit ci-dessus.

### 6.1. Dimensionnement memoire de Keycloak et LiteLLM

Verifie empiriquement sur `kind-atelier-dev` (2026-08-24) : les limites
memoire "generiques" (512Mi-1Gi) suffisantes pour `controller`/`api-server`
/`dashboard` provoquent un `OOMKilled` systematique pour :

- **Keycloak** (JVM Quarkus, phase de "build"/augmentation de la
  configuration a chaque redemarrage en mode `start`) : `2560Mi` de limite
  necessaire pour un demarrage stable (valeur par defaut du chart).
- **LiteLLM** (serveur Python + Prisma) : `1536Mi` de limite necessaire
  (valeur par defaut du chart).

Sur un cluster aux noeuds contraints, ne pas reduire ces valeurs sans
nouveau test empirique — un `OOMKilled` recurrent au demarrage se
manifeste par un `CrashLoopBackOff` qui ne se stabilise jamais (voir
section 6 ci-dessus pour la distinction avec le sequencement normal).

## 7. OpenBao : Initialisation et Descellement

### 7.1. Mode dev (`openbao.devMode: true`)

Recommande uniquement pour un environnement de test/demonstration : le
serveur demarre non scelle avec un jeton racine fixe (`"root"`),
automatiquement injecte dans `openbao-init-job` **et** dans
`controller` (variable `OPENBAO_TOKEN`, necessaire des que `OPENBAO_ADDR`
est definie — voir `crates/controller/src/openbao.rs::config_from_env`).
Toute donnee est perdue au redemarrage du pod (`-dev`, stockage en
memoire).

### 7.2. Mode production (`openbao.devMode: false`)

OpenBao demarre **scelle** avec un stockage Raft persistant
(`templates/infra/openbao-statefulset.yaml`). `bao operator init` reste
**deliberement manuel**, hors du cycle de vie Helm (ne doit JAMAIS etre
automatise dans un hook — un `helm upgrade` ne doit pas pouvoir
re-initialiser silencieusement un coffre de secrets existant), mais le
**descellement** peut etre automatique via KMS (`openbao.seal.type: awskms`,
voir `deploy/terraform/aws/modules/cluster/openbao-unseal.tf`) - fortement
recommande sur AWS : sans lui, chaque redemarrage du pod (upgrade EKS,
scale-down/up du node group, reschedule) exige de redescelle manuellement,
constate a plusieurs reprises en pratique comme source de panne (controller
et api-server bloques tant qu'OpenBao reste scelle).

**Avec `openbao.seal.type: awskms` (recommande sur AWS)** :

```sh
# Une seule fois, juste apres le premier demarrage du pod OpenBao :
kubectl exec -it <pod-openbao> -n <namespace> -- bao operator init \
  -recovery-shares=5 -recovery-threshold=3
# Descelle IMMEDIATEMENT (Sealed: false des la fin de la commande, verifier
# avec `bao status`) - pas d'etape "bao operator unseal" a faire. Conserver
# les 5 "Recovery Key" et le "Initial Root Token" dans un coffre externe
# (jamais dans ce depot Git, jamais dans un ConfigMap) : les recovery keys
# ne servent qu'a des operations d'urgence (rotation de cle, generation
# d'un nouveau root token), jamais au demarrage normal.
```

**Sans auto-unseal (`openbao.seal.type: shamir`, defaut)** :

```sh
# Une seule fois, juste apres le premier demarrage du pod OpenBao :
kubectl exec -it <pod-openbao> -n <namespace> -- bao operator init \
  -key-shares=5 -key-threshold=3
# Conserver les 5 "Unseal Key" et le "Initial Root Token" dans un coffre
# externe (jamais dans ce depot Git, jamais dans un ConfigMap).

# A chaque redemarrage du pod OpenBao (perte de l'etat "unsealed" en memoire) :
kubectl exec -it <pod-openbao> -n <namespace> -- bao operator unseal <cle-1>
kubectl exec -it <pod-openbao> -n <namespace> -- bao operator unseal <cle-2>
kubectl exec -it <pod-openbao> -n <namespace> -- bao operator unseal <cle-3>
```

**Piege IRSA vs Pod Identity** (`openbao.seal.awskms.roleArn`) : le SDK AWS
embarque dans l'image `openbao/openbao:2.0.0` rejette l'endpoint EKS Pod
Identity (`169.254.170.23`, "only loopback hosts are allowed" - constate
empiriquement) - contrairement aux autres composants AWS de ce projet
(aws-ebs-csi-driver, aws-load-balancer-controller), OpenBao a besoin
d'IRSA (federation OIDC classique), pas de Pod Identity. Apres tout
changement d'annotation `eks.amazonaws.com/role-arn` sur sa
ServiceAccount, le pod OpenBao doit etre redemarre manuellement
(`kubectl delete pod`) : le webhook mutant qui injecte
`AWS_ROLE_ARN`/`AWS_WEB_IDENTITY_TOKEN_FILE` n'agit qu'a la creation du
pod, jamais sur un pod deja demarre.

Une fois descelle, creer le Secret Kubernetes attendu par
`openbao.rootTokenSecretName` (cle `token`) avec un jeton ayant les droits
suffisants (pas necessairement le jeton racine initial — un jeton de
politique dediee est recommande en production) :

```sh
kubectl create secret generic atelier-openbao-token -n <namespace> \
  --from-literal=token='<jeton-openbao>'
```

Puis `helm upgrade` avec `openbao.rootTokenSecretName: atelier-openbao-token`
pour que `openbao-init-job` (activation de la methode d'auth Kubernetes) et
`controller` (provisioning du role OpenBao cluster-wide pour `api-server`)
puissent s'authentifier. **Sans ce Secret, `openbao-init-job` echoue
explicitement** (jamais de succes silencieux) — voir le message d'erreur
dans le Job.

## 8. Limites Connues (honnetete du DoD)

- **`services/pm-engine` (Jalon M5)** : le template
  `templates/core/pm-engine-deployment.yaml` est ecrit d'apres la
  specification (port 8000, variables d'environnement deduites) mais n'a
  pas pu etre confronte a l'implementation reelle du service au moment de
  ce Jalon M6 (developpement en parallele par un autre agent). A
  reconcilier une fois M5 livre.
- **`dashboard`** : aucune image `atelier-dashboard:dev` n'etait construite
  localement au moment du test de deploiement reel documente dans
  `docs/PROGRESS.md` — le template `dashboard-deployment.yaml` a ete
  valide par `helm template`/`kubectl apply --dry-run=client` uniquement,
  pas par un pod reellement demarre.
- **`api-server` : image locale `atelier-api-server:dev` obsolete au moment
  du test de deploiement reel** (`imagePullPolicy: Never`, image construite
  le 2026-08-23, avant les evolutions de `crates/api-server/src/routes.rs`
  des Jalons M2/M3 du 2026-08-24) : `/health/readiness` exigeait encore un
  `Authorization: Bearer` dans cette image, faisant echouer les sondes
  Kubernetes en boucle. Le code source ACTUEL de `routes.rs` expose deja
  `/health/liveness`/`/health/readiness` sur un `Router` separe, sans le
  middleware `require_auth` — reconstruire l'image locale
  (`docker build ... -t atelier-api-server:dev`) suffit a resoudre ce point,
  ce n'est pas un defaut du chart. Tous les autres composants (`keycloak`,
  `litellm`, `rustfs`, `forgejo`, `openbao`, `postgresql`, `redis`,
  `pm-engine`, `controller`) ont ete confirmes `Running`/`Ready` lors de ce
  meme test, avec les 5 Jobs d'initialisation `Completed`.
- **Cloud identity sur les Jobs d'init** : les annotations IRSA/Workload
  Identity sont bien posees sur le ServiceAccount `init-jobs`
  (`templates/rbac/serviceaccounts.yaml`), mais `s3-init-job` utilise
  aujourd'hui des cles d'acces explicites (Secret) dans son exemple
  `external.enabled: true` — l'usage reel d'une identite cloud par le
  client `mc` a l'interieur du Job necessiterait un adaptateur
  supplementaire (ex: `mc alias set` ne sait pas nativement lire un jeton
  IRSA), non implemente dans cette version.
- **`NeedsRestartForUpgrade`** : `WorkshopStatus.upgrade_state`
  (`crates/common/src/crd.rs`) existe et est correctement reporte d'une
  reconciliation a l'autre (`crates/controller/src/reconcile.rs::carry_forward_status`),
  mais **aucune detection automatique** d'un changement de version de
  template de pod parent n'est encore implementee cote controller (hors
  perimetre de cette tache, qui portait sur le chart Helm) : le champ est
  pret a etre positionne, mais rien ne le positionne encore aujourd'hui.
  Cela reste conforme a l'objectif "non perturbateur" du Jalon M6 (un
  `helm upgrade` du controller/api-server ne touche jamais aux pods
  parents des Workshops, qui sont des ressources independantes possedees
  par le controller, pas par ce chart), meme si la fonctionnalite de
  notification explicite reste a batir.
- **Deploiement complet non valide de bout en bout avec TLS/cert-manager
  reel** : le test empirique documente dans `docs/PROGRESS.md` desactive
  `tls.enabled` (pas de domaine public reel/certificat Let's Encrypt
  disponible dans l'environnement de test). Le rendu des annotations
  cert-manager est verifie par `helm template`, pas par un certificat
  reellement emis.

## 9. Sauvegarde et Restauration PostgreSQL

Le PostgreSQL embarque (`postgresql.enabled: true`) est un unique
StatefulSet avec un PVC (`postgresql.persistence`). Sans solution de
sauvegarde geree (RDS/Cloud SQL/Azure Database, recommande en production —
voir `postgresql.external`), utiliser `pg_dump`/`pg_restore` :

### 9.1. Sauvegarde (toutes les bases applicatives)

```sh
NAMESPACE=atelier
POD=$(kubectl get pod -n "$NAMESPACE" -l app.kubernetes.io/component=postgresql -o jsonpath='{.items[0].metadata.name}')

for db in atelier_apiserver atelier_controller atelier_pm atelier_keycloak atelier_forgejo atelier_litellm; do
  kubectl exec -n "$NAMESPACE" "$POD" -- pg_dump -U atelier_admin -Fc "$db" > "backup-${db}-$(date +%Y%m%d).dump"
done
```

Automatiser via un `CronJob` Kubernetes externe a ce chart (non fourni ici
delibrement : la strategie de retention/destination des sauvegardes — S3,
Glacier, disque local... — depend trop de l'environnement cible pour un
choix unique raisonnable dans un chart generique).

### 9.2. Restauration

```sh
# 1. Recreer la base vide (voir db-init-job.yaml pour la convention de
#    nommage et le proprietaire attendu, atelier_migrator) :
kubectl exec -n "$NAMESPACE" "$POD" -- psql -U atelier_admin -d postgres \
  -c "DROP DATABASE IF EXISTS atelier_apiserver;" \
  -c "CREATE DATABASE atelier_apiserver OWNER atelier_migrator;"

# 2. Restaurer :
kubectl exec -i -n "$NAMESPACE" "$POD" -- pg_restore -U atelier_admin -d atelier_apiserver \
  < backup-atelier_apiserver-20260101.dump
```

**Toujours** mettre `controller`/`api-server` a l'echelle 0 replica
(`kubectl scale deployment ... --replicas=0`) avant une restauration
complete : une ecriture concurrente pendant le `DROP DATABASE`/`pg_restore`
peut corrompre l'etat applicatif.

## 10. Depannage

| Symptome | Cause probable | Verification |
|---|---|---|
| `CrashLoopBackOff` sur `controller` avec `OPENBAO_ADDR est defini mais OPENBAO_TOKEN est absent` | `openbao.enabled: true`, `devMode: false` et `rootTokenSecretName` vide | Suivre la section 7.2, ou desactiver OpenBao pour le controller en laissant `rootTokenSecretName` vide ET `devMode: false` (le controller tourne alors sans OpenBao, fonctionnalite optionnelle) |
| `api-server` : `error sending request ... dns error` en recuperant le JWKS | `ATELIER_JWT_JWKS_URL` pointe vers le domaine public au lieu du Service interne | Verifie deja corrige dans `apiserver-deployment.yaml` (JWKS = Service interne, issuer = domaine public) — si l'erreur persiste, verifier une eventuelle surcharge locale de `apiServer.env` |
| `api-server`/`controller` en `CrashLoopBackOff` qui finit par se stabiliser seul en quelques minutes | Sequencement normal au premier demarrage (section 6) | `kubectl get pods -w`, attendre la fin de `db-init`/`db-migrate` |
| `keycloak-init`/`openbao-init`/`s3-init` restent `Running` indefiniment | Le service cible (Keycloak/OpenBao/RustFS) n'a jamais atteint l'etat pret (OOM, image absente...) | `kubectl describe pod` sur le service cible, pas sur le Job lui-meme |
| `helm install` echoue avec `customresourcedefinitions.apiextensions.k8s.io "workshops.atelier.dev" already exists` | Le CRD `Workshop` est deja gere par une autre installation Helm dans ce cluster (les CRD sont cluster-scoped) | `crds.install: false` si le CRD est deja applique ailleurs — un seul chart `atelier` par cluster doit posseder ce CRD |
| `helm upgrade`/`install` : `another operation (install/upgrade/rollback) is in progress` | Une operation precedente a echoue/timeout et a laisse le Release en `pending-*` | `helm uninstall` puis reinstaller, ou `helm rollback` si une revision anterieure stable existe |
| Buckets S3 non crees | `s3-init-job` execute avant que RustFS/le endpoint externe ne soit joignable | Le Job boucle sur `mc alias set` jusqu'a joindre le endpoint (pas de timeout cote script) — verifier `kubectl logs job/<...>-s3-init` pour confirmer qu'il boucle plutot que d'avoir echoue |

## 11. Un Seul Chart `atelier` par Cluster

Le CRD `Workshop` (`templates/crds/workshop.yaml`) est **cluster-scoped** :
deux installations Helm distinctes de ce chart dans le meme cluster
entreraient en conflit sur ce CRD (voir la table de depannage ci-dessus).
Pour un cluster multi-tenant necessitant plusieurs instances logiques
d'Atelier, deployer une seule fois le CRD (`crds.install: true` sur une
seule Release) puis `crds.install: false` sur toutes les autres Releases
dans des namespaces differents.

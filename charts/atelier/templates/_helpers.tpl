{{/*
Nom court du chart, utilise comme prefixe de la plupart des ressources.
*/}}
{{- define "atelier.name" -}}
{{- .Chart.Name -}}
{{- end -}}

{{/*
Nom complet d'une release, prefixe des noms de ressources (Deployments,
Services, Jobs...). Suit la convention standard des charts Helm.
*/}}
{{- define "atelier.fullname" -}}
{{- if contains .Chart.Name .Release.Name -}}
{{- .Release.Name | trunc 63 | trimSuffix "-" -}}
{{- else -}}
{{- printf "%s-%s" .Release.Name .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{/*
Labels communs, appliques a toute ressource de ce chart (fusionnes avec
`global.extraLabels`).
*/}}
{{- define "atelier.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/part-of: atelier
{{- with .Values.global.extraLabels }}
{{ toYaml . }}
{{- end }}
{{- end -}}

{{/*
Labels de selection pour un composant donne. Usage : `include "atelier.selectorLabels" (dict "root" $ "component" "controller")`.
*/}}
{{- define "atelier.selectorLabels" -}}
app.kubernetes.io/name: {{ printf "%s-%s" (include "atelier.fullname" .root) .component }}
app.kubernetes.io/component: {{ .component }}
{{- end -}}

{{/*
Nom d'une ressource pour un composant donne. Usage : `include "atelier.componentName" (dict "root" $ "component" "controller")`.
*/}}
{{- define "atelier.componentName" -}}
{{- printf "%s-%s" (include "atelier.fullname" .root) .component -}}
{{- end -}}

{{/*
Annotations d'identite Cloud (IRSA/Workload Identity/Workload ID) a fusionner
sur les ServiceAccounts. Vide si `cloudIdentity.provider == "none"` et
qu'aucune annotation explicite n'est fournie.
*/}}
{{- define "atelier.cloudIdentityAnnotations" -}}
{{- .Values.cloudIdentity.annotations | toYaml -}}
{{- end -}}

{{/*
Encode les caracteres reserves d'une URI (RFC 3986) dans un mot de passe
avant de l'interpoler dans une chaine "postgres://user:PASSWORD@host:port/db" -
sans ceci, un mot de passe genere par AWS Secrets Manager (RDS
manage_master_user_password, voir deploy/terraform/aws/modules/cluster/database.tf)
contenant "?"/"#" tronque l'URI au mauvais endroit ("invalid port number"
constate empiriquement lors du premier `helm install` contre Aurora - le "?"
d'un mot de passe demarre une chaine de requete, "#" un fragment). Sprig/Helm
n'expose pas de fonction d'encodage URL native (contrairement a html/template) :
chaine de `replace` sur les caracteres reserves de la RFC, "%" en premier pour
ne pas re-encoder les "%XX" produits par les remplacements suivants. Ne PAS
utiliser pour KC_DB_PASSWORD (keycloak-deployment.yaml) : ce n'est pas une URI,
c'est une valeur de variable d'environnement discrete, l'encoder la
corromprait.
*/}}
{{- define "atelier.urlEncodePassword" -}}
{{- $s := . -}}
{{- $s = $s | replace "%" "%25" -}}
{{- $s = $s | replace ":" "%3A" -}}
{{- $s = $s | replace "/" "%2F" -}}
{{- $s = $s | replace "?" "%3F" -}}
{{- $s = $s | replace "#" "%23" -}}
{{- $s = $s | replace "[" "%5B" -}}
{{- $s = $s | replace "]" "%5D" -}}
{{- $s = $s | replace "@" "%40" -}}
{{- $s = $s | replace " " "%20" -}}
{{- $s -}}
{{- end -}}

{{/*
DSN PostgreSQL pour un composant donne, pointant soit vers le PostgreSQL
embarque de ce chart, soit vers `postgresql.external` si active.
Usage : `include "atelier.postgresDsn" (dict "root" $ "database" .Values.postgresql.databases.apiServer "user" "atelier_app")`.
*/}}
{{- define "atelier.postgresHost" -}}
{{- if .root.Values.postgresql.external.enabled -}}
{{- .root.Values.postgresql.external.host -}}
{{- else -}}
{{- printf "%s-postgresql" (include "atelier.fullname" .root) -}}
{{- end -}}
{{- end -}}

{{/*
Mode SSL a utiliser dans les DATABASE_URL du chart. `postgresql.external.sslMode`
(par defaut "require") est pense pour une base geree externe (RDS/Cloud SQL...)
qui termine reellement du TLS. Le StatefulSet PostgreSQL embarque par ce chart
(templates/infra/postgresql-statefulset.yaml, image postgres officielle sans
certificat configure) ne parle PAS TLS : imposer "require" contre lui echoue
systematiquement ("server does not support TLS", constate empiriquement avec
LiteLLM/Prisma, le seul consommateur a valider strictement la connexion au
demarrage). Ne surcharge donc la valeur choisie par l'utilisateur QUE quand le
Postgres embarque est utilise ET que l'utilisateur a laisse la valeur par
defaut "require" — un choix explicite de "disable"/"prefer" reste respecte.
*/}}
{{- define "atelier.postgresSslMode" -}}
{{- if and (not .root.Values.postgresql.external.enabled) (eq .root.Values.postgresql.external.sslMode "require") -}}
disable
{{- else -}}
{{- .root.Values.postgresql.external.sslMode -}}
{{- end -}}
{{- end -}}

{{/*
Nom du Secret contenant les identifiants PostgreSQL admin/migrator.
*/}}
{{- define "atelier.postgresSecretName" -}}
{{- printf "%s-postgresql-auth" (include "atelier.fullname" .) -}}
{{- end -}}

{{/*
Annotation cert-manager a poser sur un Ingress (vide si TLS ou cert-manager
desactives). Usage : `include "atelier.certManagerAnnotation" $`.
*/}}
{{- define "atelier.certManagerAnnotation" -}}
{{- if and .Values.tls.enabled .Values.tls.certManager.enabled -}}
cert-manager.io/{{ if eq .Values.tls.certManager.issuerKind "ClusterIssuer" }}cluster-issuer{{ else }}issuer{{ end }}: {{ .Values.tls.certManager.issuer | quote }}
{{- end -}}
{{- end -}}

{{/*
Annotations communes a poser sur TOUS les Ingress (contrairement a
<composant>.ingress.annotations, specifiques a un seul) - typiquement les
annotations alb.ingress.kubernetes.io/* (scheme, certificate-arn,
group.name : partager un seul ALB entre les 4 Ingress necessite la meme
valeur de group.name partout, voir deploy/terraform/aws/modules/cluster/outputs.tf).
Usage : `include "atelier.commonIngressAnnotations" $`.
*/}}
{{- define "atelier.commonIngressAnnotations" -}}
{{- with .Values.ingress.annotations }}
{{ toYaml . }}
{{- end }}
{{- end -}}

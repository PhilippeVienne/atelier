#!/usr/bin/env bash
# Installation single-node low-cost d'Atelier (docs/specs/10-low-cost-single-node-install.md).
#
# Usage :
#   curl -fsSL https://raw.githubusercontent.com/PhilippeVienne/atelier/main/scripts/install.sh | bash -s -- --domain atelier.exemple.com --email admin@exemple.com
#
# Idempotent : une deuxieme execution met a jour une installation existante
# (`helm upgrade --install`), elle ne repart jamais de zero.
#
# NON teste de bout en bout sur un serveur frais dans la session qui a
# ecrit ce script (voir la spec, section 6) : execute-le contre une vraie
# VM/un vrai serveur avant de t'y fier en production.
set -euo pipefail

REPO_URL="https://github.com/PhilippeVienne/atelier.git"
INSTALL_DIR="${ATELIER_INSTALL_DIR:-/opt/atelier}"
SRC_DIR="$INSTALL_DIR/src"
VALUES_FILE="$INSTALL_DIR/values-generated.yaml"
CREDENTIALS_FILE="$INSTALL_DIR/credentials.txt"
NAMESPACE="${ATELIER_NAMESPACE:-atelier-system}"
CLUSTER_ISSUER="letsencrypt-prod"
# Versions de chart EPINGLEES (verifiees disponibles le 2026-09-02) — memes
# raisons que les tags d'image explicites du reste du depot : une version
# "latest" non fixee installerait une version differente a chaque execution
# du script, potentiellement incompatible avec les options ci-dessous sans
# avertissement.
INGRESS_NGINX_CHART_VERSION="4.15.1"
CERT_MANAGER_CHART_VERSION="v1.21.1"

DOMAIN=""
EMAIL=""
OPENBAO_PRODUCTION="false"

log()  { echo "==> $*"; }
warn() { echo "AVERTISSEMENT: $*" >&2; }
die()  { echo "ERREUR: $*" >&2; exit 1; }

# --------------------------------------------------------------------------
# 0. Arguments
# --------------------------------------------------------------------------
while [ $# -gt 0 ]; do
  case "$1" in
    --domain) DOMAIN="$2"; shift 2 ;;
    --domain=*) DOMAIN="${1#*=}"; shift ;;
    --email) EMAIL="$2"; shift 2 ;;
    --email=*) EMAIL="${1#*=}"; shift ;;
    --openbao-production) OPENBAO_PRODUCTION="true"; shift ;;
    -h|--help)
      grep '^#' "$0" | sed 's/^# \{0,1\}//' | sed '1,2d'
      exit 0
      ;;
    *) die "argument inconnu: $1 (voir --help)" ;;
  esac
done

# --------------------------------------------------------------------------
# 1. Garde-fous (voir la spec, section 2) — AVANT toute installation.
# --------------------------------------------------------------------------
[ "$(id -u)" -eq 0 ] || die "ce script doit s'executer en root (ou via sudo)."

ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64|aarch64|arm64) ;;
  *) die "architecture non supportee: $ARCH (x86_64/arm64 attendus)." ;;
esac

if [ ! -e /dev/kvm ]; then
  die "/dev/kvm est absent : Atelier ne peut faire tourner AUCUN Workshop \
sans acces materiel a KVM. La plupart des VPS grand public (Droplets, \
instances cloud standard...) n'exposent pas la virtualisation imbriquee a \
l'invite. Un serveur bare-metal (virtualisation activee au BIOS/UEFI) ou \
une instance cloud explicitement 'metal'/nested-virt est requis — voir \
docs/admin-guide.md, section 1.1, pour le detail par fournisseur cloud."
fi
if [ ! -r /dev/kvm ] || [ ! -w /dev/kvm ]; then
  die "/dev/kvm existe mais n'est pas accessible en lecture/ecriture pour \
ce processus — verifie les permissions (groupe 'kvm') avant de continuer."
fi
log "/dev/kvm accessible — virtualisation materielle confirmee."

command -v systemctl >/dev/null 2>&1 || die "systemd est requis (k3s en depend)."

# --------------------------------------------------------------------------
# 2. Domaine / e-mail
# --------------------------------------------------------------------------
# Sous `curl | bash`, stdin (fd 0) est deja occupe par le FLUX DU SCRIPT
# lui-meme : un `read` normal n'y lirait pas une reponse tapee au clavier
# mais la suite du script (ou EOF) — bug classique des installeurs
# `curl | bash` avec invite interactive. `/dev/tty` (le terminal de
# controle reel) reste accessible independamment de la redirection de
# stdin ; on y lit l'invite quand elle existe, et on exige les arguments
# sinon (execution non interactive, ex: dans une pipeline CI).
prompt() {
  local message="$1"
  if [ -t 0 ]; then
    read -r -p "$message" REPLY
  elif [ -r /dev/tty ]; then
    read -r -p "$message" REPLY < /dev/tty
  else
    REPLY=""
  fi
}

if [ -z "$DOMAIN" ]; then
  prompt "Domaine de base (ex: atelier.exemple.com) : "
  DOMAIN="$REPLY"
fi
[ -n "$DOMAIN" ] || die "un domaine de base est requis : relance avec \
--domain <domaine> (l'invite interactive n'est pas disponible sans \
terminal de controle, ex: dans une pipeline non interactive)."

if [ -z "$EMAIL" ]; then
  prompt "E-mail pour Let's Encrypt : "
  EMAIL="$REPLY"
fi
[ -n "$EMAIL" ] || die "un e-mail est requis : relance avec --email <adresse> \
(voir la remarque ci-dessus sur l'invite interactive)."

DOMAIN_AUTH="auth.$DOMAIN"
DOMAIN_FORGEJO="git.$DOMAIN"
DOMAIN_DASHBOARD="app.$DOMAIN"
DOMAIN_API="api.$DOMAIN"

# Verifie que le DNS pointe deja vers ce serveur — un defi HTTP-01 echoue
# sinon, silencieusement en tache de fond cote cert-manager. N'arrete PAS
# le script (un DNS tout juste pose peut ne pas avoir encore propage),
# mais le dit clairement plutot que de laisser l'echec se decouvrir plus
# tard sans explication.
SERVER_IP="$(curl -fsSL https://api.ipify.org || true)"
if [ -n "$SERVER_IP" ] && command -v getent >/dev/null 2>&1; then
  RESOLVED_IP="$(getent hosts "$DOMAIN_AUTH" | awk '{print $1}' | head -n1 || true)"
  if [ -n "$RESOLVED_IP" ] && [ "$RESOLVED_IP" != "$SERVER_IP" ]; then
    warn "$DOMAIN_AUTH resout vers $RESOLVED_IP, pas l'IP de ce serveur \
($SERVER_IP) — verifie ton DNS si les certificats TLS echouent."
  elif [ -z "$RESOLVED_IP" ]; then
    warn "$DOMAIN_AUTH ne resout vers rien pour l'instant — assure-toi que \
les 4 sous-domaines ($DOMAIN_AUTH, $DOMAIN_FORGEJO, $DOMAIN_DASHBOARD, \
$DOMAIN_API) pointent vers $SERVER_IP (ou un wildcard *.$DOMAIN) avant \
que les certificats TLS ne puissent etre delivres."
  fi
fi

if [ "$OPENBAO_PRODUCTION" = "true" ]; then
  log "OpenBao : mode production (devMode=false) — 'bao operator init'/'unseal' \
resteront a derouler manuellement (docs/admin-guide.md)."
else
  warn "OpenBao demarre en mode DEV (devMode=true, defaut low-cost) : AUCUNE \
persistance — un redemarrage du pod OpenBao perd tous les secrets deja \
distribues aux Workshops (identifiants git/session, cles LiteLLM). \
Relance ce script avec --openbao-production pour la persistance reelle \
(cerémonie d'initialisation manuelle requise, voir docs/admin-guide.md)."
fi

# --------------------------------------------------------------------------
# 3. k3s (Traefik desactive : le chart cible ingress-nginx, voir la spec §3.1)
# --------------------------------------------------------------------------
mkdir -p "$INSTALL_DIR"
if ! command -v k3s >/dev/null 2>&1; then
  log "installation de k3s..."
  curl -sfL https://get.k3s.io | INSTALL_K3S_EXEC="--disable=traefik" sh -
else
  log "k3s deja installe, reutilise."
fi
export KUBECONFIG=/etc/rancher/k3s/k3s.yaml
log "attente du noeud k3s..."
for _ in $(seq 1 60); do
  kubectl get nodes >/dev/null 2>&1 && break
  sleep 5
done
kubectl wait --for=condition=Ready node --all --timeout=300s

# --------------------------------------------------------------------------
# 4. Helm
# --------------------------------------------------------------------------
if ! command -v helm >/dev/null 2>&1; then
  log "installation de Helm..."
  curl -fsSL https://raw.githubusercontent.com/helm/helm/main/scripts/get-helm-3 | bash
else
  log "Helm deja installe, reutilise."
fi

# --------------------------------------------------------------------------
# 5. ingress-nginx + cert-manager
# --------------------------------------------------------------------------
helm repo add ingress-nginx https://kubernetes.github.io/ingress-nginx >/dev/null 2>&1 || true
helm repo add jetstack https://charts.jetstack.io >/dev/null 2>&1 || true
helm repo update >/dev/null

log "ingress-nginx..."
helm upgrade --install ingress-nginx ingress-nginx/ingress-nginx \
  --version "$INGRESS_NGINX_CHART_VERSION" \
  --namespace ingress-nginx --create-namespace \
  --set controller.ingressClassResource.default=true \
  --wait --timeout 5m

log "cert-manager..."
helm upgrade --install cert-manager jetstack/cert-manager \
  --version "$CERT_MANAGER_CHART_VERSION" \
  --namespace cert-manager --create-namespace \
  --set crds.enabled=true \
  --wait --timeout 5m

log "ClusterIssuer $CLUSTER_ISSUER..."
cat <<EOF | kubectl apply -f -
apiVersion: cert-manager.io/v1
kind: ClusterIssuer
metadata:
  name: $CLUSTER_ISSUER
spec:
  acme:
    server: https://acme-v02.api.letsencrypt.org/directory
    email: $EMAIL
    privateKeySecretRef:
      name: $CLUSTER_ISSUER-account-key
    solvers:
      - http01:
          ingress:
            ingressClassName: nginx
EOF

# --------------------------------------------------------------------------
# 6. Depot Atelier (chart non publie separement — voir la spec, section 3.7)
# --------------------------------------------------------------------------
if [ -d "$SRC_DIR/.git" ]; then
  log "depot Atelier deja clone, mise a jour..."
  git -C "$SRC_DIR" fetch --depth 1 origin main
  git -C "$SRC_DIR" reset --hard origin/main
else
  log "clonage du depot Atelier..."
  git clone --depth 1 "$REPO_URL" "$SRC_DIR"
fi

# --------------------------------------------------------------------------
# 7. Secrets generes + valeurs Helm (voir la spec, sections 3.3/3.4)
# --------------------------------------------------------------------------
if [ -f "$CREDENTIALS_FILE" ]; then
  log "identifiants deja generes ($CREDENTIALS_FILE), reutilises."
  # shellcheck disable=SC1090
  source "$CREDENTIALS_FILE"
  # Installation prealable a l'ajout de LITELLM_SALT_KEY (spec
  # docs/specs/11-admin-litellm-model-config.md §4.2) : la generer
  # maintenant plutot que de laisser une valeur vide passer pour
  # "configuree" cote chart (voir apiserver-deployment.yaml).
  if [ -z "${LITELLM_SALT_KEY:-}" ]; then
    log "LITELLM_SALT_KEY absente des identifiants existants, generation..."
    LITELLM_SALT_KEY="$(openssl rand -hex 24)"
    umask 077
    printf 'LITELLM_SALT_KEY="%s"\n' "$LITELLM_SALT_KEY" >> "$CREDENTIALS_FILE"
  fi
else
  log "generation des identifiants..."
  POSTGRES_ADMIN_PASSWORD="$(openssl rand -hex 24)"
  POSTGRES_MIGRATOR_PASSWORD="$(openssl rand -hex 24)"
  KEYCLOAK_ADMIN_PASSWORD="$(openssl rand -hex 24)"
  LITELLM_MASTER_KEY="$(openssl rand -hex 24)"
  LITELLM_SALT_KEY="$(openssl rand -hex 24)"
  umask 077
  cat > "$CREDENTIALS_FILE" <<EOF
# Genere par scripts/install.sh le $(date -u +%Y-%m-%dT%H:%M:%SZ) — a garder confidentiel.
POSTGRES_ADMIN_PASSWORD="$POSTGRES_ADMIN_PASSWORD"
POSTGRES_MIGRATOR_PASSWORD="$POSTGRES_MIGRATOR_PASSWORD"
KEYCLOAK_ADMIN_PASSWORD="$KEYCLOAK_ADMIN_PASSWORD"
LITELLM_MASTER_KEY="$LITELLM_MASTER_KEY"
LITELLM_SALT_KEY="$LITELLM_SALT_KEY"
EOF
  chmod 600 "$CREDENTIALS_FILE"
fi

umask 077
cat > "$VALUES_FILE" <<EOF
# Genere par scripts/install.sh — ne pas committer, ne pas partager.
domains:
  keycloak: "$DOMAIN_AUTH"
  forgejo: "$DOMAIN_FORGEJO"
  dashboard: "$DOMAIN_DASHBOARD"
  apiServer: "$DOMAIN_API"

ingress:
  className: "nginx"

tls:
  enabled: true
  certManager:
    enabled: true
    issuer: "$CLUSTER_ISSUER"
    issuerKind: "ClusterIssuer"

postgresql:
  auth:
    adminPassword: "$POSTGRES_ADMIN_PASSWORD"
    migratorPassword: "$POSTGRES_MIGRATOR_PASSWORD"

keycloak:
  auth:
    adminPassword: "$KEYCLOAK_ADMIN_PASSWORD"

litellm:
  masterKey: "$LITELLM_MASTER_KEY"
  saltKey: "$LITELLM_SALT_KEY"

openbao:
  devMode: $([ "$OPENBAO_PRODUCTION" = "true" ] && echo "false" || echo "true")
EOF

# --------------------------------------------------------------------------
# 8. Chart Atelier
# --------------------------------------------------------------------------
log "kubectl apply du CRD Workshop..."
kubectl apply -f "$SRC_DIR/crds/workshop.yaml"

log "helm upgrade --install atelier (idempotent)..."
helm upgrade --install atelier "$SRC_DIR/charts/atelier" \
  --namespace "$NAMESPACE" --create-namespace \
  -f "$VALUES_FILE" \
  --wait --timeout 10m || {
    warn "helm upgrade --install a echoue ou a depasse son delai — un \
CrashLoopBackOff transitoire de quelques minutes au premier demarrage est \
NORMAL (voir docs/admin-guide.md, section 6) tant que les Jobs \
d'initialisation tournent. Verifie 'kubectl get pods -n $NAMESPACE -w' \
avant de conclure a un echec reel."
  }

# --------------------------------------------------------------------------
# 9. Resume
# --------------------------------------------------------------------------
cat <<EOF

==================================================================
Installation Atelier terminee (ou en cours de stabilisation).

Dashboard   : https://$DOMAIN_DASHBOARD
API Server  : https://$DOMAIN_API
Keycloak    : https://$DOMAIN_AUTH
Forgejo     : https://$DOMAIN_FORGEJO

Identifiants generes : $CREDENTIALS_FILE (chmod 600, a conserver en lieu sur)

Verifier l'etat des pods : kubectl get pods -n $NAMESPACE -w
EOF

if [ "$OPENBAO_PRODUCTION" != "true" ]; then
  cat <<'EOF'

AVERTISSEMENT — OpenBao tourne en mode DEV (devMode=true) : aucune
persistance, tous les secrets de Workshop sont perdus a chaque
redemarrage du pod OpenBao. Relance ce script avec --openbao-production
pour la persistance reelle (ceremonie d'initialisation manuelle requise,
voir docs/admin-guide.md).
EOF
fi

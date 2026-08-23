#!/usr/bin/env bash
# Stack de developpement locale complete : kind + CRD + PKI locale +
# OpenBao + PostgreSQL + Keycloak + S3 (RustFS) + Forgejo + ingress
# Traefik + registre OCI + (optionnel) LLM Proxy + images `:dev` des
# composants qui tournent dans les pods Workshop (net-proxy,
# identity-proxy, mcp-gateway, vm-supervisor, image-builder). Idempotent :
# peut etre relance sans effet de bord si tout est deja en place.
#
# Remplace Kanidm (retire du plan, cf. docs/specs/PLAN-ACTION-GLOBAL.md
# section 9.0) par Keycloak, et integre les composants ajoutes au fil des
# sessions precedentes mais jamais orchestres ici : PostgreSQL, S3, Forgejo,
# la PKI locale et l'ingress Traefik.
#
# `controller` et `api-server` ne sont PAS deployes comme pods Kubernetes
# par ce script : ils tournent comme process locaux (`cargo run`), meme
# methode que celle deja validee tout au long de docs/PROGRESS.md.
# Consequence documentee dans deploy/dev/traefik/README.md : ils sont
# exposes aux pods du cluster (Traefik, entre autres) via un `Service` sans
# selecteur + un `Endpoints` manuel pointant sur la gateway Docker `kind`
# (172.19.0.1) — a mettre a jour si le reseau Docker `kind` est recree.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

KIND_CLUSTER="${KIND_CLUSTER:-atelier-dev}"
STACK_DIR="deploy/dev/local-stack"
ENV_FILE="$STACK_DIR/env.sh"
mkdir -p "$STACK_DIR"

PG_PASSWORD="dev-only-not-for-production"

log() { echo "==> $*"; }

port_open() {
  # Verifie qu'un port local repond deja (evite de relancer un
  # port-forward en double a chaque execution du script).
  (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null && exec 3>&- || return 1
}

# --- 1. kind + CRD ------------------------------------------------------
if ! kubectl config current-context >/dev/null 2>&1; then
  echo "aucun contexte kubectl actif — kind create cluster --name $KIND_CLUSTER" >&2
  exit 1
fi
log "CRD Workshop"
kubectl apply -f crds/workshop.yaml >/dev/null

# --- 2. PKI locale (deploy/dev/pki/) -------------------------------------
# Genere une Root CA + certificat multi-SAN dev-only, idempotent (le
# script ne regenere pas une CA deja presente sur cette machine). Pas
# encore consommee par l'entrypoint Traefik (HTTP simple pour l'instant,
# voir deploy/dev/traefik/README.md), mais deja utile pour faire confiance
# aux services qui exposent du TLS directement (Kanidm historiquement) et
# preparee pour le jour ou l'ingress passera en HTTPS.
log "PKI locale"
./deploy/dev/pki/init-pki.sh >/dev/null

# --- 3. OpenBao (deja un pod kind, cf. deploy/dev/openbao/) --------------
log "OpenBao"
kubectl apply -f deploy/dev/openbao/dev-pod.yaml >/dev/null
kubectl wait --for=condition=Ready pod/atelier-openbao-dev --timeout=60s >/dev/null

# Auth Kubernetes deja activee ? (idempotent, verifie avant d'ecrire)
if ! kubectl exec atelier-openbao-dev -- sh -c 'BAO_ADDR=http://127.0.0.1:8200 BAO_TOKEN=root bao auth list' 2>/dev/null | grep -q '^kubernetes/'; then
  log "OpenBao: activation de l'auth Kubernetes"
  kubectl exec atelier-openbao-dev -- sh -c '
    export BAO_ADDR=http://127.0.0.1:8200 BAO_TOKEN=root
    bao auth enable kubernetes
    bao write auth/kubernetes/config \
      kubernetes_host="https://kubernetes.default.svc" \
      token_reviewer_jwt=@/var/run/secrets/kubernetes.io/serviceaccount/token \
      kubernetes_ca_cert=@/var/run/secrets/kubernetes.io/serviceaccount/ca.crt
  ' >/dev/null
fi

# Port-forward vers le host (le Service est NodePort, pas mappe sur le
# host par kind par defaut) : reutilise s'il tourne deja.
if ! port_open 8200; then
  log "OpenBao: demarrage du port-forward (127.0.0.1:8200)"
  nohup kubectl port-forward svc/atelier-openbao-dev 8200:8200 >/tmp/atelier-openbao-port-forward.log 2>&1 &
  disown
  for _ in $(seq 1 20); do
    curl -s --max-time 1 http://127.0.0.1:8200/v1/sys/health >/dev/null 2>&1 && break
    sleep 0.5
  done
fi

# --- 4. PostgreSQL (deploy/dev/postgres/) --------------------------------
log "PostgreSQL"
kubectl apply -f deploy/dev/postgres/dev-pod.yaml >/dev/null
kubectl wait --for=condition=Ready pod/atelier-postgres-dev --timeout=60s >/dev/null

pg_db_exists() {
  kubectl exec atelier-postgres-dev -- psql -U atelier_admin -d postgres -tAc \
    "SELECT 1 FROM pg_database WHERE datname='$1'" 2>/dev/null | grep -q 1
}
pg_ensure_db() {
  if pg_db_exists "$1"; then
    log "PostgreSQL: base '$1' deja presente"
  else
    log "PostgreSQL: creation de la base '$1'"
    kubectl exec atelier-postgres-dev -- psql -U atelier_admin -d postgres -c "CREATE DATABASE $1;" >/dev/null
  fi
}
# atelier_apiserver est deja creee automatiquement (POSTGRES_DB) ; les
# autres bases par composant sont a la main (une seule base geree
# nativement par l'image officielle via cette variable, cf. README.md).
pg_ensure_db atelier_controller
pg_ensure_db keycloak
pg_ensure_db forgejo

if ! port_open 5433; then
  log "PostgreSQL: demarrage du port-forward (127.0.0.1:5433, le 5432 par defaut est deja pris sur cette machine)"
  nohup kubectl port-forward svc/atelier-postgres-dev 5433:5432 >/tmp/atelier-postgres-port-forward.log 2>&1 &
  disown
  for _ in $(seq 1 20); do
    port_open 5433 && break
    sleep 0.5
  done
fi

# --- 5. Keycloak (deploy/dev/keycloak/) — remplace Kanidm ----------------
log "Keycloak"
kubectl create configmap atelier-keycloak-realm \
  --from-file=atelier-realm.json=deploy/dev/keycloak/realm-export.json \
  --dry-run=client -o yaml | kubectl apply -f - >/dev/null
kubectl apply -f deploy/dev/keycloak/dev-pod.yaml >/dev/null
kubectl wait --for=condition=Ready pod/atelier-keycloak-dev --timeout=120s >/dev/null

# --- 6. S3 / RustFS (deploy/dev/s3/) --------------------------------------
log "S3 (RustFS)"
kubectl apply -f deploy/dev/s3/dev-pod.yaml >/dev/null
kubectl wait --for=condition=Ready pod/atelier-s3-dev --timeout=60s >/dev/null

if ! port_open 9000; then
  log "S3: demarrage du port-forward (127.0.0.1:9000)"
  nohup kubectl port-forward svc/atelier-s3-dev 9000:9000 >/tmp/atelier-s3-port-forward.log 2>&1 &
  disown
  for _ in $(seq 1 20); do
    port_open 9000 && break
    sleep 0.5
  done
fi

# --- 7. Forgejo (deploy/dev/forgejo/) -------------------------------------
log "Forgejo"
kubectl apply -f deploy/dev/forgejo/dev-pod.yaml >/dev/null
kubectl wait --for=condition=Ready pod/atelier-forgejo-dev --timeout=60s >/dev/null
# La base 'forgejo' vient d'etre creee/verifiee a l'etape 4 : si le pod
# tournait deja avant que la base existe (deja constate en pratique sur ce
# cluster), il faut le relancer pour qu'il execute ses migrations.
if kubectl logs atelier-forgejo-dev --tail=5 2>/dev/null | grep -q 'database "forgejo" does not exist'; then
  log "Forgejo: redemarrage (la base 'forgejo' vient d'etre creee, le pod tournait avant)"
  kubectl delete pod atelier-forgejo-dev --wait=true >/dev/null
  kubectl apply -f deploy/dev/forgejo/dev-pod.yaml >/dev/null
  kubectl wait --for=condition=Ready pod/atelier-forgejo-dev --timeout=60s >/dev/null
fi

FORGEJO_ADMIN_TOKEN=""
if [ -f "$ENV_FILE" ]; then
  FORGEJO_ADMIN_TOKEN=$(grep '^export ATELIER_FORGEJO_ADMIN_TOKEN=' "$ENV_FILE" 2>/dev/null | cut -d= -f2- | tr -d '"')
fi
if [ -n "$FORGEJO_ADMIN_TOKEN" ]; then
  log "Forgejo: administrateur + token deja generes, reutilise ($ENV_FILE)"
else
  log "Forgejo: creation de l'administrateur de test"
  # Juste apres un (re)demarrage du pod (cf. ci-dessus), le serveur web
  # Forgejo met quelques secondes a terminer ses migrations avant
  # d'accepter des commandes admin, alors meme que le pod est deja au
  # statut Ready (aucune readiness probe applicative definie dans
  # dev-pod.yaml) — on retente donc plutot que d'echouer silencieusement.
  created=false
  for _ in $(seq 1 30); do
    out=$(kubectl exec atelier-forgejo-dev -- su-exec 1000:1000 forgejo admin user create \
      --username atelier_admin \
      --password dev-only-not-for-production \
      --email admin@atelier.local \
      --admin 2>&1) && created=true && break
    echo "$out" | grep -qi "already exists" && created=true && break
    sleep 1
  done
  if [ "$created" != true ]; then
    echo "Forgejo: echec de la creation de l'administrateur apres 30 tentatives — $out" >&2
    exit 1
  fi
  log "Forgejo: generation du token API"
  FORGEJO_ADMIN_TOKEN=$(kubectl exec atelier-forgejo-dev -- su-exec 1000:1000 forgejo admin user generate-access-token \
    --username atelier_admin \
    --token-name "dev-local-stack-$(date +%s)" \
    --scopes all 2>/dev/null | grep -oP 'Access token was successfully created: \K.*' || true)
fi

# --- 8. Traefik (ingress de dev, deploy/dev/traefik/) --------------------
# Applique apres Keycloak/Forgejo : les Ingress referencent leurs Service.
log "Traefik (ingress de dev)"
kubectl apply -f deploy/dev/traefik/dev-traefik.yaml >/dev/null
kubectl wait --for=condition=Available deployment/atelier-traefik-dev --timeout=60s >/dev/null
kubectl apply -f deploy/dev/traefik/ingresses.yaml >/dev/null

# --- 9. Registre OCI (conteneur Docker, rejoint le reseau kind pour etre
#        joignable depuis les pods, cf. docs/PROGRESS.md "Reseau kind ↔
#        registre") ------------------------------------------------------
log "Registre OCI"
docker start atelier-registry-dev >/dev/null 2>&1 || true
docker network connect kind atelier-registry-dev --alias atelier-registry-dev >/dev/null 2>&1 || true

# --- 10. Images `:dev` des composants qui tournent dans les pods Workshop
# ------------------------------------------------------------------------
log "Build + kind load des images :dev (net-proxy, identity-proxy, mcp-gateway, vm-supervisor, image-builder)"
for component in net-proxy identity-proxy mcp-gateway vm-supervisor image-builder; do
  docker build -q -t "atelier-$component:dev" -f "crates/$component/Dockerfile" . >/dev/null
  kind load docker-image "atelier-$component:dev" --name "$KIND_CLUSTER" >/dev/null
done

# --- 11. LLM Proxy (optionnel — voir deploy/dev/llm-proxy/README.md) -----
LLM_PROXY_ADDR=""
LLM_PROXY_AUTH_TOKEN=""
if [ -n "${DEEPSEEK_API_KEY:-}" ] || [ -n "${ANTHROPIC_API_KEY:-}" ]; then
  log "LLM Proxy (LiteLLM)"
  kubectl create configmap atelier-llm-proxy-config \
    --from-file=config.yaml=deploy/dev/llm-proxy/config.yaml \
    --dry-run=client -o yaml | kubectl apply -f - >/dev/null
  LLM_PROXY_AUTH_TOKEN="${LITELLM_MASTER_KEY:-sk-atelier-llm-proxy-dev}"
  kubectl create secret generic atelier-llm-proxy-dev \
    --from-literal=DEEPSEEK_API_KEY="${DEEPSEEK_API_KEY:-unset}" \
    --from-literal=ANTHROPIC_API_KEY="${ANTHROPIC_API_KEY:-unset}" \
    --from-literal=LITELLM_MASTER_KEY="$LLM_PROXY_AUTH_TOKEN" \
    --dry-run=client -o yaml | kubectl apply -f - >/dev/null
  kubectl apply -f deploy/dev/llm-proxy/dev-deployment.yaml >/dev/null
  kubectl wait --for=condition=Available deployment/atelier-llm-proxy --timeout=90s >/dev/null
  LLM_PROXY_ADDR="atelier-llm-proxy.default.svc.cluster.local:4000"
else
  log "LLM Proxy: ignore (DEEPSEEK_API_KEY/ANTHROPIC_API_KEY absentes)"
fi

# --- 12. Redis (Jalon M5, pas encore d'infra de dev) ----------------------
log "Redis: pas encore d'infra de dev (Jalon M5, verrouille [ ] dans docs/specs/PLAN-ACTION-GLOBAL.md — non fait par ce script, ne pas en inventer une)"

# --- 13. Fichier d'environnement pour `cargo run` / `npm run dev` --------
# `controller` et `api-server` utilisent tous deux `DATABASE_URL` mais sur
# des bases distinctes (isolation par base, cf. deploy/dev/postgres/README.md)
# : deux variables dediees ici, a affecter explicitement a `DATABASE_URL`
# dans chaque terminal (voir le message final).
cat > "$ENV_FILE" <<EOF
# Genere par $0 — a sourcer avant de lancer controller/api-server/dashboard
# en local. Ne pas commiter (voir .gitignore).
export ATELIER_NAMESPACE=default
export OPENBAO_ADDR=http://127.0.0.1:8200
export OPENBAO_TOKEN=root

# PostgreSQL — une base par composant, meme instance (port-forward 5433
# cote hote, le 5432 par defaut est deja pris sur cette machine).
export ATELIER_DATABASE_URL_API_SERVER="postgres://atelier_admin:${PG_PASSWORD}@127.0.0.1:5433/atelier_apiserver"
export ATELIER_DATABASE_URL_CONTROLLER="postgres://atelier_admin:${PG_PASSWORD}@127.0.0.1:5433/atelier_controller"

# Keycloak (remplace Kanidm) — via l'ingress Traefik, meme nom d'hote que
# celui utilise au moment du login par le dashboard : le JWKS/issuer doit
# correspondre exactement a la claim "iss" des vrais JWT emis, voir
# deploy/dev/keycloak/README.md et docs/PROGRESS.md (mapper d'audience
# "atelier-api"). Necessite /etc/hosts a jour, voir le message final.
export ATELIER_OIDC_ISSUER_URL=http://auth.atelier.local/realms/atelier
export ATELIER_JWT_ISSUER=http://auth.atelier.local/realms/atelier
export ATELIER_JWT_JWKS_URL=http://auth.atelier.local/realms/atelier/protocol/openid-connect/certs
export ATELIER_JWT_AUDIENCE=atelier-api
export ATELIER_OAUTH2_CLIENT_ID=atelier-dashboard

# Registre OCI d'images :dev des composants Workshop.
export ATELIER_REGISTRY_ADDR="atelier-registry-dev:5000"
export ATELIER_REGISTRY_INSECURE=true

# S3 (RustFS) — port-forward 9000 cote hote.
export S3_ENDPOINT="http://127.0.0.1:9000"
export S3_REGION="us-east-1"
export AWS_ACCESS_KEY_ID="atelier-rustfs-access-key"
export AWS_SECRET_ACCESS_KEY="atelier-rustfs-secret-key"
export S3_BUCKET_SESSIONS="atelier-sessions"
export S3_BUCKET_SNAPSHOTS="atelier-snapshots"
export S3_FORCE_PATH_STYLE="true"

# Forgejo — via l'ingress Traefik (necessite /etc/hosts a jour).
export ATELIER_FORGEJO_URL=http://git.atelier.local
export ATELIER_FORGEJO_ADMIN_TOKEN="$FORGEJO_ADMIN_TOKEN"

# LLM Proxy (optionnel, voir deploy/dev/llm-proxy/README.md).
export ATELIER_LLM_PROXY_ADDR="$LLM_PROXY_ADDR"
export ATELIER_LLM_PROXY_AUTH_TOKEN="$LLM_PROXY_AUTH_TOKEN"

# api-server/dashboard, via l'ingress Traefik (necessite /etc/hosts a jour).
export ATELIER_API_SERVER_URL=http://api.atelier.local
EOF

log "Termine. Pour lancer la stack :"
cat <<EOF

  source $ENV_FILE
  DATABASE_URL="\$ATELIER_DATABASE_URL_CONTROLLER" cargo run --bin atelier-controller   # terminal 1
  DATABASE_URL="\$ATELIER_DATABASE_URL_API_SERVER" cargo run -p atelier-api-server      # terminal 2
  cd dashboard && npm run dev                                                          # terminal 3

Dashboard sur http://app.atelier.local (port 80 via l'ingress Traefik). Voir
deploy/dev/local-stack/README.md pour la limite assumee (port-forward/
"Ouvrir VS Code" indisponibles dans cette configuration precise) et
comment la lever.

Etapes manuelles restantes (non automatisables depuis ce script) :

  1. Resolution DNS locale des 4 domaines de dev (necessite sudo, ce
     script ne peut pas le faire lui-meme sans bloquer en attente d'un
     mot de passe) :

       sudo deploy/dev/traefik/update-hosts.sh

  2. (Optionnel) faire confiance a la CA locale pour vos outils (curl,
     Node.js, AWS CLI...) sans avertissement TLS — voir
     deploy/dev/pki/README.md :

       export SSL_CERT_FILE="$ROOT_DIR/deploy/dev/pki/ca/atelier-ca.crt"
       export NODE_EXTRA_CA_CERTS="$ROOT_DIR/deploy/dev/pki/ca/atelier-ca.crt"

  3. Redis : pas encore d'infra de dev (Jalon M5, voir
     docs/specs/PLAN-ACTION-GLOBAL.md).
EOF

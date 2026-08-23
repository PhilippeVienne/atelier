#!/usr/bin/env bash
# Stack de developpement locale complete : kind + CRD + OpenBao + Kanidm +
# registre OCI + (optionnel) LLM Proxy + images `:dev` des composants qui
# tournent dans les pods Workshop (net-proxy, identity-proxy, mcp-gateway,
# vm-supervisor, image-builder). Idempotent : peut etre relance sans effet
# de bord si tout est deja en place.
#
# `controller` et `api-server` ne sont PAS deployes comme pods Kubernetes
# par ce script : ils tournent comme process locaux (`cargo run`), meme
# methode que celle deja validee tout au long de docs/PROGRESS.md — evite
# la complexite de faire resoudre "localhost:8443" (Kanidm, dont le
# certificat TLS n'est valide que pour ce nom precis) depuis l'interieur
# d'un pod. Limite assumee, documentee dans le README a cote de ce script :
# les fonctionnalites d'`api-server` qui doivent joindre une IP de pod
# directement (port-forward, pont "Ouvrir VS Code") ne fonctionnent pas
# dans cette configuration precise (le host ne route pas vers les IP de
# pods kind) — utilisable seulement une fois `api-server` lui-meme
# containerise avec `--network container:<noeud-kind>`, cf. README.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

KIND_CLUSTER="${KIND_CLUSTER:-atelier-dev}"
STACK_DIR="deploy/dev/local-stack"
ENV_FILE="$STACK_DIR/env.sh"
mkdir -p "$STACK_DIR"

log() { echo "==> $*"; }

# --- 1. kind + CRD ----------------------------------------------------
if ! kubectl config current-context >/dev/null 2>&1; then
  echo "aucun contexte kubectl actif — kind create cluster --name $KIND_CLUSTER" >&2
  exit 1
fi
log "CRD Workshop"
kubectl apply -f crds/workshop.yaml >/dev/null

# --- 2. OpenBao (deja un pod kind, cf. deploy/dev/openbao/) ------------
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
if ! curl -s --max-time 1 http://127.0.0.1:8200/v1/sys/health >/dev/null 2>&1; then
  log "OpenBao: demarrage du port-forward (127.0.0.1:8200)"
  nohup kubectl port-forward svc/atelier-openbao-dev 8200:8200 >/tmp/atelier-openbao-port-forward.log 2>&1 &
  disown
  for _ in $(seq 1 20); do
    curl -s --max-time 1 http://127.0.0.1:8200/v1/sys/health >/dev/null 2>&1 && break
    sleep 0.5
  done
fi

# --- 3. Kanidm (conteneur Docker independant du cluster) --------------
log "Kanidm"
KANIDM_DIR="deploy/dev/kanidm"
if [ ! -f "$KANIDM_DIR/data/ca.pem" ]; then
  echo "Kanidm jamais initialise sur cette machine — suis d'abord $KANIDM_DIR/README.md (etapes 1-4) puis relance ce script." >&2
  exit 1
fi
docker start atelier-kanidm-dev >/dev/null 2>&1 || true
for _ in $(seq 1 20); do
  curl -sk --max-time 1 https://localhost:8443/status >/dev/null 2>&1 && break
  sleep 0.5
done

# --- 4. Registre OCI (conteneur Docker, rejoint le reseau kind pour etre
#        joignable depuis les pods, cf. docs/PROGRESS.md "Reseau kind ↔
#        registre") ------------------------------------------------------
log "Registre OCI"
docker start atelier-registry-dev >/dev/null 2>&1 || true
docker network connect kind atelier-registry-dev --alias atelier-registry-dev >/dev/null 2>&1 || true
REGISTRY_IP=$(docker inspect atelier-registry-dev --format '{{.NetworkSettings.Networks.kind.IPAddress}}')

# --- 5. Images `:dev` des composants qui tournent dans les pods Workshop
# ------------------------------------------------------------------------
log "Build + kind load des images :dev (net-proxy, identity-proxy, mcp-gateway, vm-supervisor, image-builder)"
for component in net-proxy identity-proxy mcp-gateway vm-supervisor image-builder; do
  docker build -q -t "atelier-$component:dev" -f "crates/$component/Dockerfile" . >/dev/null
  kind load docker-image "atelier-$component:dev" --name "$KIND_CLUSTER" >/dev/null
done

# --- 6. LLM Proxy (optionnel — voir deploy/dev/llm-proxy/README.md) -----
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

# --- 7. Kanidm : role + token API pour le controller (une seule fois —
#        un token API Kanidm n'est affiche qu'a sa creation, reutilise
#        celui deja genere si present dans env.sh) -----------------------
if [ -f "$ENV_FILE" ] && grep -q '^export KANIDM_API_TOKEN=' "$ENV_FILE"; then
  log "Kanidm: token API du controller deja genere, reutilise ($ENV_FILE)"
  KANIDM_API_TOKEN=$(grep '^export KANIDM_API_TOKEN=' "$ENV_FILE" | cut -d= -f2- | tr -d '"')
else
  log "Kanidm: (re)generation du service account + token API du controller"
  IDM_ADMIN_PW=$(docker exec atelier-kanidm-dev kanidmd recover-account -c /data/server.toml idm_admin 2>&1 \
    | grep -oP 'new_password: "\K[^"]+')
  docker run --rm --network host \
    -v "$ROOT_DIR/$KANIDM_DIR/data/ca.pem:/data/ca.pem:ro" \
    -e KANIDM_URL=https://localhost:8443 -e KANIDM_CA_PATH=/data/ca.pem \
    --entrypoint sh kanidm/tools:latest -c "
      kanidm login --name idm_admin -p '$IDM_ADMIN_PW'
      kanidm service-account create atelier-controller 'Atelier Controller' idm_admin --name idm_admin 2>/dev/null || true
      kanidm group add-members idm_admins atelier-controller --name idm_admin
    " >/dev/null
  KANIDM_API_TOKEN=$(docker run --rm --network host \
    -v "$ROOT_DIR/$KANIDM_DIR/data/ca.pem:/data/ca.pem:ro" \
    -e KANIDM_URL=https://localhost:8443 -e KANIDM_CA_PATH=/data/ca.pem \
    --entrypoint sh kanidm/tools:latest -c "
      kanidm login --name idm_admin -p '$IDM_ADMIN_PW' >/dev/null
      kanidm service-account api-token generate atelier-controller atelier-controller-local-stack --readwrite --name idm_admin
    " | tail -1)
fi

# --- 8. Fichier d'environnement pour `cargo run` / `npm run dev` -------
cat > "$ENV_FILE" <<EOF
# Genere par $0 — a sourcer avant de lancer controller/api-server/dashboard
# en local. Ne pas commiter (voir .gitignore).
export ATELIER_NAMESPACE=default
export OPENBAO_ADDR=http://127.0.0.1:8200
export OPENBAO_TOKEN=root
export KANIDM_URL=https://localhost:8443
export KANIDM_CA_PATH="$ROOT_DIR/$KANIDM_DIR/data/ca.pem"
export KANIDM_API_TOKEN="$KANIDM_API_TOKEN"
export ATELIER_REGISTRY_ADDR="atelier-registry-dev:5000"
export ATELIER_REGISTRY_INSECURE=true
export ATELIER_JWT_ISSUER=https://localhost:8443/oauth2/openid/atelier
export ATELIER_JWT_JWKS_URL=https://localhost:8443/oauth2/openid/atelier/public_key.jwk
export ATELIER_JWT_AUDIENCE=atelier
export ATELIER_JWT_CA_PATH="$ROOT_DIR/$KANIDM_DIR/data/ca.pem"
export ATELIER_LLM_PROXY_ADDR="$LLM_PROXY_ADDR"
export ATELIER_LLM_PROXY_AUTH_TOKEN="$LLM_PROXY_AUTH_TOKEN"
export ATELIER_API_SERVER_URL=http://localhost:8080
export ATELIER_KANIDM_URL=https://localhost:8443
export ATELIER_OAUTH2_CLIENT_ID=atelier
EOF

log "Termine. Pour lancer la stack :"
cat <<EOF

  source $ENV_FILE
  cargo run --bin atelier-controller  # terminal 1
  cargo run -p atelier-api-server     # terminal 2 (aussi besoin de \$KANIDM_CA_PATH etc, deja dans env.sh)
  cd dashboard && npm run dev         # terminal 3

Dashboard sur http://localhost:3000. Voir deploy/dev/local-stack/README.md
pour la limite assumee (port-forward/"Ouvrir VS Code" indisponibles dans
cette configuration precise) et comment la lever.
EOF

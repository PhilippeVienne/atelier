#!/usr/bin/env bash
# Detruit proprement toutes les ressources creees par ./local-stack.sh :
# CRD Workshop EXCLUE (elle porte les vrais Workshops du cluster, jamais
# touchee ici), OpenBao, PostgreSQL, Keycloak, S3 (RustFS), Forgejo,
# Traefik (+ ses Ingress/RBAC), LLM Proxy (si deploye) et le registre OCI
# (conteneur Docker arrete, PAS supprime — voir "Registre OCI" ci-dessous).
#
# Symetrique de local-stack.sh : chaque bloc de suppression cible les
# memes fichiers manifest (kubectl delete -f, --ignore-not-found) que ceux
# appliques par local-stack.sh, jamais un selecteur de label large ni un
# "delete --all" — pour ne prendre aucun risque avec les vrais Workshops
# du cluster (pods "*-parent"/"*-image-build", ressources Workshop) qui ne
# sont JAMAIS references par ce script.
#
# ATTENTION — cluster de dev partage : supprimer OpenBao (secrets vivants
# consommes par net-proxy/identity-proxy/api-server des Workshops REELS en
# cours d'execution) et/ou PostgreSQL/Keycloak (session dev active d'un
# autre developpeur) casse immediatement ce qui tourne, sans toucher aux
# pods Workshop eux-memes. Confirmation explicite requise avant toute
# suppression (variable d'environnement CONFIRM=yes ou reponse "yes" au
# prompt interactif) : ce script ne s'execute jamais "en silence".
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

log() { echo "==> $*"; }

if [ "${CONFIRM:-}" != "yes" ]; then
  cat <<'EOF' >&2
Ce script detruit OpenBao/PostgreSQL/Keycloak/S3/Forgejo/Traefik/LLM Proxy
de dev sur le cluster kind actuellement configure (contexte kubectl
courant). Si d'autres process locaux (controller/api-server/dashboard) ou
de vrais Workshops en cours d'execution en dependent, ils cesseront de
fonctionner immediatement (les Workshops eux-memes ne sont jamais
supprimes, mais leur chaine d'authentification OpenBao/Keycloak le sera).

Relancer avec CONFIRM=yes pour executer reellement :

  CONFIRM=yes ./deploy/dev/teardown-stack.sh
EOF
  exit 1
fi

if ! kubectl config current-context >/dev/null 2>&1; then
  echo "aucun contexte kubectl actif — rien a detruire" >&2
  exit 1
fi
log "Contexte kubectl actif : $(kubectl config current-context)"

# --- 1. Traefik (ingress + Deployment + RBAC) ----------------------------
log "Traefik : Ingress + Deployment + RBAC"
kubectl delete -f deploy/dev/traefik/ingresses.yaml --ignore-not-found=true >/dev/null
kubectl delete -f deploy/dev/traefik/dev-traefik.yaml --ignore-not-found=true >/dev/null

# --- 2. LLM Proxy (optionnel, peut ne jamais avoir ete deploye) ----------
log "LLM Proxy (si deploye)"
kubectl delete -f deploy/dev/llm-proxy/dev-deployment.yaml --ignore-not-found=true >/dev/null
kubectl delete secret atelier-llm-proxy-dev --ignore-not-found=true >/dev/null
kubectl delete configmap atelier-llm-proxy-config --ignore-not-found=true >/dev/null

# --- 3. Forgejo -----------------------------------------------------------
log "Forgejo"
kubectl delete -f deploy/dev/forgejo/dev-pod.yaml --ignore-not-found=true >/dev/null

# --- 4. S3 (RustFS) --------------------------------------------------------
log "S3 (RustFS)"
kubectl delete -f deploy/dev/s3/dev-pod.yaml --ignore-not-found=true >/dev/null

# --- 5. Keycloak (+ ConfigMap du realm, pas gere par dev-pod.yaml) -------
log "Keycloak"
kubectl delete -f deploy/dev/keycloak/dev-pod.yaml --ignore-not-found=true >/dev/null
kubectl delete configmap atelier-keycloak-realm --ignore-not-found=true >/dev/null

# --- 6. PostgreSQL (emptyDir : toutes les bases dev sont perdues ici) ----
log "PostgreSQL"
kubectl delete -f deploy/dev/postgres/dev-pod.yaml --ignore-not-found=true >/dev/null

# --- 7. OpenBao (le plus sensible : secrets vivants des vrais Workshops) -
log "OpenBao"
kubectl delete -f deploy/dev/openbao/dev-pod.yaml --ignore-not-found=true >/dev/null

# --- 8. PKI locale : secrets TLS/CA synchronises dans le cluster ---------
# Les fichiers sur disque (deploy/dev/pki/ca/, deploy/dev/pki/certs/) ne
# sont PAS supprimes : ils sont utilises par des outils locaux (curl,
# Node.js...) independamment du cluster, et regenerer une CA changerait sa
# valeur de confiance pour rien.
log "PKI locale : secrets Kubernetes (CA/TLS)"
kubectl delete secret atelier-dev-tls atelier-dev-ca --ignore-not-found=true >/dev/null

# --- 9. Port-forwards locaux demarres par local-stack.sh -----------------
log "Port-forwards locaux (OpenBao 8200, PostgreSQL 5433, S3 9000)"
pkill -f "kubectl port-forward svc/atelier-openbao-dev" 2>/dev/null || true
pkill -f "kubectl port-forward svc/atelier-postgres-dev" 2>/dev/null || true
pkill -f "kubectl port-forward svc/atelier-s3-dev" 2>/dev/null || true

# --- 10. Registre OCI : arrete, PAS supprime ------------------------------
# Choix delibere : le registre est un conteneur Docker independant du
# cluster kind, potentiellement reutilise par de vrais Workshops (pull
# d'image au demarrage d'un pod parent, build d'image en cours). Le
# supprimer ferait perdre toutes les images :dev deja poussees (obligerait
# un rebuild complet au prochain ./local-stack.sh) et casserait un pull en
# cours. On se contente de l'arreter — `docker start atelier-registry-dev`
# (fait par local-stack.sh) suffit a le relancer sans perte de donnees
# (volume Docker nomme, pas un emptyDir).
log "Registre OCI : arret (pas de suppression, conserve les images :dev pour un prochain demarrage)"
docker stop atelier-registry-dev >/dev/null 2>&1 || true

# --- Volontairement PAS supprime par ce script ---------------------------
# - crds/workshop.yaml : porterait la suppression de TOUS les Workshops du
#   cluster, reels ou non — jamais touche ici.
# - Images Docker/kind "atelier-*:dev" : encore utilisees par les pods
#   Workshop reels en cours d'execution (net-proxy, identity-proxy,
#   vm-supervisor...) ; les supprimer casserait des sessions actives sans
#   aucun benefice (elles seront reconstruites de toute facon au prochain
#   ./local-stack.sh si besoin).
# - deploy/dev/kanidm (conteneur Docker "atelier-kanidm-dev") : composant
#   deja retire de local-stack.sh (remplace par Keycloak), pas du ressort
#   de ce teardown qui ne detruit que ce que local-stack.sh cree
#   aujourd'hui — a nettoyer separement si besoin.

log "Termine. deploy/dev/local-stack/env.sh n'est pas supprime (regenere au prochain ./local-stack.sh)."

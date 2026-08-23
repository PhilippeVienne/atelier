#!/usr/bin/env bash
# Met a jour /etc/hosts avec l'IP actuelle du node kind, pour les domaines
# de dev routes par l'ingress Traefik (voir README.md). Idempotent :
# remplace la ligne existante au lieu d'en ajouter une nouvelle a chaque
# execution. Necessite sudo (ecriture de /etc/hosts) — impossible a
# automatiser depuis l'interieur du cluster (un Job Kubernetes ne voit que
# le systeme de fichiers du node kind, un conteneur Docker isole de la
# vraie machine hote, jamais /etc/hosts de l'hote lui-meme).
set -euo pipefail

HOSTS=(auth.atelier.local git.atelier.local app.atelier.local api.atelier.local)
CONTAINER="${ATELIER_KIND_NODE:-atelier-dev-control-plane}"
MARKER="# atelier-dev-hosts"

ip=$(docker inspect "$CONTAINER" --format '{{(index .NetworkSettings.Networks.kind).IPAddress}}')
if [[ -z "$ip" ]]; then
  echo "impossible de determiner l'IP du node kind ($CONTAINER)" >&2
  exit 1
fi

line="$ip ${HOSTS[*]} $MARKER"

if [[ $EUID -ne 0 ]]; then
  echo "ce script doit etre execute avec sudo (ecriture de /etc/hosts)" >&2
  echo "  sudo $0" >&2
  exit 1
fi

# Retire toute ligne precedente posee par ce script (identifiee par le
# marqueur) OU une ligne manuelle anterieure (identifiee par
# "atelier.local" tout court, ex: celle posee a la main avant ce script),
# puis ajoute la ligne a jour — jamais de doublon meme apres plusieurs
# executions (ex: cluster kind recree, IP changee).
sed -i "/$MARKER/d; /atelier\.local/d" /etc/hosts
echo "$line" >> /etc/hosts

echo "/etc/hosts mis a jour : $line"

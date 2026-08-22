#!/bin/sh
# Sert un statut ("OK"/"FAIL") sur le port 9999, en boucle, resultat d'une
# vraie requete HTTPS sortante faite au demarrage. curl lit HTTP_PROXY/
# HTTPS_PROXY depuis l'environnement du process (fourni par systemd via
# EnvironmentFile=/etc/environment, cf. le fichier .service a cote) : si ces
# variables sont absentes, curl tente une connexion directe, bloquee par les
# regles iptables de vm-supervisor (restrict_to_net_proxy) -> timeout -> FAIL.
RESULT="FAIL"
if curl -sf --max-time 8 https://example.com/ -o /dev/null; then
  RESULT="OK"
fi
while true; do
  printf '%s\n' "$RESULT" | nc -l -p 9999 -q 1 || true
done

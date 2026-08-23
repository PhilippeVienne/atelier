#!/usr/bin/env bash
# Initialisation de la PKI de developpement local Atelier.
# Genere une Autorite de Certification (Root CA) locale et un certificat serveur Multi-SAN.
# Cree automatiquement les Secrets TLS dans le cluster Kind.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CA_DIR="$SCRIPT_DIR/ca"
CERTS_DIR="$SCRIPT_DIR/certs"
mkdir -p "$CA_DIR" "$CERTS_DIR"

log() { echo "==> [PKI Dev] $*"; }

# 1. Autorite de Certification Racine (Root CA)
if [ ! -f "$CA_DIR/atelier-ca.key" ] || [ ! -f "$CA_DIR/atelier-ca.crt" ]; then
  log "Generation de la Root CA locale (atelier-ca)"
  openssl genrsa -out "$CA_DIR/atelier-ca.key" 4096 2>/dev/null
  openssl req -x509 -new -nodes -key "$CA_DIR/atelier-ca.key" -sha256 -days 3650 \
    -subj "/C=FR/ST=IDF/L=Paris/O=Atelier Dev/OU=PKI/CN=Atelier Dev Root CA" \
    -out "$CA_DIR/atelier-ca.crt"
else
  log "Root CA existante conservee ($CA_DIR/atelier-ca.crt)"
fi

# 2. Certificat Serveur Multi-SAN (Wildcard + Domaines dev + IPs)
log "Generation du certificat serveur Multi-SAN"
openssl genrsa -out "$CERTS_DIR/server.key" 2048 2>/dev/null

SAN_CONFIG="$CERTS_DIR/san.cnf"
cat << 'CONF' > "$SAN_CONFIG"
[req]
default_bits = 2048
prompt = no
default_md = sha256
req_extensions = req_ext
distinguished_name = dn

[dn]
C = FR
ST = IDF
L = Paris
O = Atelier Dev
OU = Core
CN = *.atelier.local

[req_ext]
subjectAltName = @alt_names

[alt_names]
DNS.1 = *.atelier.local
DNS.2 = atelier.local
DNS.3 = auth.atelier.local
DNS.4 = git.atelier.local
DNS.5 = app.atelier.local
DNS.6 = api.atelier.local
DNS.7 = *.127.0.0.1.nip.io
DNS.8 = 127.0.0.1.nip.io
DNS.9 = git.atelier.internal
DNS.10 = forgejo.atelier.internal
DNS.11 = localhost
DNS.12 = host.docker.internal
DNS.13 = atelier-keycloak-dev
DNS.14 = atelier-keycloak-dev.default
DNS.15 = atelier-keycloak-dev.default.svc
DNS.16 = atelier-keycloak-dev.default.svc.cluster.local
IP.1 = 127.0.0.1
IP.2 = 169.254.0.1
CONF

openssl req -new -key "$CERTS_DIR/server.key" -out "$CERTS_DIR/server.csr" -config "$SAN_CONFIG"

cat << 'EXT' > "$CERTS_DIR/v3.ext"
authorityKeyIdentifier=keyid,issuer
basicConstraints=CA:FALSE
keyUsage = digitalSignature, nonRepudiation, keyEncipherment, dataEncipherment
extendedKeyUsage = serverAuth, clientAuth
subjectAltName = @alt_names

[alt_names]
DNS.1 = *.atelier.local
DNS.2 = atelier.local
DNS.3 = auth.atelier.local
DNS.4 = git.atelier.local
DNS.5 = app.atelier.local
DNS.6 = api.atelier.local
DNS.7 = *.127.0.0.1.nip.io
DNS.8 = 127.0.0.1.nip.io
DNS.9 = git.atelier.internal
DNS.10 = forgejo.atelier.internal
DNS.11 = localhost
DNS.12 = host.docker.internal
DNS.13 = atelier-keycloak-dev
DNS.14 = atelier-keycloak-dev.default
DNS.15 = atelier-keycloak-dev.default.svc
DNS.16 = atelier-keycloak-dev.default.svc.cluster.local
IP.1 = 127.0.0.1
IP.2 = 169.254.0.1
EXT

openssl x509 -req -in "$CERTS_DIR/server.csr" -CA "$CA_DIR/atelier-ca.crt" -CAkey "$CA_DIR/atelier-ca.key" \
  -CAcreateserial -out "$CERTS_DIR/server.crt" -days 825 -sha256 -extfile "$CERTS_DIR/v3.ext" 2>/dev/null

cat "$CERTS_DIR/server.crt" "$CA_DIR/atelier-ca.crt" > "$CERTS_DIR/server-bundle.crt"
rm -f "$CERTS_DIR/server.csr" "$SAN_CONFIG" "$CERTS_DIR/v3.ext"

# 3. Synchronisation Kubernetes (Secret TLS & CA)
if kubectl config current-context >/dev/null 2>&1; then
  log "Synchronisation des secrets TLS et CA dans Kind"
  kubectl create secret tls atelier-dev-tls \
    --cert="$CERTS_DIR/server.crt" \
    --key="$CERTS_DIR/server.key" \
    --dry-run=client -o yaml | kubectl apply -f - >/dev/null

  kubectl create secret generic atelier-dev-ca \
    --from-file=ca.crt="$CA_DIR/atelier-ca.crt" \
    --dry-run=client -o yaml | kubectl apply -f - >/dev/null
fi

log "PKI prete avec succes !"
log "CA Root : $CA_DIR/atelier-ca.crt"
log "Certificat Serveur : $CERTS_DIR/server.crt"

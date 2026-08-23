# PKI de Développement Local Atelier

PKI locale validable permettant de sécuriser tous les flux TLS en environnement de développement (`Keycloak`, `Forgejo`, `api-server`, `dashboard`, `net-proxy`) sans avertissement de sécurité ni contournement (`insecure_skip_verify`).

## 1. Structure

- `ca/atelier-ca.crt` : Certificat racine (Root CA) valide 10 ans.
- `ca/atelier-ca.key` : Clé privée de la Root CA.
- `certs/server.crt` & `server.key` : Certificat serveur Multi-SAN couvrant :
  - `*.atelier.local`, `auth.atelier.local`, `git.atelier.local`, `app.atelier.local`, `api.atelier.local`
  - `git.atelier.internal`, `forgejo.atelier.internal`
  - `localhost`, `127.0.0.1`, `host.docker.internal`
  - Noms DNS de services Kubernetes internes (`atelier-keycloak-dev.default.svc`, etc.)

## 2. Secrets Kubernetes dans Kind

Le script `init-pki.sh` crée automatiquement dans le cluster Kind :
- `atelier-dev-tls` : Secret de type `kubernetes.io/tls` (`tls.crt`, `tls.key`).
- `atelier-dev-ca` : Secret contenant `ca.crt`.

## 3. Utilisation & Confiance Système

### Variables d'environnement pour vos outils locaux (Rust, Node.js, Python, AWS CLI) :

```sh
export SSL_CERT_FILE="$(pwd)/deploy/dev/pki/ca/atelier-ca.crt"
export NODE_EXTRA_CA_CERTS="$(pwd)/deploy/dev/pki/ca/atelier-ca.crt"
export REQUESTS_CA_BUNDLE="$(pwd)/deploy/dev/pki/ca/atelier-ca.crt"
export AWS_CA_BUNDLE="$(pwd)/deploy/dev/pki/ca/atelier-ca.crt"
```

### Résolution DNS locale (/etc/hosts) :

Pour résoudre confortablement les domaines `*.atelier.local` sur votre machine de développement :

```sh
echo "127.0.0.1 auth.atelier.local git.atelier.local app.atelier.local api.atelier.local" | sudo tee -a /etc/hosts
```

### Installation dans le magasin de confiance de l'OS (Optionnel) :

```sh
# Debian / Ubuntu
sudo cp deploy/dev/pki/ca/atelier-ca.crt /usr/local/share/ca-certificates/atelier-dev-ca.crt
sudo update-ca-certificates
```


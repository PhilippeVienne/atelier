# Keycloak (Fournisseur OIDC / IAM) de développement

Instance Keycloak en mode dev, déployée **dans** le cluster Kind et connectée à la base de données PostgreSQL partagée (`atelier-postgres-dev:5432/keycloak`).

## 1. Démarrage & Déploiement

```sh
# 1. Créer la base keycloak dans PostgreSQL si ce n'est pas déjà fait
kubectl exec atelier-postgres-dev -- psql -U atelier_admin -d postgres -c 'CREATE DATABASE keycloak;'

# 2. Déployer Keycloak avec le Realm "atelier" pré-importé
kubectl create configmap atelier-keycloak-realm --from-file=atelier-realm.json=deploy/dev/keycloak/realm-export.json --dry-run=client -o yaml | kubectl apply -f -
kubectl apply -f deploy/dev/keycloak/dev-pod.yaml
kubectl wait --for=condition=Ready pod/atelier-keycloak-dev --timeout=90s

# 3. Exposer Keycloak sur l'hôte (port 8090 : le 8080 est déjà pris par atelier-api-server)
kubectl port-forward svc/atelier-keycloak-dev 8090:8080 &
```

## 2. Configuration du Realm "atelier"

- **Endpoints OIDC** :
  - Discovery : `http://127.0.0.1:8090/realms/atelier/.well-known/openid-configuration`
  - Authorization : `http://127.0.0.1:8090/realms/atelier/protocol/openid-connect/auth`
  - Token : `http://127.0.0.1:8090/realms/atelier/protocol/openid-connect/token`
  - JWKS : `http://127.0.0.1:8090/realms/atelier/protocol/openid-connect/certs`
- **Clients OIDC Pré-configurés** :
  - `atelier-dashboard` : Client public avec PKCE (S256), Redirect URIs `http://localhost:3000/*`, `http://127.0.0.1:3000/*`, `http://app.atelier.local:3000/*`.
  - `atelier-api` : Bearer-only client / Resource Server.
- **Utilisateurs de Test** :
  - `atelier-admin` / `dev-only-not-for-production` (Rôle : `admin`)
  - `atelier-test-user` / `dev-only-not-for-production` (Rôle : `developer`)

## 3. Test d'Obtention de Token OIDC via CLI

```sh
curl -s -X POST http://127.0.0.1:8090/realms/atelier/protocol/openid-connect/token \
  -d "client_id=atelier-dashboard" \
  -d "grant_type=password" \
  -d "username=atelier-test-user" \
  -d "password=dev-only-not-for-production" \
  -d "scope=openid email profile" | jq .
```

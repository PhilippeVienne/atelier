# Forgejo (Forge Git 100% HTTPS) de développement

Instance Forgejo en mode dev, déployée **dans** le cluster Kind et connectée à l'instance partagée **PostgreSQL** (`atelier-postgres-dev`) : base `forgejo`, données isolées, SSH désactivé (100% HTTPS).

```sh
# 1. Créer la base forgejo dans PostgreSQL si ce n'est pas déjà fait
kubectl exec atelier-postgres-dev -- psql -U atelier_admin -d postgres -c 'CREATE DATABASE forgejo;'

# 2. Déployer le pod Forgejo
kubectl apply -f deploy/dev/forgejo/dev-pod.yaml
kubectl wait --for=condition=Ready pod/atelier-forgejo-dev --timeout=60s

# 3. Créer l'administrateur de test
kubectl exec atelier-forgejo-dev -- su-exec 1000:1000 forgejo admin user create \
  --username atelier_admin \
  --password dev-only-not-for-production \
  --email admin@atelier.local \
  --admin

# 4. Générer un token d'accès PAT pour les tests
kubectl exec atelier-forgejo-dev -- su-exec 1000:1000 forgejo admin user generate-access-token \
  --username atelier_admin \
  --token-name dev-test-token \
  --scopes all

# 5. Exposer Forgejo sur l'hôte (port 3000)
kubectl port-forward svc/atelier-forgejo-dev 3000:3000 &
```

## Caractéristiques d'Architecture

- **Base de Données Partagée** : Connecté à `atelier-postgres-dev:5432` sur la base `forgejo` (121 tables gérées par le schéma Forgejo).
- **100% HTTPS** : Le serveur SSH est désactivé (`DISABLE_SSH: true`). Tout le trafic passe par HTTP/HTTPS et est intercepté par `identity-proxy` pour l'injection transparente de tokens.
- **API REST standard Gitea/Forgejo** : Utilisée par `services/pm-engine` pour la gestion des tickets, des branches et des Pull Requests.

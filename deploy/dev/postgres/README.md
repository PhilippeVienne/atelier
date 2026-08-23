# PostgreSQL de developpement

Instance PostgreSQL en mode dev, deployee **dans** le cluster kind (meme
convention que `deploy/dev/kanidm` et `deploy/dev/openbao`) : pas de
persistance (`emptyDir`), donnees perdues a la suppression du pod.

```sh
# 1. Deployer le pod PostgreSQL (utilisateur atelier_admin, base
#    atelier_apiserver creee automatiquement)
kubectl apply -f deploy/dev/postgres/dev-pod.yaml
kubectl wait --for=condition=Ready pod/atelier-postgres-dev --timeout=60s

# 2. Creer les bases des autres composants (une seule base est creee
#    automatiquement par l'image officielle via POSTGRES_DB)
kubectl exec atelier-postgres-dev -- psql -U atelier_admin -d postgres -c \
  'CREATE DATABASE atelier_controller;'

# 3. Exposer PostgreSQL sur l'hote pour piloter les migrations/tests depuis
#    l'exterieur du cluster (port 5433 cote hote : le 5432 par defaut est
#    deja pris par d'autres projets sur cette machine de dev)
kubectl port-forward svc/atelier-postgres-dev 5433:5432 &
```

## Bases et convention de nommage

Chaque composant qui a besoin de PostgreSQL a sa propre base (isolation par
base, pas seulement par schema) :

| Base                 | Composant                | Migrations                               |
|----------------------|---------------------------|-------------------------------------------|
| `atelier_apiserver`  | `crates/api-server`       | `crates/api-server/migrations/`            |
| `atelier_controller` | `crates/controller`       | `crates/controller/migrations/`            |
| `atelier_pm`         | `services/pm-engine` (M5) | pas encore cree (hors perimetre M1)        |

En production (Jalon M6, `charts/atelier`), ces bases sont creees par un
Job Kubernetes dedie (`db-init-job.yaml`) avec un role d'administration de
schema separe (`atelier_migrator`) — voir
`docs/specs/PLAN-ACTION-GLOBAL.md`, section 9.3. En dev, `atelier_admin`
(mot de passe dans le Secret `atelier-postgres-dev`) joue directement ce
role, pas de separation des privileges necessaire pour un environnement
jetable.

## Lancer les tests avec PostgreSQL

```sh
export DATABASE_URL="postgres://atelier_admin:dev-only-not-for-production@127.0.0.1:5433/atelier_apiserver"
cargo test -p atelier-api-server
```

(adapter le nom de la base dans l'URL selon le composant teste, voir le
tableau ci-dessus).

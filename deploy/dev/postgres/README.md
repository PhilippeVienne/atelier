# PostgreSQL de developpement

Instance PostgreSQL en mode dev, deployee **dans** le cluster kind (meme
convention que `deploy/dev/kanidm` et `deploy/dev/openbao`) : pas de
persistance (`emptyDir`), donnees perdues a la suppression du pod.

```sh
# 1. Deployer le pod PostgreSQL (utilisateur atelier_admin superutilisateur,
#    base atelier_apiserver et role applicatif atelier_app crees
#    automatiquement au premier demarrage, voir ConfigMap
#    atelier-postgres-dev-init)
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
`docs/specs/PLAN-ACTION-GLOBAL.md`, section 9.3.

## Roles : `atelier_admin` vs `atelier_app`

Deux roles distincts existent des le premier demarrage (voir la ConfigMap
`atelier-postgres-dev-init`) :

- **`atelier_admin`** (`POSTGRES_USER`, superutilisateur) : execute les
  migrations (`sqlx::migrate!`, DDL), utilise par `DATABASE_URL` dans
  `main.rs` pour l'instant (voir limite ci-dessous).
- **`atelier_app`** (non-superutilisateur, `NOBYPASSRLS`) : role destine aux
  requetes applicatives une fois qu'un endpoint interagit reellement avec
  `session_logs`/`audit_events` — c'est lui qui doit etre utilise pour que
  la Row Level Security des migrations produise un effet.

**Verifie empiriquement** : un role superutilisateur (ou `BYPASSRLS`)
ignore silencieusement `ENABLE`/`FORCE ROW LEVEL SECURITY`, quelle que soit
la policy — comportement standard PostgreSQL, pas un bug de la migration.
`atelier_admin` (`POSTGRES_USER` cree par l'image officielle) est
superutilisateur ; se connecter avec ce role rend donc les policies RLS de
`crates/api-server/migrations/` inertes. Verifie a la main :

```sh
# Avec atelier_admin (superutilisateur) : RLS ignoree, les deux lignes de
# tenants differents restent visibles malgre `app.current_tenant`.
kubectl exec atelier-postgres-dev -- psql -U atelier_admin -d atelier_apiserver -c \
  "SET app.current_tenant='alice'; SELECT owner_subject FROM audit_events;"

# Avec atelier_app (non-superutilisateur) : RLS appliquee, seules les
# lignes d'alice sont visibles/insertables.
PGPASSWORD=dev-only-not-for-production kubectl exec -i atelier-postgres-dev -- psql \
  -U atelier_app -d atelier_apiserver -c \
  "SET app.current_tenant='alice'; SELECT owner_subject FROM audit_events;"
```

Tant qu'aucun code applicatif ne lit/ecrit ces tables (le cas au Jalon M1 :
elles existent par le schema mais ne sont pas encore consommees), utiliser
`atelier_admin` dans `DATABASE_URL` pour les migrations est sans
consequence. Le jour ou un endpoint ecrit reellement dans
`session_logs`/`audit_events`, `DATABASE_URL` (ou une variable dediee) devra
pointer vers `atelier_app`, avec un mecanisme de migration separe (Job
dedie, comme prevu au Jalon M6) puisque ce role n'a pas les privileges DDL.

## Lancer les tests avec PostgreSQL

```sh
export DATABASE_URL="postgres://atelier_admin:dev-only-not-for-production@127.0.0.1:5433/atelier_apiserver"
cargo test -p atelier-api-server
```

(adapter le nom de la base dans l'URL selon le composant teste, voir le
tableau ci-dessus).

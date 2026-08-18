# Kanidm de developpement

Instance Kanidm locale (Docker) pour tester le provisioning d'identite
d'Atelier, sans TLS valide (certificat auto-signe).

```sh
cd deploy/dev/kanidm

# 1. Generer les certificats TLS auto-signes (une seule fois)
docker run --rm -v "$(pwd)/data":/data -v "$(pwd)/server.toml":/data/server.toml:ro \
  kanidm/server:latest kanidmd cert-generate -c /data/server.toml

# 2. Lancer le serveur
docker run -d --name atelier-kanidm-dev -p 8443:8443 \
  -v "$(pwd)/data":/data -v "$(pwd)/server.toml":/data/server.toml:ro \
  kanidm/server:latest kanidmd server -c /data/server.toml

# 3. Recuperer les mots de passe admin/idm_admin (generes aleatoirement)
docker exec atelier-kanidm-dev kanidmd recover-account -c /data/server.toml idm_admin

# 4. Creer un service account privilegie pour le controller (doit pouvoir
#    creer/lire/supprimer d'autres service accounts, donc membre de idm_admins)
docker run --rm --network host \
  -v "$(pwd)/data/ca.pem":/data/ca.pem:ro \
  -e KANIDM_URL=https://localhost:8443 -e KANIDM_CA_PATH=/data/ca.pem \
  --entrypoint sh kanidm/tools:latest -c '
    kanidm login --name idm_admin -p "<mot de passe recupere a l etape 3>"
    kanidm service-account create atelier-controller "Atelier Controller" idm_admin --name idm_admin
    kanidm group add-members idm_admins atelier-controller --name idm_admin
    kanidm service-account api-token generate atelier-controller atelier-controller-ci --readwrite
  '
```

## Lancer les tests avec Kanidm

`crates/controller/tests/reconcile.rs` contient un test
(`apply_provisions_kanidm_entity_when_configured`) qui exerce le vrai
provisioning d'identite. Sans configuration il est silencieusement ignore (le
provisioning Kanidm est optionnel, cf. `ReconcileCtx.kanidm`) ; avec :

```sh
export KANIDM_URL=https://localhost:8443
export KANIDM_CA_PATH="$(pwd)/deploy/dev/kanidm/data/ca.pem"
export KANIDM_API_TOKEN="<token genere a l'etape 4>"
cargo test -p atelier-controller --test reconcile
```

Notes :

- `admin` gere le domaine (recycle bin, etc.), pas les comptes : c'est
  `idm_admin` qu'il faut utiliser pour creer des service accounts.
- Un service account "normal" (non membre d'un groupe admin) n'a pas le
  droit de creer d'autres service accounts : c'est pour ca que
  `atelier-controller` est ajoute au groupe `idm_admins` — c'est ce compte
  (pas `idm_admin` lui-meme) que le `controller` utilise en production, via
  son token API.
- Les tokens API generes ne sont affiches qu'une seule fois (non recuperables
  ensuite) ; par defaut ils sont en lecture seule, `--readwrite` est
  necessaire pour un service account qui doit creer/modifier des entites.
- `data/` (base + certs generes) est ignore par git, voir `.gitignore`.

Pour tout arreter/reinitialiser : `docker rm -f atelier-kanidm-dev && rm -rf data/* && mkdir -p data`.

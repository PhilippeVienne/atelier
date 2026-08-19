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

## OAuth2 Resource Server + test de `atelier-api-server` contre un vrai flux

`atelier-api-server` valide des JWT signes par un OAuth2 Resource Server
Kanidm (`ATELIER_JWT_ISSUER`/`ATELIER_JWT_JWKS_URL`/`ATELIER_JWT_AUDIENCE`,
voir `crates/api-server/src/auth.rs`). Setup, une seule fois :

```sh
# 1. Client OAuth2 public (PKCE) — le redirect_uri n'a pas besoin de
#    repondre reellement, seule la redirection HTTP est utilisee.
kanidm system oauth2 create-public atelier "Atelier" \
  https://localhost:9443/callback --name idm_admin

# 2. Portee "openid" accordee a tous les comptes (idm_all_persons) : sans
#    ce mapping, /oauth2/authorise refuse la demande de scope.
kanidm system oauth2 update-scope-map atelier idm_all_persons openid \
  --name idm_admin

# 3. Compte de test
kanidm person create atelier-test-user "Atelier Test User" --name idm_admin
docker exec atelier-kanidm-dev kanidmd recover-account \
  -c /data/server.toml atelier-test-user
# -> affiche un mot de passe genere, a reutiliser ci-dessous
```

Obtenir un vrai `access_token` (flux `authorization_code` + PKCE, scripte
via `get-oauth2-token.sh` — voir ce fichier pour le detail de chaque etape,
notamment pourquoi `kanidm login` seul ne suffit pas) :

```sh
./get-oauth2-token.sh atelier-test-user '<mot de passe recupere a l'etape 3>'
```

Lancer `atelier-api-server` contre ce Resource Server reel :

```sh
export ATELIER_JWT_ISSUER=https://localhost:8443/oauth2/openid/atelier
export ATELIER_JWT_JWKS_URL=https://localhost:8443/oauth2/openid/atelier/public_key.jwk
export ATELIER_JWT_AUDIENCE=atelier
export ATELIER_JWT_CA_PATH="$(pwd)/data/ca.pem"   # CA auto-signee du Kanidm de dev
cargo run -p atelier-api-server
```

Puis, avec l'`access_token` recupere plus haut :

```sh
curl -X POST http://localhost:8080/v1/workshops \
  -H "Authorization: Bearer <access_token>" -H 'Content-Type: application/json' \
  -d '{"name":"test","devcontainer":{"repo":"https://example.invalid/repo.git"},"resources":{"cpu":"500m","memory":"512Mi"}}'
```

`status.ownerSubject`/`spec.ownerSubject` du `Workshop` cree doit porter le
`sub` du token (l'UUID Kanidm du compte), jamais une valeur du corps de la
requete.

Deja verifie reellement cette session : `HTTP 201`, `ownerSubject` egal a
l'UUID Kanidm attendu. Voir `docs/PROGRESS.md` pour le detail du vrai bug
trouve en testant ceci (`InvalidAudience` — invisible avec les tokens de
test synthetiques du test d'integration, qui n'incluaient pas de claim
`aud`, contrairement a un vrai token Kanidm qui en porte toujours une).

## Client OAuth2 `atelier` : redirect_uri supplementaire pour le dashboard

Le meme client `atelier` (cree ci-dessus) sert aussi au dashboard
(`dashboard/README.md`), avec une seconde `redirect_uri` en plus de celle
du script `get-oauth2-token.sh` :

```sh
kanidm system oauth2 add-redirect-url atelier http://localhost:3000/api/auth/callback --name idm_admin
# necessaire : par defaut Kanidm refuse les redirections vers localhost
# pour un client public (risque usuel de detournement local)
kanidm system oauth2 enable-localhost-redirects atelier --name idm_admin
```

`oauth2_strict_redirect_uri` (active par defaut) exige une correspondance
exacte : toute nouvelle URL de callback doit etre ajoutee explicitement via
`add-redirect-url`, pas seulement son origine.

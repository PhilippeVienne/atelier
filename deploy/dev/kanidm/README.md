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

# 4. Utiliser le CLI (image kanidm/tools) pour piloter l'instance, ex:
docker run --rm --network host \
  -v "$(pwd)/data/ca.pem":/data/ca.pem:ro \
  -e KANIDM_URL=https://localhost:8443 -e KANIDM_CA_PATH=/data/ca.pem \
  --entrypoint sh kanidm/tools:latest -c '
    kanidm login --name idm_admin -p "<mot de passe recupere a l etape 3>"
    kanidm service-account create atelier-workshop-test "Atelier Workshop Test" idm_admin --name idm_admin
    kanidm service-account api-token generate atelier-workshop-test atelier-controller --readwrite
  '
```

Notes :

- `admin` gere le domaine (recycle bin, etc.), pas les comptes : c'est
  `idm_admin` qu'il faut utiliser pour creer des service accounts.
- Les tokens API generes ne sont affiches qu'une seule fois (non recuperables
  ensuite) ; par defaut ils sont en lecture seule, `--readwrite` est
  necessaire pour un service account qui doit creer/modifier des entites
  (ce que fera le `controller` pour provisionner l'identite de chaque
  Workshop).
- `data/` (base + certs generes) est ignore par git, voir `.gitignore`.

Pour tout arreter/reinitialiser : `docker rm -f atelier-kanidm-dev && rm -rf data/* && mkdir -p data`.

# Redis de developpement

Instance Redis en mode dev, deployee **dans** le cluster kind (meme
convention que `deploy/dev/postgres` et `deploy/dev/s3`) : pas de
persistance (`emptyDir`, AOF desactive), donnees perdues a la suppression
du pod.

```sh
# 1. Deployer le pod Redis
kubectl apply -f deploy/dev/redis/dev-pod.yaml
kubectl wait --for=condition=Ready pod/atelier-redis-dev --timeout=60s

# 2. Exposer Redis sur l'hote pour piloter les tests depuis l'exterieur du
#    cluster
kubectl port-forward svc/atelier-redis-dev 6379:6379 &
```

## Usage prevu : Redis Streams pour le Jalon M5 (`services/pm-engine`)

Voir `docs/specs/05-devfactory-pm-engine.md`, section 2 (garantie
at-least-once) : les webhooks d'issues/PRs des forges Git (Forgejo,
GitHub, GitLab) sont empiles dans un Stream Redis, consommes par
`atelier-pm-engine` via un consumer group avec accuse de reception
explicite (`XACK`). En cas de crash du worker, les messages non acquittes
sont repris via `XAUTOCLAIM`.

Les Streams sont une structure de donnees native de Redis (disponibles
depuis Redis 5, pas un module a activer separement a la construction ou au
demarrage) : `redis:7.4-alpine` les supporte nativement, aucune
configuration serveur additionnelle n'est necessaire au-dela d'un Redis
standard.

### Verification empirique du cycle at-least-once (faite le 2026-08-24)

```sh
# Empiler un evenement webhook simule
kubectl exec atelier-redis-dev -- redis-cli XADD pm-engine:webhooks '*' \
  event issue.opened repo atelier issue 42
# -> "<id>-0"

kubectl exec atelier-redis-dev -- redis-cli XLEN pm-engine:webhooks
# -> 1

# Creer le consumer group (equivalent de "pm-engine-workers" du schema
# d'architecture) et lire un message non acquitte
kubectl exec atelier-redis-dev -- redis-cli XGROUP CREATE pm-engine:webhooks pm-engine-workers 0
kubectl exec atelier-redis-dev -- redis-cli XREADGROUP GROUP pm-engine-workers worker-1 \
  COUNT 1 STREAMS pm-engine:webhooks '>'
# -> renvoie bien le message empile ci-dessus

# Le message reste en attente (PEL, Pending Entries List) tant qu'il n'est
# pas acquitte : simule un worker qui crashe avant XACK
kubectl exec atelier-redis-dev -- redis-cli XPENDING pm-engine:webhooks pm-engine-workers
# -> 1 message en attente pour worker-1 (rejouable via XAUTOCLAIM)

# Acquittement explicite : le message sort de la PEL
kubectl exec atelier-redis-dev -- redis-cli XACK pm-engine:webhooks pm-engine-workers <id>-0
kubectl exec atelier-redis-dev -- redis-cli XPENDING pm-engine:webhooks pm-engine-workers
# -> 0 message en attente
```

Ce cycle (`XADD` -> `XREADGROUP` -> `XPENDING` non vide avant `XACK` -> vide
apres) valide que le mecanisme at-least-once decrit dans la spec est
utilisable tel quel avec cette image, sans mock. L'implementation reelle
du consommateur (`services/pm-engine/redis_consumer.py`, tache 5.4.3, avec
`XAUTOCLAIM` sur incident) reste hors perimetre de ce lot dev-infra.

## Variables d'environnement pour les tests & composants

```sh
export REDIS_URL="redis://127.0.0.1:6379/0"
```

Pour tout arreter/reinitialiser :
`kubectl delete -f deploy/dev/redis/dev-pod.yaml`.

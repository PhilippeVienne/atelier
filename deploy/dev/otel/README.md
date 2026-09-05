# Observabilité de développement (traces/métriques/logs)

Service global du cluster (même niveau qu'OpenBao/LiteLLM, `deploy/dev/otel/`), déployé inconditionnellement par `deploy/dev/local-stack.sh` — voir `docs/specs/12-observabilite.md`.

Une seule image tout-en-un, `grafana/otel-lgtm` : collecteur OTLP + Tempo (traces) + Prometheus (métriques) + Loki (logs) + Grafana, datasources auto-provisionnées. Mesuré empiriquement (`docker run`, 2026-09-05) : `healthy` en ~40s, ~480Mi de RAM au repos.

```sh
# 1. Déployer
kubectl apply -f deploy/dev/otel/dev-deployment.yaml

# 2. Port-forward (fait automatiquement par local-stack.sh)
kubectl port-forward svc/atelier-observability 4317:4317 3000:3000 &

# 3. Vérifier
curl http://127.0.0.1:3000/api/health

# 4. Consulter (identifiants par défaut de l'image : admin/admin)
open http://127.0.0.1:3000
```

## Branchement côté binaires Rust

`crates/common/src/telemetry.rs` exporte des traces OTLP dès que `OTEL_EXPORTER_OTLP_ENDPOINT` est présente dans l'environnement — `local-stack.sh` la positionne automatiquement dans `env.sh` (`http://127.0.0.1:4317`, port-forward local puisque `controller`/`api-server` tournent hors cluster en dev).

## Limites assumées (dev)

- **Volume éphémère** : aucune persistance, un redémarrage du pod perd tout l'historique (voir spec §5).
- **`api-server` n'exporte encore aucune trace** (spec §4.1, tâche 7.3) : seule la boucle de réconciliation du `controller` est aujourd'hui instrumentée (§4.2 de la spec).

Pour tout arrêter/réinitialiser :
`kubectl delete -f deploy/dev/otel/dev-deployment.yaml`.

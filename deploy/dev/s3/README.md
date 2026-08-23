# Stockage S3 (RustFS) de développement

Instance S3 locale en mode dev basée sur **RustFS** (haute performance, 100% Rust), déployée **dans** le cluster Kind (même convention que `deploy/dev/openbao` et `deploy/dev/postgres`) : pas de persistance (`emptyDir`), données jetables.

```sh
# 1. Déployer le pod RustFS (buckets créés automatiquement via l'initContainer)
kubectl apply -f deploy/dev/s3/dev-pod.yaml
kubectl wait --for=condition=Ready pod/atelier-s3-dev --timeout=60s

# 2. Exposer l'API S3 sur l'hôte (port 9000)
kubectl port-forward svc/atelier-s3-dev 9000:9000 &
```

## Buckets & Conventions de Nommage

| Bucket | Usage | Jalon |
|---|---|---|
| `atelier-sessions` | Enregistrements de sessions VS Code / terminal compressés (zstd) | M2 |
| `atelier-snapshots` | Snapshots mémoire microVMs déchargés sur S3 | M2 / M5 |
| `forgejo-lfs-attachments` | Stockage LFS / attachements Forgejo | M2 |

## Variables d'Environnement pour les Tests & Composants

```sh
export S3_ENDPOINT="http://127.0.0.1:9000"
export S3_REGION="us-east-1"
export AWS_ACCESS_KEY_ID="atelier-rustfs-access-key"
export AWS_SECRET_ACCESS_KEY="atelier-rustfs-secret-key"
export S3_BUCKET_SESSIONS="atelier-sessions"
export S3_BUCKET_SNAPSHOTS="atelier-snapshots"
export S3_FORCE_PATH_STYLE="true"
```

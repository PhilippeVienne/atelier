# Ollama de developpement (embedding local)

Instance Ollama en mode dev, deployee **dans** le cluster kind (meme
convention que `deploy/dev/redis`/`deploy/dev/postgres`) : pas de
persistance (`emptyDir`), modele telecharge perdu a la suppression du pod.

```sh
# 1. Deployer le pod Ollama
kubectl apply -f deploy/dev/ollama/dev-pod.yaml
kubectl wait --for=condition=Ready pod/atelier-ollama-dev --timeout=60s

# 2. Telecharger le modele d'embedding (une seule fois par pod, ~46 Mo —
#    meme etape ponctuelle post-deploiement que l'activation de la methode
#    d'auth Kubernetes d'OpenBao, voir deploy/dev/openbao/README.md)
kubectl exec atelier-ollama-dev -- ollama pull all-minilm

# 3. Exposer Ollama sur l'hote pour tester depuis l'exterieur du cluster
kubectl port-forward svc/atelier-ollama-dev 11434:11434 &
curl http://127.0.0.1:11434/api/embeddings -d '{"model":"all-minilm","prompt":"test"}'
```

## Pourquoi Ollama plutot que l'API Hugging Face (tache 5.0.2)

La tache 5.0.2 demandait initialement de router `deploy/dev/llm-proxy` vers
l'API d'inference hebergee de Hugging Face
(`huggingface/sentence-transformers/all-MiniLM-L6-v2`). Verifie
empiriquement (2026-08-24) : cette API exige desormais une authentification
meme pour un modele public (`AuthenticationError`, page de login HTML
retournee) — contraire a l'objectif explicite "sans cle payante bloquante"
de cette tache. Ollama sert le meme modele (`all-minilm`, meme famille que
`sentence-transformers/all-MiniLM-L6-v2`) entierement en local, sans aucune
cle ni appel reseau externe apres le telechargement initial.

## Integration avec LiteLLM (`deploy/dev/llm-proxy`)

`deploy/dev/llm-proxy/config.yaml` route le modele `embedding-dev-local`
vers `http://atelier-ollama-dev:11434` (nom de Service, resolu en DNS
interne au cluster — LiteLLM et Ollama tournent tous deux comme services
`ClusterIP` globaux, pas dans un pod de Workshop). Dimension native du
vecteur produit : 384 — different de `VECTOR(1536)` (colonne
`project_memories.embedding`, calibree sur `text-embedding-3-small`, voir
`services/pm-engine/migrations/20260824000000_init_pm_engine.sql`) : ne pas
ecrire directement dans cette colonne avec ce modele sans adaptation
(re-projection, ou colonne dediee aux tests dev).

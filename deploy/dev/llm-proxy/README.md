# LLM Proxy (LiteLLM) de developpement

Service **global du cluster**, au meme niveau qu'OpenBao
(`deploy/dev/openbao/`) : une seule instance LiteLLM partagee par tous les
Workshops — pas un sidecar par pod. Traduit les appels Anthropic Messages
API de Claude Code (`/v1/messages`) vers DeepSeek par defaut (reduction de
cout), avec un alias explicite vers le vrai Anthropic Sonnet pour les
taches complexes. Voir `docs/PROGRESS.md`, section "LLM Proxy", pour le
detail de l'architecture et les limites assumees.

```sh
# 1. ConfigMap (le modele de routage, sans aucune cle)
kubectl create configmap atelier-llm-proxy-config \
  --from-file=config.yaml=deploy/dev/llm-proxy/config.yaml

# 2. Secret (cles reelles — jamais commitees). LITELLM_MASTER_KEY est le
#    jeton que Claude Code presentera ensuite comme ANTHROPIC_AUTH_TOKEN ;
#    n'importe quelle chaine suffit pour un cluster de dev non expose.
kubectl create secret generic atelier-llm-proxy-dev \
  --from-literal=DEEPSEEK_API_KEY="<ta cle DeepSeek>" \
  --from-literal=ANTHROPIC_API_KEY="<ta cle Anthropic, pour sonnet-premium>" \
  --from-literal=LITELLM_MASTER_KEY="sk-atelier-llm-proxy-dev"

# 3. Deployer
kubectl apply -f deploy/dev/llm-proxy/dev-deployment.yaml

# 4. Verifier (depuis l'hote)
kubectl port-forward svc/atelier-llm-proxy 4000:4000 &
curl http://127.0.0.1:4000/health/liveliness
curl -X POST http://127.0.0.1:4000/v1/messages \
  -H "Authorization: Bearer sk-atelier-llm-proxy-dev" \
  -H "Content-Type: application/json" \
  -d '{"model":"deepseek-dev","max_tokens":64,"messages":[{"role":"user","content":"dis bonjour en un mot"}]}'
```

## Branchement cote Workshop

`crates/controller` cable automatiquement, quand `ATELIER_LLM_PROXY_ADDR`
et `ATELIER_LLM_PROXY_AUTH_TOKEN` sont positionnes sur le `controller`
lui-meme (ex: `atelier-llm-proxy.default.svc.cluster.local:4000` et le
`LITELLM_MASTER_KEY` ci-dessus) :

- l'alias `net-proxy` `llm-proxy` (`crates/net-proxy/src/internal.rs`),
  toujours actif, jamais dans l'allowlist egress de l'utilisateur ;
- `ANTHROPIC_BASE_URL=http://llm-proxy` / `ANTHROPIC_AUTH_TOKEN=<jeton>` /
  `ANTHROPIC_API_KEY=` dans `/etc/environment` du devcontainer construit
  (`crates/image-builder/src/main.rs::inject_net_proxy_config`) — Claude
  Code a l'interieur de la microVM les prend en compte automatiquement,
  aucune configuration manuelle dans le devcontainer.

Sans ces deux variables sur le `controller`, la fonctionnalite reste
desactivee (aucun alias, aucune injection) — meme convention que
`OPENBAO_ADDR`.

## Limites assumees (dev)

- Un seul jeton (`LITELLM_MASTER_KEY`/`ANTHROPIC_AUTH_TOKEN`) partage par
  tous les Workshops : pas d'isolation de budget/abus par Workshop dans ce
  lot (LiteLLM a une fonctionnalite de cles virtuelles par tenant, non
  branchee ici).
- Un bug connu de LiteLLM (`BerriAI/litellm#8795`) fait qu'une version
  donnee peut exiger un en-tete `x-litellm-key` sur les sondes de sante —
  si le Deployment reste en `CrashLoopBackOff`/`Unready` a cause des
  probes, retirer temporairement `readinessProbe`/`livenessProbe` du
  manifest pour diagnostiquer avant d'ajuster.

Pour tout arreter/reinitialiser :
`kubectl delete -f deploy/dev/llm-proxy/dev-deployment.yaml && kubectl delete configmap atelier-llm-proxy-config && kubectl delete secret atelier-llm-proxy-dev`.

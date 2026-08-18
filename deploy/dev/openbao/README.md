# OpenBao de developpement

Instance OpenBao en mode dev, deployee **dans** le cluster kind (pas en
conteneur Docker a cote) : la methode d'auth Kubernetes a besoin de parler a
l'API server, ce qui est trivial en intra-cluster (`https://kubernetes.default.svc`)
et evite de bricoler la connectivite Docker/kind pour un cas d'usage dev.

```sh
# 1. Deployer le pod OpenBao (mode dev, root token = "root") + le
#    ServiceAccount/ClusterRoleBinding qui lui permet d'appeler TokenReview
kubectl apply -f deploy/dev/openbao/dev-pod.yaml

# 2. Activer et configurer la methode d'auth Kubernetes (une seule fois),
#    en utilisant le ServiceAccount du pod OpenBao lui-meme comme reviewer
kubectl exec atelier-openbao-dev -- sh -c '
  export BAO_ADDR=http://127.0.0.1:8200 BAO_TOKEN=root
  bao auth enable kubernetes
  bao write auth/kubernetes/config \
    kubernetes_host="https://kubernetes.default.svc" \
    token_reviewer_jwt=@/var/run/secrets/kubernetes.io/serviceaccount/token \
    kubernetes_ca_cert=@/var/run/secrets/kubernetes.io/serviceaccount/ca.crt
'

# 3. Exposer OpenBao sur l'hote pour piloter l'API depuis les tests/le CLI
kubectl port-forward svc/atelier-openbao-dev 8200:8200 &
```

Le `controller` (crate `atelier-controller`) provisionne ensuite, par
Workshop, une policy + un role `auth/kubernetes/role/workshop-<name>` scopant
l'acces au chemin KV `secret/{data,metadata}/workshops/<name>/*` au seul
ServiceAccount dedie du pod parent (`<name>-parent`), voir
`crates/controller/src/openbao.rs`.

## Lancer les tests avec OpenBao

```sh
export OPENBAO_ADDR=http://127.0.0.1:8200
export OPENBAO_TOKEN=root
cargo test -p atelier-controller --test reconcile
```

Sans ces variables, `apply_provisions_openbao_role_when_configured` est
silencieusement ignore (le provisioning OpenBao est optionnel, cf.
`ReconcileCtx.openbao`).

## Tester identity-proxy en conditions reelles (sans image/pod)

`identity-proxy` lit son token de ServiceAccount depuis
`ATELIER_K8S_SA_TOKEN_PATH` (par defaut le chemin standard projete par
Kubernetes dans un pod) : on peut donc valider le pont d'authentification
depuis l'hote avec un vrai token, sans build/push d'image ni deploiement de
pod complet :

```sh
kubectl create serviceaccount demo-parent
# ... provisionner le role OpenBao pour ce SA, cf. openbao.rs ou le test
#     apply_provisions_openbao_role_when_configured pour la sequence exacte ...
kubectl create token demo-parent > /tmp/sa-token.txt

OPENBAO_ADDR=http://127.0.0.1:8200 \
ATELIER_WORKSHOP_NAME=demo \
ATELIER_K8S_SA_TOKEN_PATH=/tmp/sa-token.txt \
cargo run -p atelier-identity-proxy
```

Pour tout arreter/reinitialiser : `kubectl delete -f deploy/dev/openbao/dev-pod.yaml`
(mode dev : tout l'etat OpenBao est en memoire, rien a nettoyer cote OpenBao
lui-meme).

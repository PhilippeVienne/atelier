# Traefik (Ingress de dev) — routage par nom d'hôte

Remplace les port-forwards individuels (un par service, sources de
collisions de port constatées en pratique — voir `docs/PROGRESS.md`) par un
seul point d'entrée, routé par en-tête `Host`, comme le ferait un vrai
ingress de production.

`atelier-keycloak-dev` et `atelier-forgejo-dev` tournent **dans** le
cluster (Service `ClusterIP` normal). `atelier-api-server` et le dashboard
tournent encore comme process sur l'hôte (pas conteneurisés) : ils sont
exposés via un `Service` sans sélecteur + un `Endpoints` manuel pointant
sur `172.19.0.1`, la gateway du réseau Docker `kind` — routable depuis
n'importe quel pod du cluster vers l'hôte (vérifié réellement :
`kubectl run curltest --image=curlimages/curl --rm -i --restart=Never --
curl http://172.19.0.1:8080/healthz` répond `200`). Ces deux process
doivent donc écouter sur `0.0.0.0`, pas seulement `127.0.0.1` (déjà le cas :
`crates/api-server/src/main.rs` et `dashboard/server.ts`).

## Déploiement

```sh
kubectl apply -f deploy/dev/traefik/dev-traefik.yaml
kubectl wait --for=condition=Available deployment/atelier-traefik-dev --timeout=60s
kubectl apply -f deploy/dev/traefik/ingresses.yaml
```

## IP du node kind et `/etc/hosts`

Traefik tourne en `hostNetwork: true` et lie le port **80 standard**
directement dans l'espace réseau du node (pas un `Service` `NodePort` : le
port 80 est hors de la plage `NodePort` par défaut de l'API server,
`30000-32767`) : joignable sur l'IP du node kind lui-même, port 80, sans
port-forward.

```sh
sudo deploy/dev/traefik/update-hosts.sh
```

Détecte l'IP actuelle du node (`docker inspect
atelier-dev-control-plane`) et met à jour `/etc/hosts` de façon idempotente
(remplace la ligne précédente, identifiée par le marqueur
`# atelier-dev-hosts`, plutôt que d'en empiler une nouvelle) — à relancer
si le cluster kind est recréé (nouvelle IP de node). Cette IP est
**directement joignable depuis l'hôte** : réseau Docker en pont standard
sur Linux, pas de VM Docker Desktop intermédiaire.

## URLs résultantes

| Domaine | Cible |
|---|---|
| `http://auth.atelier.local` | `atelier-keycloak-dev` (in-cluster) |
| `http://git.atelier.local` | `atelier-forgejo-dev` (in-cluster) |
| `http://api.atelier.local` | `atelier-api-server` (process hôte, via la gateway Docker) |
| `http://app.atelier.local` | dashboard Next.js (process hôte, via la gateway Docker) |

Port 80 implicite pour les quatre, aucun numéro de port à retenir.

## Limites connues (dev uniquement)

- Pas de TLS (`--entrypoints.web` seulement, HTTP en clair) — suffisant pour
  du dev local, pas destiné à un usage au-delà.
- `hostNetwork: true` : le pod Traefik partage l'espace réseau du node kind
  (pas d'isolation réseau pod-a-pod pour lui) — acceptable pour un cluster
  de dev jetable, jamais en production.
- `apiVersion: v1` `Endpoints` est déprécié depuis Kubernetes 1.33 au profit
  de `discovery.k8s.io/v1` `EndpointSlice` (avertissement au `kubectl
  apply`, sans impact fonctionnel sur cette version de kind).
- Si l'IP de la gateway Docker (`172.19.0.1`, utilisée dans
  `ingresses.yaml` pour joindre les process hôte) change (recréation du
  réseau Docker `kind`), mettre à jour `deploy/dev/traefik/ingresses.yaml`
  — pas automatisé, contrairement à l'IP du node (`update-hosts.sh`).

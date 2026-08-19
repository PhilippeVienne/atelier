# Device plugin `/dev/kvm` de developpement

`crates/kvm-device-plugin` implemente l'API kubelet "device plugin"
v1beta1 (`k8s.io/kubelet/pkg/apis/deviceplugin/v1beta1`, proto vendore dans
`crates/kvm-device-plugin/proto/api.proto` — sous-ensemble stable, pas de
negociation d'allocation preferee). Un exemplaire par noeud (DaemonSet),
qui annonce `/dev/kvm` **et** `/dev/net/tun` comme ressource allouable
unique `atelier.dev/kvm` (les deux sont toujours alloues ensemble : tout
conteneur de ce projet qui a besoin de l'un a besoin de l'autre).

Objectif : permettre a `vm-supervisor`/`image-builder` de demander ces
devices via `resources.limits`, sans `securityContext.privileged: true` —
voir `deploy/dev/vm-supervisor/README.md` pour le detail du blocage que ca
resout (device cgroup controller de Kubernetes/containerd).

## Prerequis pour compiler : `protoc`

`tonic-prost-build` a besoin du compilateur `protoc` au moment du build
(pas seulement de la lib `prost`). Sur une machine sans `apt`/`sudo`
disponibles, telecharger un binaire precompile et le pointer via la
variable d'environnement `PROTOC` (ex: dans `.cargo/config.toml`, non
commite — chemin specifique a la machine) :

```sh
curl -sL -o /tmp/protoc.zip \
  https://github.com/protocolbuffers/protobuf/releases/download/v28.3/protoc-28.3-linux-x86_64.zip
unzip -o /tmp/protoc.zip -d ~/.local/protoc
export PROTOC=~/.local/protoc/bin/protoc
```

Sur Debian/Ubuntu avec `sudo` disponible : `apt-get install protobuf-compiler`
suffit (pas besoin de `PROTOC` explicite).

## Construire et deployer dans kind

```sh
docker build -t atelier-kvm-device-plugin:dev -f crates/kvm-device-plugin/Dockerfile .
kind load docker-image atelier-kvm-device-plugin:dev --name atelier-dev
kubectl apply -f deploy/dev/kvm-device-plugin/daemonset.yaml
```

## Verifier

```sh
kubectl -n kube-system get pods -l app=atelier-kvm-device-plugin
kubectl get node <nom-du-noeud> -o jsonpath='{.status.allocatable}' | python3 -m json.tool
# doit lister "atelier.dev/kvm": "32" (nombre configurable via
# ATELIER_KVM_DEVICE_COUNT dans le DaemonSet)
```

Test d'allocation reelle (pod non privilegie, ouvre effectivement le
device — pas juste `ls`) :

```sh
kubectl run kvm-alloc-test --image=debian:bookworm-slim --restart=Never \
  --overrides='{"spec":{"containers":[{"name":"test","image":"debian:bookworm-slim","command":["sh","-c","exec 3<>/dev/kvm && echo OPEN_OK; sleep 3600"],"resources":{"limits":{"atelier.dev/kvm":"1"}}}]}}'
kubectl logs kvm-alloc-test
# doit afficher OPEN_OK
kubectl delete pod kvm-alloc-test --force --grace-period=0
```

## Pourquoi `/dev/net/tun` et pas seulement `/dev/kvm`

Un plugin plus "correct" annoncerait deux ressources distinctes. Choix
delibere de n'en annoncer qu'une seule ici : dans ce projet, aucun
conteneur ne demande jamais l'un sans l'autre (Firecracker + TAP reseau
sont toujours utilises ensemble), et une ressource unique simplifie le
`ResourceRequirements` genere par le controller
(`crates/controller/src/reconcile.rs::kvm_device_resources`).

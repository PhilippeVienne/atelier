# vm-supervisor de developpement

Le pod parent (conteneur `vm-supervisor`) pilote une microVM Firecracker
**jailee** (cf. `crates/vm-supervisor/src/vm.rs`). Ca demande un acces reel
a `/dev/kvm` depuis un pod Kubernetes, ce qui n'est pas trivial.

## Plus de `privileged: true` : device plugin `/dev/kvm`

Constate d'abord en testant reellement (voir `docs/ARCHITECTURE.md`) :
monter `/dev/kvm` via `hostPath` dans un pod **non privilegie** donnait
`Operation not permitted` a l'ouverture du device, meme avec les bonnes
permissions de fichier — le **device cgroup controller** de
Kubernetes/containerd bloque par defaut l'acces aux devices non
allowlistes independamment des permissions du fichier.

Resolu par `crates/kvm-device-plugin` (voir
`deploy/dev/kvm-device-plugin/README.md`) : un device plugin Kubernetes qui
annonce `/dev/kvm` (et `/dev/net/tun`) comme ressource allouable
(`atelier.dev/kvm`), ce qui fait explicitement allowlister ces devices par
le kubelet pour le conteneur qui la demande via `resources.limits` — sans
pod privilegie. Il reste necessaire d'ajouter explicitement les
capabilities `NET_ADMIN` (creation du TAP), `SYS_ADMIN` et `SYS_RESOURCE`
(le jailer en a besoin pour elever ses capabilities de fichier a l'exec,
posees via `setcap` dans ce Dockerfile) — le reste des capabilities du
`setcap` (`SYS_CHROOT`, `SETUID`, `SETGID`, `MKNOD`, `DAC_OVERRIDE`) fait
deja partie de l'ensemble par defaut containerd/Docker. Voir
`crates/controller/src/reconcile.rs::firecracker_security_context` et
`::kvm_device_resources`, verifie contre un vrai pod sur kind (boot
Firecracker reel, `Running`, aucune capability hors de cette liste).

## Construire et charger l'image dans kind

```sh
# necessite deploy/dev/firecracker/assets/{firecracker,jailer,vmlinux.bin}
# (cf. deploy/dev/firecracker/README.md)
docker build -t atelier-vm-supervisor:dev -f crates/vm-supervisor/Dockerfile .
kind load docker-image atelier-vm-supervisor:dev --name atelier-dev
```

## Tester de bout en bout (Workshop reel -> microVM reelle)

Le pipeline complet (image-builder construit et pousse au registre) n'est
pas encore testable directement dans kind (le Job image-builder doit
pouvoir joindre le registre de dev, ce qui demande un cablage reseau
kind <-> registre non fait). En attendant, on peut valider le pod parent
seul en peuplant le PVC de cache a la main :

```sh
# 1. Recuperer un rootfs deja construit (cf. deploy/dev/image-builder/) et
#    le placer dans le PVC via un pod temporaire
kubectl run pvc-populate --image=busybox --restart=Never \
  --overrides='{"spec":{"containers":[{"name":"populate","image":"busybox","command":["sleep","300"],"volumeMounts":[{"name":"cache","mountPath":"/cache"}]}],"volumes":[{"name":"cache","persistentVolumeClaim":{"claimName":"atelier-image-cache"}}]}}'
kubectl wait --for=condition=Ready pod/pvc-populate --timeout=60s
kubectl exec pvc-populate -- mkdir -p /cache/sha256_finaltest
kubectl cp rootfs.ext4 default/pvc-populate:/cache/sha256_finaltest/rootfs.ext4
kubectl delete pod pvc-populate

# 2. Creer un Workshop avec status.imageDigest deja renseigne (simule un
#    build deja termine)
kubectl apply -f - <<'EOF'
apiVersion: atelier.dev/v1alpha1
kind: Workshop
metadata:
  name: test-vm
  namespace: default
spec:
  devcontainer:
    repo: https://github.com/microsoft/vscode-remote-try-python
  resources:
    cpu: "500m"
    memory: "768Mi"
  ownerSubject: test-user
EOF
kubectl patch workshop test-vm --type merge --subresource status \
  -p '{"status":{"phase":"BuildingImage","imageDigest":"sha256:finaltest"}}'

# 3. Lancer le controller (doit tourner pour reconcilier)
cargo run -p atelier-controller

# 4. Verifier
kubectl logs test-vm-parent -f
# doit afficher "microVM running", puis status.phase du Workshop passe a
# "Running"
```

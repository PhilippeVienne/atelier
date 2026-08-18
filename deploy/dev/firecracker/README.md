# Firecracker de developpement

Necessite `/dev/kvm` accessible (virtualisation materielle activee, et
l'utilisateur courant doit pouvoir l'ouvrir en lecture-ecriture — via le
groupe `kvm` ou une ACL).

```sh
./deploy/dev/firecracker/fetch-test-assets.sh
```

Recupere dans `deploy/dev/firecracker/assets/` (ignore par git, gros
fichiers binaires) :

- `firecracker` / `jailer` — binaires officiels
- `vmlinux.bin` — noyau de test (artefact CI du projet Firecracker)
- `rootfs.ext4` — rootfs Ubuntu 22.04 minimal de test (~300 Mo)

Ce ne sont **pas** les images produites par `image-builder` a partir d'un
devcontainer : ce sont des fixtures generiques qui servent uniquement a
valider que `vm-supervisor` sait piloter Firecracker (boot, snapshot,
restore) independamment du pipeline de build d'image, encore incomplet.

## Lancer les tests

```sh
export ATELIER_TEST_FIRECRACKER_BIN="$(pwd)/deploy/dev/firecracker/assets/firecracker"
export ATELIER_TEST_VM_KERNEL_PATH="$(pwd)/deploy/dev/firecracker/assets/vmlinux.bin"
export ATELIER_TEST_VM_ROOTFS_PATH="$(pwd)/deploy/dev/firecracker/assets/rootfs.ext4"
cargo test -p atelier-vm-supervisor
```

Sans ces variables, le test est silencieusement ignore (Firecracker/KVM
n'est pas disponible dans tous les environnements, notamment beaucoup de CI).

## Lancer vm-supervisor lui-meme

```sh
ATELIER_FIRECRACKER_BIN="$(pwd)/deploy/dev/firecracker/assets/firecracker" \
ATELIER_VM_KERNEL_PATH="$(pwd)/deploy/dev/firecracker/assets/vmlinux.bin" \
ATELIER_VM_ROOTFS_PATH="$(pwd)/deploy/dev/firecracker/assets/rootfs.ext4" \
ATELIER_VM_SOCKET_PATH=/tmp/atelier-vm.sock \
cargo run -p atelier-vm-supervisor
```

Note : `ATELIER_VM_SOCKET_PATH` doit rester court — `sun_path` (chemin d'un
socket Unix) est limite a ~108 octets sur Linux, un chemin sous ce
repertoire de travail le depasserait facilement.

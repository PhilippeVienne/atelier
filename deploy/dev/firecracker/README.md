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
valider que `vm-supervisor` sait piloter Firecracker (boot jaile, snapshot,
restore) independamment du pipeline de build d'image, encore incomplet.

## Jailer sans root : capabilities Linux (setcap)

`vm-supervisor` utilise toujours le jailer (isolation reelle : chroot,
cgroups). Le binaire `jailer` a besoin de privileges (unshare de namespace
de montage, chroot, cgroups, setuid/setgid, creation de device nodes), mais
**pas de root complet** : on les lui donne directement via des capabilities
Linux sur le fichier, une seule fois :

```sh
sudo setcap cap_sys_admin,cap_sys_resource,cap_sys_chroot,cap_setuid,cap_setgid,cap_mknod,cap_dac_override+eip \
  deploy/dev/firecracker/assets/jailer
```

`sudo` a ete essaye en premier (via le `SudoProcessSpawner` de `fctools`) et
abandonne : ce spawner invoque toujours `sudo -S -s <jailer> ...`, et le
flag `-s` fait autoriser le **shell** par sudoers plutot que le binaire
jailer lui-meme — impossible de scoper une regle NOPASSWD finement sans
autoriser un shell root arbitraire. Les capabilities evitent completement
le probleme (et evitent sudo/root a l'execution).

## Piege : `chroot-base-dir` ne doit pas etre sur du `tmpfs,nodev`

Le jailer cree ses propres device nodes (`/dev/kvm`, `/dev/urandom`, ...)
dans le jail via `mknod`. Si le repertoire de base du jail est sur un
systeme de fichiers monte avec l'option `nodev` (ex: `/tmp` en `tmpfs` sur
beaucoup de distributions), ces device nodes sont crees mais **inertes** :
Firecracker echoue au demarrage avec `Kvm error: ... Permission denied`,
message trompeur qui suggere un probleme d'ACL alors que le vrai probleme
est `nodev`. Utiliser un repertoire sur le systeme de fichiers racine (ou
tout montage sans `nodev`), ex: `/var/tmp` ou `/srv/jailer` (le defaut du
jailer), pas `/tmp`. Verifier avec `findmnt <chemin>`.

## Lancer les tests

```sh
export ATELIER_TEST_FIRECRACKER_BIN="$(pwd)/deploy/dev/firecracker/assets/firecracker"
export ATELIER_TEST_JAILER_BIN="$(pwd)/deploy/dev/firecracker/assets/jailer"
export ATELIER_TEST_VM_KERNEL_PATH="$(pwd)/deploy/dev/firecracker/assets/vmlinux.bin"
export ATELIER_TEST_VM_ROOTFS_PATH="$(pwd)/deploy/dev/firecracker/assets/rootfs.ext4"
cargo test -p atelier-vm-supervisor
```

Sans ces variables, le test est silencieusement ignore (Firecracker/KVM
n'est pas disponible dans tous les environnements, notamment beaucoup de CI).

## Lancer vm-supervisor lui-meme

```sh
ATELIER_FIRECRACKER_BIN="$(pwd)/deploy/dev/firecracker/assets/firecracker" \
ATELIER_JAILER_BIN="$(pwd)/deploy/dev/firecracker/assets/jailer" \
ATELIER_VM_KERNEL_PATH="$(pwd)/deploy/dev/firecracker/assets/vmlinux.bin" \
ATELIER_VM_ROOTFS_PATH="$(pwd)/deploy/dev/firecracker/assets/rootfs.ext4" \
ATELIER_VM_CHROOT_BASE_DIR=/var/tmp/atelier-jailer \
ATELIER_VM_UID=$(id -u) ATELIER_VM_GID=$(id -g) \
cargo run -p atelier-vm-supervisor
```

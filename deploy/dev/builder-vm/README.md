# microVM "builder" de developpement

Isole `envbuilder` (build du devcontainer d'un Workshop) dans une microVM
Firecracker jetable plutot que dans le conteneur Kubernetes du Job
`image-builder`, pour eviter d'avoir a accorder `CAP_SYS_ADMIN` a un
conteneur qui execute des instructions de build issues du **depot cible du
Workshop** — potentiellement non fiable. Voir `docs/PROGRESS.md`, section
"Reseau kind ↔ registre", pour le raisonnement complet, et
`crates/firecracker/src/network.rs` / `crates/builder-vm-init` pour
l'implementation.

Reutilise le meme plumbing jailer/`fctools` que `vm-supervisor`
(`crates/firecracker`, extrait de `vm-supervisor` cette session) et le meme
pipeline crane-export + `mke2fs` que `image-builder` pour produire un rootfs
bootable a partir d'une image OCI normale.

Reseau : pas d'acces direct/NAT vers Internet. Le guest ne voit qu'un lien
point-a-point (TAP + sous-reseau link-local `/30`) vers son seul voisin,
`net-proxy` (`crates/net-proxy`, deja "Fonctionnel" — allowlist de domaines +
tunnel `CONNECT`), configure comme `HTTP_PROXY`/`HTTPS_PROXY` pour
`envbuilder`. C'est `net-proxy`, pas cette microVM, qui gere la sortie
reelle vers Internet et l'allowlist — coherent avec le modele de securite du
projet ("net-proxy = seul chemin de sortie reseau autorise pour la
microVM").

## 1. Construire le rootfs de la builder VM

Contenu de confiance (notre propre `Dockerfile`), construit via `docker
build` classique — pas d'isolation particuliere necessaire ici, voir le
commentaire en tete du Dockerfile.

```sh
docker build -t atelier-builder-vm-init:dev -f crates/builder-vm-init/Dockerfile .
```

Conversion en disque ext4 bootable, meme pipeline que
`deploy/dev/image-builder/README.md` (crane export -> tar -> `mke2fs -d`).
Necessite un registre pour l'etape `crane export` (envoi via un registre
intermediaire, `atelier-registry-dev` du reste de la stack de dev convient) :

```sh
docker tag atelier-builder-vm-init:dev localhost:5000/atelier-builder-vm-init:dev
docker push localhost:5000/atelier-builder-vm-init:dev

mkdir -p /var/tmp/atelier-builder-vm
deploy/dev/crane/crane export localhost:5000/atelier-builder-vm-init:dev /var/tmp/atelier-builder-vm/rootfs.tar
mkdir -p /var/tmp/atelier-builder-vm/rootfs
tar xf /var/tmp/atelier-builder-vm/rootfs.tar -C /var/tmp/atelier-builder-vm/rootfs

SIZE_KB=$(du -sk /var/tmp/atelier-builder-vm/rootfs | cut -f1)
# Marge de 4096M (pas 512M) : un vrai build devcontainer (paquets apt/pip,
# ex. gcc pour un devcontainer Python) a besoin de bien plus que la seule
# taille de base de l'image builder-vm-init — 512M produisait un
# "no space left on device" en plein build, decouvert en testant reellement.
truncate -s $(( SIZE_KB/1024 + 4096 ))M /var/tmp/atelier-builder-vm/rootfs.ext4
mke2fs -F -t ext4 -d /var/tmp/atelier-builder-vm/rootfs /var/tmp/atelier-builder-vm/rootfs.ext4
```

Deja verifie cette session : le build Docker reussit, `crane export` +
`mke2fs` produisent un `rootfs.ext4` contenant bien
`/sbin/atelier-builder-vm-init` et `/.envbuilder/bin/envbuilder`.

## 2. Tester la mecanique reseau (deja valide, sans internet requis)

```sh
unshare --net --map-root-user -- \
  cargo test -p atelier-firecracker --test network -- --nocapture
```

Cree un vrai device TAP, verifie son IP/etat, le demonte. Ne necessite pas
de connectivite reelle (namespace reseau isole) — deja passe cette session.

## 3. Test complet (boot + envbuilder + push registre) : necessite un vrai `CAP_NET_ADMIN`

Le guest doit atteindre `net-proxy`, qui doit lui-meme atteindre Internet —
les deux ne peuvent pas etre vrais a la fois dans un `unshare --net` isole
(qui n'a pas de route de sortie). Sur une machine avec un vrai sudo :

```sh
# 1. Demarrer net-proxy sur la machine (pas dans un netns isole), allowlist
#    large pour ce test de dev :
ATELIER_EGRESS_ALLOWLIST='*' cargo run -p atelier-net-proxy &

# 2. Lancer le test avec un vrai CAP_NET_ADMIN (sudo, pas unshare) :
sudo -E env "PATH=$PATH" \
  ATELIER_TEST_FIRECRACKER_BIN=$PWD/deploy/dev/firecracker/assets/firecracker \
  ATELIER_TEST_JAILER_BIN=$PWD/deploy/dev/firecracker/assets/jailer \
  ATELIER_TEST_VM_KERNEL_PATH=$PWD/deploy/dev/firecracker/assets/vmlinux.bin \
  ATELIER_TEST_BUILDER_ROOTFS_PATH=/var/tmp/atelier-builder-vm/rootfs.ext4 \
  ATELIER_TEST_REGISTRY_ADDR=localhost:5000 \
  ATELIER_TEST_NET_PROXY_ADDR=0.0.0.0:3128 \
  cargo test -p atelier-firecracker --test builder_vm -- --nocapture
```

Sans sudo interactif disponible, un conteneur Docker `--privileged
--network host --device=/dev/kvm --device=/dev/net/tun` est une alternative
qui fonctionne tout aussi bien (voir `docs/PROGRESS.md`, "Lecons retenues")
— `--network host` partage directement le netns de l'hote, donc le TAP cree
dans le conteneur et `net-proxy` lance sur l'hote se voient mutuellement
sans configuration supplementaire. Compiler avec un `CARGO_TARGET_DIR`
dedie au conteneur (piege glibc, voir "Lecons retenues").

Assertions : la microVM s'eteint d'elle-meme (pas de canal de controle
vsock dans ce MVP — le succes se lit dans le registre, pas via un message
explicite du guest) et `crane manifest` confirme que l'image attendue y a
bien ete poussee par `envbuilder` execute a l'interieur du guest.

**Valide reellement** : cycle complet boot -> reseau -> `envbuilder`
(clone + build + push) -> extinction propre -> verification registre, en
35s (`test result: ok`). Cinq causes racines ont ete trouvees et corrigees
pour y arriver — voir `docs/PROGRESS.md`, section "Builder microVM", pour
le detail (chemin de socket jail trop long, `KANIKO_DIR` manquant, rootfs
trop petit, `localhost` exclu du proxy HTTP par le client Go, `reboot`
sans ACPI).

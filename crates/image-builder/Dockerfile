# Image du Job image-builder. Le devcontainer cible n'est plus construit
# "en place" dans CE conteneur (ancienne approche, invocation directe
# d'`envbuilder` — abandonnee) : `envbuilder` tourne desormais dans son
# propre noyau, a l'interieur d'une microVM Firecracker jetable
# (`crates/builder-vm-init`), demarree par `atelier-image-builder` via
# `atelier-firecracker`. Voir le commentaire de module de
# `crates/image-builder/src/main.rs` et `docs/PROGRESS.md` (section
# "Builder microVM") pour le detail du "pourquoi".
#
# Build multi-etapes : compile les deux binaires Rust necessaires
# (atelier-image-builder, atelier-builder-vm-init), construit le contenu
# guest de la microVM builder (meme contenu que
# `crates/builder-vm-init/Dockerfile`, duplique ici pour pouvoir
# l'aplatir directement via `COPY --from=` sans passer par un registre/
# `crane export` — un stage Docker multi-etapes fait deja ce travail), puis
# l'empaquette en `rootfs.ext4` de base (marge minimale : redimensionne a
# chaque build par `atelier-image-builder`, voir `resolve_builder_rootfs`
# dans `src/main.rs` — un vrai build devcontainer a besoin de bien plus que
# la taille de base).

FROM rust:1-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p atelier-image-builder -p atelier-builder-vm-init

# Contenu guest de la microVM builder (identique a
# crates/builder-vm-init/Dockerfile — garder les deux synchronises si l'un
# change).
FROM debian:bookworm-slim AS builder-vm-guest
RUN apt-get update \
    && apt-get install -y --no-install-recommends iproute2 ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=ghcr.io/coder/envbuilder:latest /.envbuilder/bin/envbuilder /.envbuilder/bin/envbuilder
COPY --from=builder /build/target/release/atelier-builder-vm-init /sbin/atelier-builder-vm-init
ENV KANIKO_DIR=/.envbuilder

# Aplatit le stage precedent en rootfs.ext4 bootable. `COPY --from=<stage> /`
# copie l'integralite du filesystem de ce stage sans passer par un registre
# OCI : equivalent local a `crane export` + `tar xf` (cf.
# deploy/dev/builder-vm/README.md pour la version manuelle avec un
# registre), inutile ici puisque le contenu est deja disponible localement
# dans le build multi-etapes.
FROM debian:bookworm-slim AS builder-vm-rootfs
RUN apt-get update \
    && apt-get install -y --no-install-recommends e2fsprogs \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder-vm-guest / /rootfs
RUN SIZE_KB=$(du -sk /rootfs | cut -f1) \
    && truncate -s $(( SIZE_KB/1024 + 512 ))M /builder-vm-rootfs.ext4 \
    && mke2fs -F -t ext4 -d /rootfs /builder-vm-rootfs.ext4

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       e2fsprogs tar ca-certificates libcap2-bin iproute2 iptables \
    && rm -rf /var/lib/apt/lists/*

COPY deploy/dev/crane/crane /usr/local/bin/crane
COPY deploy/dev/firecracker/assets/firecracker /usr/local/bin/firecracker
COPY deploy/dev/firecracker/assets/jailer /usr/local/bin/jailer
COPY deploy/dev/firecracker/assets/vmlinux.bin /opt/vmlinux.bin
COPY --from=builder-vm-rootfs /builder-vm-rootfs.ext4 /opt/builder-vm-rootfs-base.ext4
COPY --from=builder /build/target/release/atelier-image-builder /usr/local/bin/atelier-image-builder

# Capabilities dediees sur le jailer plutot que root complet — meme
# raisonnement que crates/vm-supervisor/Dockerfile : le SudoProcessSpawner
# de fctools invoque `sudo -S -s`, incompatible avec une regle sudoers
# scopee finement.
RUN setcap cap_sys_admin,cap_sys_resource,cap_sys_chroot,cap_setuid,cap_setgid,cap_mknod,cap_dac_override+eip \
    /usr/local/bin/jailer

ENV ATELIER_CRANE_BIN=/usr/local/bin/crane
ENV ATELIER_BUILDER_FIRECRACKER_BIN=/usr/local/bin/firecracker
ENV ATELIER_BUILDER_JAILER_BIN=/usr/local/bin/jailer
ENV ATELIER_BUILDER_VM_KERNEL_PATH=/opt/vmlinux.bin
ENV ATELIER_BUILDER_VM_ROOTFS_BASE_PATH=/opt/builder-vm-rootfs-base.ext4
ENV ATELIER_BUILDER_VM_CHROOT_BASE_DIR=/var/lib/atelier/builder-jailer

ENTRYPOINT ["/usr/local/bin/atelier-image-builder"]

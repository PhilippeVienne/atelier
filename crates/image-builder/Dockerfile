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
RUN cargo build --release -p atelier-image-builder -p atelier-builder-vm-init -p atelier-guest-init

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

# Binaire `opencode` telecharge ICI, au `docker build` (reseau normal du
# host qui construit cette image), jamais depuis l'interieur d'une microVM
# builder — voir `inject_opencode_binary` dans `src/main.rs` pour la
# justification complete (le tunnel CONNECT de `net-proxy` reste bloque
# sans erreur sur ce meme binaire quand on le telecharge depuis le guest).
FROM debian:bookworm-slim AS opencode-download
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl ca-certificates unzip \
    && rm -rf /var/lib/apt/lists/*
RUN curl -fsSL https://opencode.ai/install | bash \
    && cp "$HOME/.opencode/bin/opencode" /opencode

# Meme raisonnement qu'`opencode-download` ci-dessus, applique cette fois a
# ttyd (terminal web) et code-server (IDE web) : jusqu'ici ces deux
# composants n'etaient fournis QUE par le devcontainer de demo externe
# (`github.com/PhilippeVienne/atelier-workspace`), jamais par
# `image-builder` lui-meme — un Workshop sur n'importe quel autre depot
# cible ne passait donc jamais `Running` (sonde de readiness sur
# `ttyd:7681`, voir `crates/controller/src/reconcile.rs`) et n'avait ni
# terminal ni IDE. Binaires statiques (`ttyd.x86_64`) ou autonomes
# (`code-server` embarque son propre Node) : aucune dependance a apt/au
# gestionnaire de paquets de l'image de base cible.
FROM debian:bookworm-slim AS guest-tools-download
RUN apt-get update \
    && apt-get install -y --no-install-recommends curl ca-certificates tar \
    && rm -rf /var/lib/apt/lists/*
ARG TTYD_VERSION=1.7.7
ARG CODE_SERVER_VERSION=4.96.2
RUN curl -fsSL -o /ttyd \
        "https://github.com/tsl0922/ttyd/releases/download/${TTYD_VERSION}/ttyd.x86_64" \
    && chmod +x /ttyd
RUN curl -fsSL \
        "https://github.com/coder/code-server/releases/download/v${CODE_SERVER_VERSION}/code-server-${CODE_SERVER_VERSION}-linux-amd64.tar.gz" \
        | tar xz -C /opt \
    && mv "/opt/code-server-${CODE_SERVER_VERSION}-linux-amd64" /opt/code-server

# `sshd` n'a pas d'equivalent statique officiel : on l'installe ici (build
# `debian:bookworm-slim`, meme famille glibc que la grande majorite des
# images de devcontainer reelles) puis on l'embarque avec ses bibliotheques
# dynamiques (`ldd`), pour qu'il tourne sans dependre du gestionnaire de
# paquets de l'image CIBLE — meme raisonnement que `code-server`, qui
# embarque son propre Node plutot que de supposer qu'il existe deja.
# `UsePAM no` (voir `inject_sshd`, `src/main.rs`) evite d'avoir a embarquer
# toute la pile PAM (modules charges dynamiquement, non captures par `ldd`).
RUN apt-get update \
    && apt-get install -y --no-install-recommends openssh-server \
    && rm -rf /var/lib/apt/lists/*
RUN mkdir -p /opt/atelier-sshd/bin /opt/atelier-sshd/lib \
    && cp /usr/sbin/sshd /opt/atelier-sshd/bin/sshd \
    && cp /usr/bin/ssh-keygen /opt/atelier-sshd/bin/ssh-keygen \
    && { ldd /opt/atelier-sshd/bin/sshd; ldd /opt/atelier-sshd/bin/ssh-keygen; } \
       | grep -oE '/[^ ]+' | sort -u \
       | xargs -I{} cp -L {} /opt/atelier-sshd/lib/ \
    && interp=$(ldd /opt/atelier-sshd/bin/sshd | grep -oE '/lib.*ld-linux[^ ]*' | head -1) \
    && cp -L "$interp" /opt/atelier-sshd/bin/ld.so \
    && echo "$interp" > /opt/atelier-sshd/interp.txt

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       e2fsprogs tar ca-certificates libcap2-bin iproute2 iptables git \
    && rm -rf /var/lib/apt/lists/*

COPY --from=opencode-download /opencode /opt/opencode/opencode
COPY --from=guest-tools-download /ttyd /opt/atelier-guest/ttyd
COPY --from=guest-tools-download /opt/code-server /opt/atelier-guest/code-server
COPY --from=guest-tools-download /opt/atelier-sshd /opt/atelier-guest/sshd
COPY --from=builder /build/target/release/atelier-guest-init /opt/atelier-guest/atelier-guest-init
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
ENV ATELIER_OPENCODE_BIN=/opt/opencode/opencode
ENV ATELIER_TTYD_BIN=/opt/atelier-guest/ttyd
ENV ATELIER_CODE_SERVER_DIR=/opt/atelier-guest/code-server
ENV ATELIER_SSHD_DIR=/opt/atelier-guest/sshd
ENV ATELIER_GUEST_INIT_BIN=/opt/atelier-guest/atelier-guest-init

ENTRYPOINT ["/usr/local/bin/atelier-image-builder"]

# Rootfs de la microVM "builder" : contenu embarque dans une image OCI
# classique (comme n'importe quelle image Docker), puis convertie en disque
# bootable ext4 via le meme pipeline crane-export + mke2fs deja utilise par
# `image-builder` (voir deploy/dev/builder-vm/README.md) — pas de nouvel
# outil, juste le meme deja valide en pratique cette session.
#
# Contenu de confiance (notre propre Dockerfile) : construit via `docker
# build` classique, sans isolation particuliere — la protection visee par la
# microVM "builder" porte sur le contenu du DEPOT CIBLE du Workshop, execute
# a l'interieur d'elle une fois demarree, pas sur ce rootfs lui-meme.
#
# Build multi-etapes (pas de binaire hote copie tel quel) pour eviter tout
# decalage de version glibc entre l'etape de compilation et l'image finale —
# meme motif que crates/image-builder/Dockerfile.

FROM rust:1-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p atelier-builder-vm-init

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       iproute2 ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Meme chemin canonique qu'attend envbuilder que dans
# crates/image-builder/Dockerfile (il s'auto-embarque dans l'image
# construite depuis ce chemin exact).
COPY --from=ghcr.io/coder/envbuilder:latest /.envbuilder/bin/envbuilder /.envbuilder/bin/envbuilder
COPY --from=builder /build/target/release/atelier-builder-vm-init /sbin/atelier-builder-vm-init

# Meme garde-fou interne d'envbuilder (Kaniko) que dans
# crates/image-builder/Dockerfile : verifie qu'on tourne bien dans un
# environnement prevu pour ca avant de vider le systeme de fichiers.
ENV KANIKO_DIR=/.envbuilder

# Pas d'ENTRYPOINT : ce rootfs n'est jamais lance comme conteneur Docker,
# uniquement converti en disque ext4 bootable (voir README) puis demarre par
# Firecracker avec `boot_args: "... init=/sbin/atelier-builder-vm-init"`.

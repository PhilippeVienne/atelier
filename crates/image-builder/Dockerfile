# Image du Job image-builder : le devcontainer se construit "en place" sur
# le systeme de fichiers de CE conteneur (c'est le mode de fonctionnement
# d'envbuilder, verifie manuellement — pas de dossier d'export propre), donc
# atelier-image-builder doit tourner dans le meme conteneur qu'envbuilder,
# pas l'invoquer depuis un conteneur separe.
#
# Build multi-etapes (pas de binaire hote copie tel quel) pour eviter tout
# decalage de version glibc entre l'etape de compilation et l'image finale.

FROM rust:1-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p atelier-image-builder

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       e2fsprogs tar ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Chemin canonique attendu par envbuilder lui-meme : il s'auto-embarque
# dans l'image construite en se copiant depuis /.envbuilder/bin/envbuilder
# (constate en pratique — le deplacer ailleurs fait echouer le build avec
# "open /.envbuilder/bin/envbuilder: no such file or directory").
COPY --from=ghcr.io/coder/envbuilder:latest /.envbuilder/bin/envbuilder /.envbuilder/bin/envbuilder
COPY deploy/dev/crane/crane /usr/local/bin/crane
COPY --from=builder /build/target/release/atelier-image-builder /usr/local/bin/atelier-image-builder

ENV ATELIER_ENVBUILDER_BIN=/.envbuilder/bin/envbuilder
ENV ATELIER_CRANE_BIN=/usr/local/bin/crane
# Garde-fou interne d'envbuilder (Kaniko) avant de vider le systeme de
# fichiers pour y extraire l'image de base : verifie qu'on tourne bien dans
# un conteneur prevu pour ca, pas sur un hote reel.
ENV KANIKO_DIR=/.envbuilder

ENTRYPOINT ["/usr/local/bin/atelier-image-builder"]

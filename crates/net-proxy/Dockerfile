# Premiere conteneurisation de net-proxy (jusqu'ici teste uniquement via
# `cargo run`/tests d'integration reels, jamais deploye en pod — voir
# docs/PROGRESS.md). Contenu de confiance (notre propre code), build
# multi-etapes pour eviter tout decalage de version glibc entre l'etape de
# compilation et l'image finale (meme motif que les autres Dockerfile du
# projet).

FROM rust:1-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p atelier-net-proxy

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/atelier-net-proxy /usr/local/bin/atelier-net-proxy

ENTRYPOINT ["/usr/local/bin/atelier-net-proxy"]

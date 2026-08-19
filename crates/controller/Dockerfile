# Premiere conteneurisation du controller (jusqu'ici lance uniquement via
# `cargo run`/tests d'integration reels depuis le poste de dev, avec un
# kubeconfig admin local — jamais deploye en pod). Build multi-etapes pour
# eviter tout decalage de version glibc entre compilation et execution
# (meme motif que les autres Dockerfile du projet).

FROM rust:1-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p atelier-controller

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/atelier-controller /usr/local/bin/atelier-controller

ENTRYPOINT ["/usr/local/bin/atelier-controller"]

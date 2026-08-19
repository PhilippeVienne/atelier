# Device plugin Kubernetes pour /dev/kvm (+/dev/net/tun) : tourne en
# DaemonSet, s'enregistre aupres du kubelet de chaque noeud
# (/var/lib/kubelet/device-plugins/kubelet.sock) pour rendre ces devices
# allouables via resources.limits, sans pod privilegie.

FROM rust:1-bookworm AS builder
WORKDIR /build
RUN apt-get update \
    && apt-get install -y --no-install-recommends protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p atelier-kvm-device-plugin

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/atelier-kvm-device-plugin /usr/local/bin/atelier-kvm-device-plugin

ENTRYPOINT ["/usr/local/bin/atelier-kvm-device-plugin"]

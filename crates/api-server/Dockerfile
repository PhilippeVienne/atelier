FROM rust:1-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p atelier-api-server

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates openssl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/atelier-api-server /usr/local/bin/atelier-api-server

EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/atelier-api-server"]

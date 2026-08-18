# Image du pod parent : pilote une microVM Firecracker jailee. Necessite
# un pod privilegie + acces host a /dev/kvm (cf. deploy/dev/vm-supervisor/README.md
# pour le detail et la justification — pas de device plugin KVM dedie pour
# l'instant, connu et documente comme limite actuelle).

FROM rust:1-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p atelier-vm-supervisor

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends libcap2-bin ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY deploy/dev/firecracker/assets/firecracker /usr/local/bin/firecracker
COPY deploy/dev/firecracker/assets/jailer /usr/local/bin/jailer
COPY deploy/dev/firecracker/assets/vmlinux.bin /opt/vmlinux.bin
COPY --from=builder /build/target/release/atelier-vm-supervisor /usr/local/bin/atelier-vm-supervisor

# Capabilities dediees sur le jailer plutot que root complet — meme
# raisonnement que pour le dev sur poste (cf. deploy/dev/firecracker/README.md) :
# le SudoProcessSpawner de fctools invoque `sudo -S -s`, incompatible avec
# une regle sudoers scopee finement.
RUN setcap cap_sys_admin,cap_sys_resource,cap_sys_chroot,cap_setuid,cap_setgid,cap_mknod,cap_dac_override+eip \
    /usr/local/bin/jailer

ENV ATELIER_FIRECRACKER_BIN=/usr/local/bin/firecracker
ENV ATELIER_JAILER_BIN=/usr/local/bin/jailer
ENV ATELIER_SNAPSHOT_EDITOR_BIN=/bin/true
ENV ATELIER_VM_KERNEL_PATH=/opt/vmlinux.bin
ENV ATELIER_VM_CHROOT_BASE_DIR=/var/lib/atelier/jailer

ENTRYPOINT ["/usr/local/bin/atelier-vm-supervisor"]

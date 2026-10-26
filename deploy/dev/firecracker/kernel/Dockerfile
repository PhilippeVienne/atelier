# Noyau invite d'Atelier (tache 14.9, spec 19 §3.7) : la version du noyau de
# CI de Firecracker, avec la configuration de reference de Firecracker plus
# `atelier.config`. Construit dans un conteneur : la machine de developpement
# n'a pas forcement flex, bison ni libelf.
FROM debian:bookworm-slim AS build
ARG KERNEL_VERSION=5.10.223
ARG FC_VERSION=v1.16.1
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential flex bison bc python3 libelf-dev libssl-dev xz-utils curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
RUN curl -fsSL "https://cdn.kernel.org/pub/linux/kernel/v5.x/linux-${KERNEL_VERSION}.tar.xz" | tar xJ --strip-components=1
RUN curl -fsSL "https://raw.githubusercontent.com/firecracker-microvm/firecracker/${FC_VERSION}/resources/guest_configs/microvm-kernel-ci-x86_64-5.10.config" -o .config
COPY atelier.config /tmp/atelier.config
# Fusionne le fragment puis laisse `olddefconfig` resoudre les dependances ;
# on verifie ensuite que chaque option demandee a bien ete retenue.
RUN scripts/kconfig/merge_config.sh -m .config /tmp/atelier.config && make olddefconfig \
    && for opt in $(grep -o '^CONFIG_[A-Z0-9_]*' /tmp/atelier.config); do \
         grep -q "^${opt}=y" .config || echo "OPTION NON RETENUE: ${opt}"; done | tee /tmp/missing.txt
RUN make -j"$(nproc)" vmlinux
FROM scratch
COPY --from=build /src/vmlinux /vmlinux.bin
COPY --from=build /src/.config /kernel.config
COPY --from=build /tmp/missing.txt /options-non-retenues.txt

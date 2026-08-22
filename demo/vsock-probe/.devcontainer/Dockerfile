# Devcontainer minimal, dedie a la validation du transport vsock natif de
# `mcp-gateway` (voir docs/PROGRESS.md, section mcp-gateway). Meme piege
# systemd que les autres devcontainers de demo de ce dossier (systemctl
# enable est intercepte) : symlink d'activation cree a la main.
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       systemd systemd-sysv python3 ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY vsock_probe.py /usr/local/bin/vsock_probe.py
RUN chmod +x /usr/local/bin/vsock_probe.py

COPY atelier-vsock-probe.service /etc/systemd/system/atelier-vsock-probe.service
RUN ln -s /etc/systemd/system/atelier-vsock-probe.service \
    /etc/systemd/system/multi-user.target.wants/atelier-vsock-probe.service

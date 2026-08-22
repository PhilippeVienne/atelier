# Devcontainer minimal, dedie a la validation comportementale de
# l'injection HTTP_PROXY/resolv.conf faite par `image-builder`
# (`inject_net_proxy_config`, voir docs/PROGRESS.md). Meme piege systemd que
# `demo/ministack-workshop` (systemctl enable est intercepte) : symlink
# d'activation cree a la main.
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       systemd systemd-sysv curl netcat-openbsd ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY probe.sh /usr/local/bin/atelier-proxy-probe.sh
RUN chmod +x /usr/local/bin/atelier-proxy-probe.sh

COPY atelier-proxy-probe.service /etc/systemd/system/atelier-proxy-probe.service
RUN ln -s /etc/systemd/system/atelier-proxy-probe.service \
    /etc/systemd/system/multi-user.target.wants/atelier-proxy-probe.service

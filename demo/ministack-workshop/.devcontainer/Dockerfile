# Devcontainer de demo pour atelier : docker (via la feature
# docker-in-docker, voir devcontainer.json), ministack (emulateur AWS local,
# https://github.com/ministackorg/ministack) et code-server (VS Code dans le
# navigateur, cf. docs/ARCHITECTURE.md "dashboard" — reference coder/code-server).
# Le CLI Claude Code est installe via la feature officielle
# ghcr.io/anthropics/devcontainer-features/claude-code, pas ici.
FROM mcr.microsoft.com/devcontainers/base:ubuntu-24.04

ARG CODE_SERVER_VERSION=4.133.0

# systemd comme PID 1 : verifie reellement (voir docs/PROGRESS.md, section
# "boot Firecracker de l'agent") que sans ca, le noyau ne trouve aucun
# `/sbin/init`/`/etc/init`/`/bin/init` dans une image devcontainer standard
# et retombe sur un `/bin/sh` nu — rien ne demarre jamais tout seul.
# `postStartCommand` (devcontainer.json) est un concept du CLI
# `devcontainer`, jamais rejoue par le noyau : nos services sont donc
# demarres via une unite systemd (`atelier-demo.service`, plus bas), pas via
# `postStartCommand`.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        systemd systemd-sysv python3 python3-venv curl ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    # Unites qui ne servent a rien dans une microVM (pas de vrai materiel/
    # reseau ethernet a configurer par systemd-networkd, pas de getty sur
    # un vrai terminal) : masquees comme le fait tout guest minimal.
    && systemctl mask systemd-udevd.service systemd-udev-trigger.service \
        getty.target systemd-networkd.service systemd-resolved.service

# ministack dans un venv dedie (Ubuntu 24.04 refuse `pip install` sur le
# python systeme, PEP 668) ; on expose seulement son binaire dans le PATH.
RUN python3 -m venv /opt/ministack-venv \
    && /opt/ministack-venv/bin/pip install --no-cache-dir ministack \
    && ln -s /opt/ministack-venv/bin/ministack /usr/local/bin/ministack

# Paquet .deb officiel, version epinglee (pas de curl|sh) :
# https://github.com/coder/code-server/releases
RUN curl -fsSL -o /tmp/code-server.deb \
        "https://github.com/coder/code-server/releases/download/v${CODE_SERVER_VERSION}/code-server_${CODE_SERVER_VERSION}_amd64.deb" \
    && apt-get install -y --no-install-recommends /tmp/code-server.deb \
    && rm -f /tmp/code-server.deb

# Demarrage au boot via systemd (pas `postStartCommand`, jamais rejoue par
# le noyau — voir plus haut) : necessaire pour que ces services tournent
# reellement une fois ce devcontainer boote comme microVM Firecracker par
# `vm-supervisor`, pas seulement via le CLI `devcontainer`.
COPY atelier-ministack.service atelier-code-server.service /etc/systemd/system/
# `systemctl enable` ne fonctionne pas ici : cette image de base intercepte
# `systemctl` par un script factice ("systemd is not running in this
# container due to its overhead", constate en pratique) pour eviter des
# echecs de paquets .deb pendant un `docker build` classique. Les unites
# installees par des paquets (ex: docker.service) sont activees par
# `deb-systemd-helper` (appele par leurs scripts postinst), qui manipule
# directement les symlinks sans passer par ce faux `systemctl` — meme
# methode reprise ici a la main.
RUN mkdir -p /etc/systemd/system/multi-user.target.wants \
    && ln -s ../atelier-ministack.service /etc/systemd/system/multi-user.target.wants/atelier-ministack.service \
    && ln -s ../atelier-code-server.service /etc/systemd/system/multi-user.target.wants/atelier-code-server.service

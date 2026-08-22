# Devcontainer minimal, dedie a la validation du chemin de production
# complet pour mcp-gateway : un vrai client MCP a l'interieur du guest,
# qui n'a RIEN de special (pas d'acces host, pas de partage de netns) — il
# ne connait que HTTP_PROXY (injecte par image-builder) et l'alias
# "mcp-gateway" de net-proxy. Voir docs/PROGRESS.md, section mcp-gateway.
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       systemd systemd-sysv curl ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY mcp_probe.sh /usr/local/bin/mcp_probe.sh
RUN chmod +x /usr/local/bin/mcp_probe.sh

COPY atelier-mcp-agent-probe.service /etc/systemd/system/atelier-mcp-agent-probe.service
RUN ln -s /etc/systemd/system/atelier-mcp-agent-probe.service \
    /etc/systemd/system/multi-user.target.wants/atelier-mcp-agent-probe.service

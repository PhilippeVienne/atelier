#!/bin/sh
# Sonde MCP reelle : parle a mcp-gateway via l'alias "mcp-gateway" de
# net-proxy, en HTTP normal (curl lit HTTP_PROXY depuis l'environnement du
# process — herite de /etc/environment via systemd EnvironmentFile= si
# injecte par image-builder, cf. inject_net_proxy_config). Chemin de
# production complet : aucun raccourci (pas de vsock, pas de partage de
# netns/process avec l'hote) — exactement ce qu'un vrai client MCP dans le
# devcontainer emprunterait.
set -eu

RESULT=/tmp/atelier-mcp-agent-probe.log
URL=http://mcp-gateway/mcp

log() {
    echo "MCP_AGENT_PROBE: $1" >> "$RESULT"
    sync
}

log "starting http_proxy=${HTTP_PROXY:-<absent>}"

HEADERS=$(mktemp)
INIT_BODY='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"atelier-mcp-agent-probe","version":"0"}}}'

# mcp-gateway peut ne pas encore etre pret (ordre de demarrage des
# conteneurs d'un pod non synchronise) : quelques tentatives espacees
# plutot qu'un unique essai.
INIT_RESP=""
i=0
while [ "$i" -lt 15 ]; do
    if INIT_RESP=$(curl -sf -D "$HEADERS" -X POST "$URL" \
        -H "Content-Type: application/json" \
        -H "Accept: application/json, text/event-stream" \
        -d "$INIT_BODY"); then
        break
    fi
    i=$((i + 1))
    sleep 2
done
if [ -z "$INIT_RESP" ]; then
    log "FAIL initialize: toutes les tentatives ont echoue"
    exit 1
fi
log "STEP init_ok resp=$INIT_RESP"

SESSION=$(grep -i '^mcp-session-id:' "$HEADERS" | tr -d '\r' | cut -d: -f2 | tr -d ' ')
log "STEP session=$SESSION"

curl -sf -X POST "$URL" \
    -H "Content-Type: application/json" \
    -H "Accept: application/json, text/event-stream" \
    -H "mcp-session-id: $SESSION" \
    -d '{"jsonrpc":"2.0","method":"notifications/initialized"}' >/dev/null \
    || { log "FAIL initialized_notification: curl exit=$?"; exit 1; }
log "STEP initialized_notification_sent"

CALL_BODY='{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"request_egress","arguments":{"host":"example.com"}}}'
CALL_RESP=$(curl -sf -X POST "$URL" \
    -H "Content-Type: application/json" \
    -H "Accept: application/json, text/event-stream" \
    -H "mcp-session-id: $SESSION" \
    -d "$CALL_BODY") || { log "FAIL tools_call: curl exit=$?"; exit 1; }
log "OK tools_call resp=$CALL_RESP"

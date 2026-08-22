#!/usr/bin/env python3
"""Client MCP minimal, parle directement AF_VSOCK (cid=2 = l'hote) au port
ou mcp-gateway ecoute (voir ATELIER_MCP_GATEWAY_VSOCK_PORT, defaut 10000).
Framing JSON-RPC delimite par newline, identique a celui utilise par rmcp
cote hote (`rmcp::transport::async_rw::JsonRpcMessageCodec`) : pas de client
MCP tiers necessaire, juste socket + json de la stdlib.
"""
import json
import os
import socket
import sys
import time
import traceback

VSOCK_PORT = 10000
CONNECT_RETRIES = 20
CONNECT_DELAY_S = 2
RESULT_PATH = "/tmp/atelier-vsock-probe.log"


def log(msg):
    # fsync explicite : ce process peut etre tue abruptement juste apres
    # (VM eteinte sans arret propre pour l'inspection post-mortem via
    # debugfs) — sans ca, l'ecriture peut rester dans le cache page du
    # guest et ne jamais atteindre le fichier ext4 backing cote hote.
    with open(RESULT_PATH, "a") as f:
        f.write(f"VSOCK_MCP_RESULT: {msg}\n")
        f.flush()
        os.fsync(f.fileno())


def send(sock, obj):
    sock.sendall((json.dumps(obj) + "\n").encode())


def recv_line(sock, buf):
    while b"\n" not in buf:
        chunk = sock.recv(4096)
        if not chunk:
            raise EOFError("connexion fermee avant reception d'une ligne complete")
        buf += chunk
    line, _, rest = buf.partition(b"\n")
    return json.loads(line), rest


def connect_with_retries():
    """mcp-gateway peut ne pas encore avoir lie son socket au moment ou ce
    service demarre (ordre de demarrage non synchronise entre le guest et
    l'hote) : ECONNREFUSED est attendu au debut, pas une erreur definitive.
    """
    last_err = None
    for attempt in range(CONNECT_RETRIES):
        try:
            s = socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM)
            s.settimeout(10)
            s.connect((2, VSOCK_PORT))
            return s
        except OSError as exc:
            last_err = exc
            time.sleep(CONNECT_DELAY_S)
    raise last_err


def main():
    buf = b""
    log("STEP starting")
    try:
        s = connect_with_retries()
        log("STEP connected")

        send(s, {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "atelier-vsock-probe", "version": "0"},
            },
        })
        log("STEP initialize_sent")
        init_resp, buf = recv_line(s, buf)
        log("STEP init_ok resp=" + json.dumps(init_resp))

        send(s, {"jsonrpc": "2.0", "method": "notifications/initialized"})
        log("STEP initialized_notification_sent")

        send(s, {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
        log("STEP tools_list_sent")
        tools_resp, buf = recv_line(s, buf)

        log("OK init=" + json.dumps(init_resp) + " tools=" + json.dumps(tools_resp))
    except Exception as exc:  # noqa: BLE001 - sonde de diagnostic, on veut tout capturer
        log("FAIL " + repr(exc))
        log(traceback.format_exc())
        sys.exit(1)


if __name__ == "__main__":
    main()

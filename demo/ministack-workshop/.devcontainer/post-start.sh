#!/usr/bin/env bash
# Demarre ministack (emulateur AWS local) et code-server (VS Code dans le
# navigateur) en arriere-plan a chaque (re)demarrage du devcontainer.
# `postStartCommand` s'execute a chaque start, contrairement a
# `postCreateCommand` (une seule fois a la creation) : les deux services
# doivent redemarrer si le conteneur est arrete/relance.
set -euo pipefail

mkdir -p /tmp/atelier-demo-logs

nohup ministack >/tmp/atelier-demo-logs/ministack.log 2>&1 &
nohup code-server --bind-addr 0.0.0.0:8080 --auth none >/tmp/atelier-demo-logs/code-server.log 2>&1 &

echo "ministack et code-server demarres en arriere-plan (logs dans /tmp/atelier-demo-logs)."

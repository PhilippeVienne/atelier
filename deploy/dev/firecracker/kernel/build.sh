#!/usr/bin/env bash
# Construit le noyau invite d'Atelier dans Docker et le depose dans
# `deploy/dev/firecracker/assets/` (ignore par git). Le noyau de CI d'origine
# est conserve sous `vmlinux-ci.bin`. Dure une dizaine de minutes.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
ASSETS="$(cd .. && pwd)/assets"
OUT="$(mktemp -d)"
docker build --output "type=local,dest=$OUT" .
[ -f "$ASSETS/vmlinux-ci.bin" ] || cp "$ASSETS/vmlinux.bin" "$ASSETS/vmlinux-ci.bin"
cp "$OUT/vmlinux.bin" "$ASSETS/vmlinux.bin"
cp "$OUT/kernel.config" "$ASSETS/vmlinux.config"
echo "Options demandees mais non retenues par le noyau :"
cat "$OUT/options-non-retenues.txt"
echo "OK: $ASSETS/vmlinux.bin"

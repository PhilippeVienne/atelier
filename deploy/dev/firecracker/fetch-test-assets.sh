#!/usr/bin/env bash
# Recupere les binaires Firecracker/jailer et un couple noyau+rootfs de test
# (les artefacts CI officiels du projet Firecracker) pour le developpement
# et les tests d'integration de crates/vm-supervisor. Rien de ceci n'est
# commite (gros fichiers binaires) : voir deploy/dev/firecracker/.gitignore.
set -euo pipefail

FC_VERSION="v1.16.1"
KERNEL_VERSION="v1.10"
ASSETS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/assets"
mkdir -p "$ASSETS_DIR"
cd "$ASSETS_DIR"

echo "==> Firecracker/jailer ${FC_VERSION}"
curl -sL "https://github.com/firecracker-microvm/firecracker/releases/download/${FC_VERSION}/firecracker-${FC_VERSION}-x86_64.tgz" -o fc.tgz
tar xzf fc.tgz "release-${FC_VERSION}-x86_64/firecracker-${FC_VERSION}-x86_64" "release-${FC_VERSION}-x86_64/jailer-${FC_VERSION}-x86_64"
cp "release-${FC_VERSION}-x86_64/firecracker-${FC_VERSION}-x86_64" firecracker
cp "release-${FC_VERSION}-x86_64/jailer-${FC_VERSION}-x86_64" jailer
chmod +x firecracker jailer
rm -rf fc.tgz "release-${FC_VERSION}-x86_64"

echo "==> Noyau de test (${KERNEL_VERSION})"
curl -sL "https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/${KERNEL_VERSION}/x86_64/vmlinux-5.10.223" -o vmlinux.bin

echo "==> Rootfs de test (Ubuntu 22.04, ~300 Mo)"
curl -sL "https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/${KERNEL_VERSION}/x86_64/ubuntu-22.04.ext4" -o rootfs.ext4

echo "==> OK, artefacts dans $ASSETS_DIR"
echo
echo "export ATELIER_TEST_FIRECRACKER_BIN=$ASSETS_DIR/firecracker"
echo "export ATELIER_TEST_VM_KERNEL_PATH=$ASSETS_DIR/vmlinux.bin"
echo "export ATELIER_TEST_VM_ROOTFS_PATH=$ASSETS_DIR/rootfs.ext4"

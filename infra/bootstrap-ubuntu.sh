#!/usr/bin/env bash
# Run as root on a clean Ubuntu 24.04 runtime host. Review before execution.
set -euo pipefail
. /etc/os-release
[[ "$ID" == ubuntu && "$VERSION_ID" == 24.04 && "$EUID" == 0 ]]
export DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=a
apt-get update
apt-get -y -o Dpkg::Options::=--force-confold upgrade --with-new-pkgs
apt-get install -y ca-certificates curl gnupg jq rsync unzip git nginx openssl python3 ufw unattended-upgrades
install -d -m 0755 /etc/apt/keyrings
curl -fsSL https://download.docker.com/linux/ubuntu/gpg -o /etc/apt/keyrings/docker.asc
chmod a+r /etc/apt/keyrings/docker.asc
cat > /etc/apt/sources.list.d/docker.sources <<EOF
Types: deb
URIs: https://download.docker.com/linux/ubuntu
Suites: noble
Components: stable
Architectures: $(dpkg --print-architecture)
Signed-By: /etc/apt/keyrings/docker.asc
EOF
apt-get update
apt-get install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
systemctl enable --now docker nginx unattended-upgrades
# Firewall and NPM address are configured separately after route verification.
# This script neither disables SSH password login nor changes existing firewall rules.

#!/usr/bin/env bash
set -euo pipefail

printf '1\n' >/proc/sys/net/ipv4/ip_forward

if ! iptables -w -C FORWARD -i tap+ -j ACCEPT 2>/dev/null; then
  iptables -w -A FORWARD -i tap+ -j ACCEPT
fi
if ! iptables -w -C FORWARD -o tap+ -m conntrack \
  --ctstate ESTABLISHED,RELATED -j ACCEPT 2>/dev/null; then
  iptables -w -A FORWARD -o tap+ -m conntrack \
    --ctstate ESTABLISHED,RELATED -j ACCEPT
fi
if ! iptables -w -t nat -C POSTROUTING -s 172.16.0.0/24 \
  -j MASQUERADE 2>/dev/null; then
  iptables -w -t nat -A POSTROUTING -s 172.16.0.0/24 \
    -j MASQUERADE
fi

echo "Firecracker TAP forwarding and NAT are enabled"

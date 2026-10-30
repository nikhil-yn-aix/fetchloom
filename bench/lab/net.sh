#!/usr/bin/env bash
# shapes what the lab server sends, TCP from port 8443, on loopback for WSL clients and on the
# interface windows reaches. the delay is one way, on the server's packets only, so it adds
# exactly that much to each round trip. run as root: net.sh lan|fast|home|far|poor|clear
set -euo pipefail

case ${1:-} in
  lan | clear) netem= ;;
  fast) netem='rate 1gbit delay 5ms' ;;
  home) netem='rate 100mbit delay 20ms' ;;
  far) netem='rate 20mbit delay 100ms loss 1%' ;;
  poor) netem='rate 5mbit delay 250ms 20ms loss 2%' ;;
  *) echo 'usage: net.sh lan|fast|home|far|poor|clear' >&2; exit 2 ;;
esac

wan=$(ip route show default | awk '{print $5; exit}')
# netem drops a segmentation offload batch whole, so loss needs offload off to hit single
# packets. without loss it stays on, since the VM's eth0 then carries 1 Gbit/s to windows.
offload=on
[[ $netem == *loss* ]] && offload=off
for dev in lo "$wan"; do
  tc qdisc del dev "$dev" root 2>/dev/null || true
  ethtool -K "$dev" tso $offload gso $offload >/dev/null
  [[ -z $netem ]] && continue
  tc qdisc add dev "$dev" root handle 1: prio bands 2 priomap 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
  tc qdisc add dev "$dev" parent 1:2 netem $netem
  for proto in ip ipv6; do
    tc filter add dev "$dev" parent 1: protocol $proto flower ip_proto tcp src_port 8443 classid 1:2
  done
done

# TCP counts its window in packets, so loopback's 64 KiB packets would outrun a real path.
# saved metrics would start a connection from what an earlier profile taught TCP.
if [[ -n $netem ]]; then
  ip link set lo mtu 1500
  sysctl -qw net.ipv4.tcp_no_metrics_save=1
else
  ip link set lo mtu 65536
  sysctl -qw net.ipv4.tcp_no_metrics_save=0
fi
ip tcp_metrics flush all 2>/dev/null || true
echo "$1 on lo and $wan${netem:+: $netem}"

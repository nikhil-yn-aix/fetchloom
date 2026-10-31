#!/usr/bin/env bash
# serves $LAB_DATA (or $SUDO_USER's ~/lab-data) over HTTPS on port 8443 with HTTP/1.1, HTTP/2,
# byte ranges and sendfile. run as root. the first run makes a local CA in $LAB_DATA/.lab and
# trusts it in this system's store. the CA's revocation list is served over HTTP on port 8442.
# given a path, windows or linux style, it copies the CA certificate there for windows.
# `serve.sh stop` stops the server.
set -euo pipefail

if [[ -n ${LAB_DATA:-} ]]; then
  data=$LAB_DATA
elif [[ -n ${SUDO_USER:-} && $SUDO_USER != root ]]; then
  owner_home=$(getent passwd "$SUDO_USER" | cut -d: -f6) || {
    echo "cannot find the home for SUDO_USER=$SUDO_USER; set LAB_DATA" >&2
    exit 1
  }
  [[ -n $owner_home ]] || { echo "SUDO_USER=$SUDO_USER has no home; set LAB_DATA" >&2; exit 1; }
  data=$owner_home/lab-data
else
  echo "set LAB_DATA to the fixture folder, or run with sudo as the data's owner" >&2
  exit 1
fi
lab=$data/.lab
conf=$lab/nginx.conf

if [[ ${1:-} == stop ]]; then
  [[ -e $lab/nginx.pid ]] && kill "$(cat "$lab/nginx.pid")"
  exit 0
fi

command -v nginx >/dev/null || { echo "install nginx first: apt-get install nginx" >&2; exit 1; }
mkdir -p "$lab/tmp"
chmod 711 "$lab"
touch "$lab/index.txt"
wan=$(ip route show default | awk '{print $5; exit}')
ip4=$(ip -4 -br addr show dev "$wan" | awk '{print $3}' | cut -d/ -f1)

# the CA may only vouch for loopback and private addresses, which limits harm if its key leaks
cat > "$lab/openssl.cnf" <<EOF
[req]
distinguished_name = dn
[dn]
[lab_ca]
basicConstraints = critical, CA:true, pathlen:0
keyUsage = critical, keyCertSign, cRLSign
subjectKeyIdentifier = hash
nameConstraints = critical, @lab_names
[lab_names]
permitted;DNS.0 = localhost
permitted;IP.0 = 127.0.0.0/255.0.0.0
permitted;IP.1 = 10.0.0.0/255.0.0.0
permitted;IP.2 = 172.16.0.0/255.240.0.0
permitted;IP.3 = 192.168.0.0/255.255.0.0
permitted;IP.4 = ::1/ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff
[lab_server]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = serverAuth
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid
subjectAltName = DNS:localhost, IP:127.0.0.1, IP:::1, IP:$ip4
# schannel, as in curl.exe, refuses a certificate whose revocation it cannot check
crlDistributionPoints = URI:http://$ip4:8442/ca.crl
[lab_crl]
database = $lab/index.txt
default_md = sha256
EOF

if [[ ! -e $lab/ca.crt ]]; then
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -noenc -days 3650 \
    -subj "/CN=fetchloom lab ca" -config "$lab/openssl.cnf" -extensions lab_ca \
    -keyout "$lab/ca.key" -out "$lab/ca.crt" 2>/dev/null
fi
# a new server certificate every start, since the address of eth0 changes when WSL restarts
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -noenc -days 365 \
  -subj "/CN=fetchloom lab" -config "$lab/openssl.cnf" -extensions lab_server \
  -CA "$lab/ca.crt" -CAkey "$lab/ca.key" -keyout "$lab/server.key" -out "$lab/server.crt" 2>/dev/null
openssl ca -batch -config "$lab/openssl.cnf" -name lab_crl -gencrl -crldays 365 \
  -cert "$lab/ca.crt" -keyfile "$lab/ca.key" 2>/dev/null | openssl crl -outform der -out "$lab/ca.crl"

trusted=/usr/local/share/ca-certificates/fetchloom-lab.crt
if ! cmp -s "$lab/ca.crt" "$trusted"; then
  install -m 644 "$lab/ca.crt" "$trusted"
  update-ca-certificates >/dev/null 2>&1
fi
if [[ -n ${1:-} ]]; then
  out=$1
  [[ $out == [A-Za-z]:* ]] && out=$(wslpath -u "$out")
  cp "$lab/ca.crt" "$out"
fi

cat > "$conf" <<EOF
user $(stat -c %U "$data");
worker_processes auto;
pid $lab/nginx.pid;
error_log $lab/error.log;
events { worker_connections 4096; }
http {
  access_log off;
  sendfile on;
  tcp_nopush on;
  keepalive_requests 100000;
  default_type application/octet-stream;
  client_body_temp_path $lab/tmp/body;
  proxy_temp_path $lab/tmp/proxy;
  fastcgi_temp_path $lab/tmp/fastcgi;
  uwsgi_temp_path $lab/tmp/uwsgi;
  scgi_temp_path $lab/tmp/scgi;
  ssl_certificate $lab/server.crt;
  ssl_certificate_key $lab/server.key;
  ssl_session_cache shared:lab:10m;
  # kernel TLS lets sendfile work under TLS
  ssl_conf_command Options KTLS;
  server {
    listen 8443 ssl http2;
    listen [::]:8443 ssl http2;
    root $data;
    location ~ /\. { return 404; }
  }
  server {
    listen 8442;
    listen [::]:8442;
    root $lab;
    location / { return 404; }
    location = /ca.crl { }
  }
}
EOF

modprobe tls 2>/dev/null || true
nginx -t -q -c "$conf"
if [[ -e $lab/nginx.pid ]] && kill -0 "$(cat "$lab/nginx.pid")" 2>/dev/null; then
  nginx -c "$conf" -s reload 2>/dev/null
else
  nginx -c "$conf"
fi
curl -sS -o /dev/null https://localhost:8443/file-1m.bin
echo "serving $data at https://localhost:8443/ and https://$ip4:8443/"

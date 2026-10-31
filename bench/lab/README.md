# network lab

an HTTPS file server in WSL for fetchloom's benchmark. `tc` shapes what it sends to the profiles below, for clients in WSL and on windows. the examples use the distro `Ubuntu-24.04`, data owned by your regular WSL user, and WSL's default NAT networking.

## set up once

as root in the distro:

```sh
apt-get install -y nginx zip zstd ethtool
systemctl disable --now nginx
```

then, as your regular WSL user, build the fixtures in `$HOME/lab-data` (`LAB_DATA` changes the folder):

```sh
bash bench/lab/fixtures.sh
```

a first run took 3.5 to 8 minutes here, depending on what else ran; later runs keep what exists and only print the hashes, in about 25 s. every byte is AES-128-CTR keystream keyed by a fixed seed and the fixture's name, so the data does not compress and any machine builds the same files. modes and times are fixed, so the archives repeat too, given the same zip, tar, gzip and zstd.

| fixture | content |
|---|---|
| `file-1m.bin`, `file-10m.bin`, `file-100m.bin`, `file-1g.bin` | 1 MiB, 10 MiB, 100 MiB and 1 GiB |
| `files-10k/` | 10,000 files of 4 KiB, in 100 folders of 100 |
| `many-1k/` | 1,000 files of 100 KiB |
| `files-10k.zip` | `files-10k/`, members stored |
| `files-10k.tar.zst` | `files-10k/`, zstd level 3 |
| `big-members.zip` | 4 members of 128 MiB, stored |
| `big.tar.gz` | one member of 512 MiB |

the hashes it prints on ubuntu 24.04, where a folder's line is the sha256 of `sha256sum` run inside it over its files in path order:

```
cfaf41392d47f3be7910061b255d8ac99cc5e29f928b3f17bc308867464e82c0  file-1m.bin
1979e644a83ab15cc1adc5375ae9e54b7a0cb2bd19a058ebb4c14cf3ebd45a94  file-10m.bin
9ca298c636580e067e9bab3d088ef222730d2f7005f82a98c6d58144a26de684  file-100m.bin
a7309af845dd72ffa8534fc48ddaf4335f483c10cfef669e080a45b7619a409e  file-1g.bin
1b59fd102a6ac655ce47e368edfd3ee0b2443e8be5e858d880064b374907f2ee  files-10k.zip
e953a6f6f59ca231677675fdd25c4b92313b591f74ce9d1baad91502ccd70873  files-10k.tar.zst
b602c66105b96c84a6b5a69c72b8d196868dbf46eff4f07ba0a49f4e88582303  big-members.zip
1f503d89043d5c683ee60f9f7b8db1cb7f1b526b174fddb2eb3ae6e477ca08ac  big.tar.gz
ebe7ddac5fed3acb92c9827c84e12a477c43013e7029660d82b77d93bd631a47  files-10k/
589300f0fee195995a01944d6154542e474bfc2ecc486f851c83bc36487658a0  many-1k/
```

## start and stop

from powershell in the repository:

```powershell
$labData = "/home/<wsl-user>/lab-data"
$env:FETCHLOOM_LAB_CA = "$env:LOCALAPPDATA\fetchloom-work\lab-ca.crt"
wsl -d Ubuntu-24.04 -u root -e env "LAB_DATA=$labData" bash bench/lab/serve.sh $env:FETCHLOOM_LAB_CA
wsl -d Ubuntu-24.04 -u root -e env "LAB_DATA=$labData" bash bench/lab/serve.sh stop
```

`-e` hands the windows path to the script as it is; without it, WSL runs the line through a shell that drops the backslashes. the CA output path is optional.

serve.sh runs as root: set `LAB_DATA` explicitly when starting it directly as root, as above. when called with `sudo` from the data owner's account, it instead looks up `SUDO_USER`'s home and defaults to that home's `lab-data`; it never defaults to root's home. use the same setting to stop the server. for a custom folder under sudo, pass it explicitly with `sudo env LAB_DATA="$LAB_DATA" bash bench/lab/serve.sh`.

serve.sh runs nginx with its master as root and its workers as the owner of the data:

- port 8443 serves `$LAB_DATA` over HTTPS with HTTP/1.1 and HTTP/2, byte ranges, and sendfile through kernel TLS for HTTP/1.1.
- port 8442 serves only the CA's revocation list, over HTTP. schannel, and so curl.exe, refuses a certificate whose revocation it cannot check.
- `$LAB_DATA/.lab` holds the CA, the keys, the nginx config and its error log. paths starting with a dot return 404, and the keys are readable by root alone.

the first run makes a CA that can only vouch for `localhost`, loopback and private addresses, and adds it to WSL's store with update-ca-certificates. every run issues a new server certificate for `localhost`, `127.0.0.1`, `::1` and eth0's address, which changes when WSL restarts, and copies the CA certificate to the given path. run serve.sh again after WSL restarts; the shaping is gone by then too.

trust the CA on windows once, for your user, without admin:

```powershell
certutil -user -addstore Root $env:FETCHLOOM_LAB_CA
```

windows may ask you to confirm with a security warning. `certutil -user -delstore Root "fetchloom lab ca"` removes it.

check it from windows with certificate checking on, using the address serve.sh prints:

```powershell
$env:FETCHLOOM_LAB = "https://<wsl-eth0-address>:8443"
curl.exe "$env:FETCHLOOM_LAB/file-1m.bin" -o NUL
curl.exe https://localhost:8443/file-1m.bin -o NUL
```

## shape

```powershell
wsl -d Ubuntu-24.04 -u root -e bash bench/lab/net.sh home
wsl -d Ubuntu-24.04 -u root -e bash bench/lab/net.sh clear
```

| profile | rate | delay, one way | jitter | loss |
|---|---|---|---|---|
| `lan` | unshaped | | | |
| `fast` | 1 Gbit/s | 5 ms | | |
| `home` | 100 Mbit/s | 20 ms | | |
| `far` | 20 Mbit/s | 100 ms | | 1% |
| `poor` | 5 Mbit/s | 250 ms | 20 ms | 2% |

`lan` and `clear` both remove the shaping.

net.sh puts a netem queue on what the server sends, TCP from port 8443 over IPv4 and IPv6, on `lo` for WSL clients and on the default route's interface, `eth0`, for windows clients. requests and acknowledgements pass unshaped. the delay is one way, on the server's egress, so a round trip grows by the delay itself: `home` gives a 20 ms round trip, not 40 ms.

while a profile is on:

- `lo` has a 1500 byte MTU. TCP counts its window in packets, so loopback's usual 64 KiB packets would carry 44 times the bytes per round trip of a real path.
- `far` and `poor` turn segmentation offload off on both interfaces, since netem drops a whole offload batch at once and their loss has to hit single packets. `fast` and `home` keep it on: without it, eth0 carried 220 to 420 Mbit/s to windows at `fast`, against about 800 with it.
- TCP does not save per destination metrics and net.sh flushes them, so a connection does not start from what an earlier profile taught TCP.

`clear` restores the MTU, offload and metrics.

## windows through localhost

shaping applies to both windows addresses. with WSL's NAT networking, a windows connection to `localhost:8443` is accepted by WSL's relay on windows, which opens its own connection to the server over loopback inside the VM: `ss` in WSL shows the peer as `[::1]`, with a 20.1 ms minimum round trip under `home`. so `lo`'s shaping covers it, while the eth0 address goes through `eth0`'s.

the relay changes the timing. its connect returns at once, and the relay's own connect then costs one delay during the TLS handshake. after the request, the first byte took two delays through the relay against one over eth0, under every profile. unshaped, the relay carried 0.4 to 1.5 Gbit/s against 1.0 to 2.7 over eth0 across three runs. prefer the eth0 address for timing.

## measured

medians of three runs on 2026-10-30, on an 11th gen intel i7-1185G7 laptop: windows 11 build 26200, WSL 3.0.4 with kernel 6.18.54.1, ubuntu 24.04, nginx 1.24.0 with OpenSSL 3.0.13. five fuzzing jobs held five of the eight threads in WSL for most of the run, which mostly lowers the unshaped `lan` row: on an idle machine WSL reached 4,800 Mbit/s there.

throughput is curl's average download speed for `file-100m.bin`, stopped after 20 s. `far` and `poor` never finish in that time, so theirs is what arrived in 20 s, slow start included. with random loss, TCP's CUBIC, the default here, stays far below the link rate.

| profile | WSL, Mbit/s | windows by eth0 address | windows by localhost |
|---|---|---|---|
| `lan` | 2,429 | 1,698 | 1,253 |
| `fast` | 837 | 779 | 849 |
| `home` | 92.8 | 94.4 | 92.4 |
| `far` | 2.1 | 2.1 | 2.0 |
| `poor` | 0.6 | 0.7 | 0.5 |

time to first byte is curl's `time_starttransfer` for `file-1m.bin` on a new connection: the TCP connect, a TLS 1.3 handshake and the request, so at least three delays. on windows, the `lan` row is local work: the connect to the VM and schannel's handshake with its revocation check.

| profile | WSL, ms | windows by eth0 address | windows by localhost |
|---|---|---|---|
| `lan` | 31 | 116 | 44 |
| `fast` | 31 | 62 | 61 |
| `home` | 69 | 165 | 117 |
| `far` | 337 | 386 | 478 |
| `poor` | 795 | 984 | 1,087 |

windows times to first byte varied by up to 3.5 times between three passes over all profiles, while builds ran on the laptop; the shaped WSL rows stayed within about 10%.

both sides use HTTP/1.1, since curl.exe has no HTTP/2. WSL's curl gets `--cacert` with the lab CA, since curl with OpenSSL 3.0 spends about 70 ms loading the system store. a 3 s pause between the two measurements lets netem's queue drain.

```sh
curl -sS --http1.1 --cacert /usr/local/share/ca-certificates/fetchloom-lab.crt -o /dev/null --max-time 20 -w '%{speed_download}\n' https://localhost:8443/file-100m.bin
curl -sS --http1.1 --cacert /usr/local/share/ca-certificates/fetchloom-lab.crt -o /dev/null -w '%{time_starttransfer}\n' https://localhost:8443/file-1m.bin
```

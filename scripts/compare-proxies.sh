#!/usr/bin/env bash
set -Eeuo pipefail

REQUESTS=50
CONCURRENCY=10
PAYLOAD_MIB=64
OUTPUT="benchmark-results.csv"
PPROXY="${PPROXY:-pproxy}"
RUST_BINARY="${RUST_BINARY:-target/release/rust-sproxy}"
PROTOCOL="both"

usage() {
    cat <<'EOF'
Compare rust-sproxy with Python pproxy using equivalent direct routes.

Usage: scripts/compare-proxies.sh [options]

Options:
  --pproxy PATH         pproxy executable (default: pproxy)
  --rust-binary PATH    rust-sproxy executable (default: target/release/rust-sproxy)
  --protocol VALUE      socks5, http, or both (default: both)
  --requests N          requests per latency/concurrency test (default: 50)
  --concurrency N       simultaneous requests (default: 10)
  --payload-mib N       throughput payload size in MiB (default: 64)
  --output PATH         CSV result path (default: benchmark-results.csv)
  -h, --help            show help

Required commands: bash, curl, python3, awk, sort, ps, getconf, truncate.
Install pproxy in a virtual environment, then pass its executable:

  python3 -m venv /tmp/pproxy-venv
  /tmp/pproxy-venv/bin/pip install pproxy uvloop
  scripts/compare-proxies.sh --pproxy /tmp/pproxy-venv/bin/pproxy

Results measure local proxy overhead. Run on an idle machine and repeat several
times. Local Python HTTP server can become throughput bottleneck, so compare
relative results rather than treating them as network capacity.
EOF
}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

while (($#)); do
    case "$1" in
        --pproxy)
            (($# >= 2)) || die "--pproxy requires a value"
            PPROXY="$2"
            shift 2
            ;;
        --rust-binary)
            (($# >= 2)) || die "--rust-binary requires a value"
            RUST_BINARY="$2"
            shift 2
            ;;
        --protocol)
            (($# >= 2)) || die "--protocol requires a value"
            PROTOCOL="$2"
            shift 2
            ;;
        --requests)
            (($# >= 2)) || die "--requests requires a value"
            REQUESTS="$2"
            shift 2
            ;;
        --concurrency)
            (($# >= 2)) || die "--concurrency requires a value"
            CONCURRENCY="$2"
            shift 2
            ;;
        --payload-mib)
            (($# >= 2)) || die "--payload-mib requires a value"
            PAYLOAD_MIB="$2"
            shift 2
            ;;
        --output)
            (($# >= 2)) || die "--output requires a value"
            OUTPUT="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *) die "unknown option: $1" ;;
    esac
done

[[ "$PROTOCOL" =~ ^(socks5|http|both)$ ]] || die "--protocol must be socks5, http, or both"
for value in "$REQUESTS" "$CONCURRENCY" "$PAYLOAD_MIB"; do
    [[ "$value" =~ ^[1-9][0-9]*$ ]] || die "numeric options must be positive integers"
done

for command in curl python3 awk sort ps getconf truncate; do
    command -v "$command" >/dev/null || die "required command not found: $command"
done
command -v "$PPROXY" >/dev/null || die "pproxy not found: $PPROXY"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "$RUST_BINARY" != /* ]]; then
    RUST_BINARY="$ROOT/$RUST_BINARY"
fi
if [[ ! -x "$RUST_BINARY" ]]; then
    command -v cargo >/dev/null || die "cargo is required to build missing Rust binary"
    printf 'Building release binary...\n'
    cargo build --release --manifest-path "$ROOT/Cargo.toml"
fi
[[ -x "$RUST_BINARY" ]] || die "Rust binary is not executable: $RUST_BINARY"

TMP="$(mktemp -d)"
TARGET_PID=""
PROXY_PID=""
MONITOR_PID=""

terminate() {
    local pid="${1:-}"
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    fi
}

cleanup() {
    terminate "$MONITOR_PID"
    terminate "$PROXY_PID"
    terminate "$TARGET_PID"
    rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

free_port() {
    python3 - <<'PY'
import socket
with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
}

wait_for_port() {
    local port="$1"
    local attempts=100
    while ((attempts--)); do
        if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
            exec 3>&-
            exec 3<&-
            return 0
        fi
        sleep 0.05
    done
    return 1
}

truncate -s 1024 "$TMP/small.bin"
truncate -s "$((PAYLOAD_MIB * 1024 * 1024))" "$TMP/payload.bin"
TARGET_PORT="$(free_port)"
python3 - "$TARGET_PORT" "$TMP" >"$TMP/target.log" 2>&1 <<'PY' &
import functools
import http.server
import sys

class BenchmarkServer(http.server.ThreadingHTTPServer):
    request_queue_size = 4096
    daemon_threads = True

handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=sys.argv[2])
BenchmarkServer(("127.0.0.1", int(sys.argv[1])), handler).serve_forever()
PY
TARGET_PID=$!
wait_for_port "$TARGET_PORT" || die "target server failed to start; see $TMP/target.log"

printf 'upstreams = []\n' >"$TMP/sproxy.toml"

start_proxy() {
    local implementation="$1"
    local protocol="$2"
    local port="$3"
    local log="$TMP/${implementation}-${protocol}.log"

    if [[ "$implementation" == "rust" ]]; then
        "$RUST_BINARY" --type "$protocol" --listen "127.0.0.1:$port" \
            --config "$TMP/sproxy.toml" --max-connections "$((CONCURRENCY + 32))" \
            >"$log" 2>&1 &
    else
        "$PPROXY" -l "${protocol}://127.0.0.1:$port" >"$log" 2>&1 &
    fi
    PROXY_PID=$!
    if ! wait_for_port "$port"; then
        printf 'Proxy log:\n' >&2
        cat "$log" >&2
        die "$implementation $protocol proxy failed to start"
    fi
}

request() {
    local protocol="$1"
    local port="$2"
    local path="$3"
    local format="$4"
    local proxy="http://127.0.0.1:$port"
    if [[ "$protocol" == "socks5" ]]; then
        proxy="socks5h://127.0.0.1:$port"
        curl --silent --output /dev/null --max-time 30 --noproxy '' --proxy "$proxy" \
            --write-out "$format" "http://127.0.0.1:$TARGET_PORT/$path"
    else
        curl --silent --output /dev/null --max-time 30 --noproxy '' --proxy "$proxy" \
            --proxytunnel --write-out "$format" "http://127.0.0.1:$TARGET_PORT/$path"
    fi
}

cpu_ticks() {
    awk '{print $14 + $15}' "/proc/$1/stat" 2>/dev/null || printf '0\n'
}

monitor_rss() {
    local pid="$1"
    local stop_file="$2"
    local result_file="$3"
    local maximum=0
    local current
    while kill -0 "$pid" 2>/dev/null && [[ ! -e "$stop_file" ]]; do
        current="$(ps -o rss= -p "$pid" 2>/dev/null | awk '{print $1}')"
        current="${current:-0}"
        ((current > maximum)) && maximum="$current"
        sleep 0.05
    done
    printf '%s\n' "$maximum" >"$result_file"
}

summarize_latencies() {
    local input="$1"
    local sorted="$2"
    awk -F, '$2 == 200 {print $1 * 1000}' "$input" | sort -n >"$sorted"
    awk '
        { values[NR] = $1; total += $1 }
        END {
            if (NR == 0) { print "0,0,0"; exit }
            percentile = int((NR * 95 + 99) / 100)
            if (percentile < 1) percentile = 1
            printf "%d,%.3f,%.3f\n", NR, total / NR, values[percentile]
        }
    ' "$sorted"
}

run_concurrent() {
    local protocol="$1"
    local port="$2"
    local directory="$3"
    local completed=0
    local batch size index pid
    local pids=()

    while ((completed < REQUESTS)); do
        size="$CONCURRENCY"
        ((completed + size > REQUESTS)) && size="$((REQUESTS - completed))"
        pids=()
        for ((batch = 0; batch < size; batch++)); do
            index="$((completed + batch))"
            (request "$protocol" "$port" small.bin '%{time_total},%{http_code}\n' \
                >"$directory/$index" || printf '0,000\n' >"$directory/$index") &
            pids+=("$!")
        done
        for pid in "${pids[@]}"; do
            wait "$pid" || true
        done
        completed="$((completed + size))"
    done
    cat "$directory"/*
}

run_case() {
    local implementation="$1"
    local protocol="$2"
    local port seq_file seq_sorted concurrent_file concurrent_dir
    local start_ns end_ns wall_s throughput speed_bps cpu_start cpu_end cpu_s
    local stop_file rss_file peak_rss latency_summary success avg_ms p95_ms rps
    local concurrent_summary concurrent_success

    port="$(free_port)"
    start_proxy "$implementation" "$protocol" "$port"
    seq_file="$TMP/${implementation}-${protocol}-sequential.csv"
    seq_sorted="$TMP/${implementation}-${protocol}-sequential.sorted"
    concurrent_file="$TMP/${implementation}-${protocol}-concurrent.csv"
    concurrent_dir="$TMP/${implementation}-${protocol}-requests"
    stop_file="$TMP/${implementation}-${protocol}-monitor.stop"
    rss_file="$TMP/${implementation}-${protocol}-rss"
    mkdir "$concurrent_dir"

    monitor_rss "$PROXY_PID" "$stop_file" "$rss_file" &
    MONITOR_PID=$!
    cpu_start="$(cpu_ticks "$PROXY_PID")"

    : >"$seq_file"
    for ((i = 0; i < REQUESTS; i++)); do
        request "$protocol" "$port" small.bin '%{time_total},%{http_code}\n' \
            >>"$seq_file" || printf '0,000\n' >>"$seq_file"
    done
    latency_summary="$(summarize_latencies "$seq_file" "$seq_sorted")"
    IFS=, read -r success avg_ms p95_ms <<<"$latency_summary"

    start_ns="$(date +%s%N)"
    run_concurrent "$protocol" "$port" "$concurrent_dir" >"$concurrent_file"
    end_ns="$(date +%s%N)"
    wall_s="$(awk -v start="$start_ns" -v end="$end_ns" 'BEGIN {printf "%.6f", (end-start)/1000000000}')"
    concurrent_summary="$(summarize_latencies "$concurrent_file" "$TMP/${implementation}-${protocol}-concurrent.sorted")"
    IFS=, read -r concurrent_success _ _ <<<"$concurrent_summary"
    rps="$(awk -v requests="$concurrent_success" -v seconds="$wall_s" 'BEGIN {if (seconds > 0) printf "%.2f", requests/seconds; else print 0}')"

    speed_bps="$(request "$protocol" "$port" payload.bin '%{speed_download}' || printf '0')"
    throughput="$(awk -v speed="$speed_bps" 'BEGIN {printf "%.2f", speed*8/1000000}')"

    cpu_end="$(cpu_ticks "$PROXY_PID")"
    cpu_s="$(awk -v ticks="$((cpu_end - cpu_start))" -v hz="$(getconf CLK_TCK)" 'BEGIN {printf "%.3f", ticks/hz}')"
    touch "$stop_file"
    wait "$MONITOR_PID" || true
    MONITOR_PID=""
    peak_rss="$(<"$rss_file")"

    printf '%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s\n' \
        "$implementation" "$protocol" "$REQUESTS" "$success" "$concurrent_success" "$avg_ms" "$p95_ms" \
        "$wall_s" "$rps" "$throughput" "$cpu_s" "$peak_rss" >>"$OUTPUT"
    printf '%-7s %-6s avg=%8s ms p95=%8s ms concurrent=%8s req/s throughput=%9s Mbit/s CPU=%6s s RSS=%8s KiB\n' \
        "$implementation" "$protocol" "$avg_ms" "$p95_ms" "$rps" "$throughput" "$cpu_s" "$peak_rss"

    terminate "$PROXY_PID"
    PROXY_PID=""
}

printf 'implementation,protocol,requests,sequential_successful,concurrent_successful,seq_avg_ms,seq_p95_ms,concurrent_wall_s,requests_per_s,throughput_mbps,cpu_s,peak_rss_kib\n' >"$OUTPUT"
printf 'Target: local HTTP server; requests=%s concurrency=%s payload=%s MiB\n' \
    "$REQUESTS" "$CONCURRENCY" "$PAYLOAD_MIB"

protocols=(socks5 http)
[[ "$PROTOCOL" != "both" ]] && protocols=("$PROTOCOL")
for protocol in "${protocols[@]}"; do
    run_case rust "$protocol"
    run_case pproxy "$protocol"
done

printf 'CSV written to %s\n' "$OUTPUT"

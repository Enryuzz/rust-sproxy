# Proxy Benchmark Report

## Environment

- Route: client -> tested proxy -> local Python HTTP server
- Implementations: release-built `rust-sproxy` and Python `pproxy` 2.7.9
- Protocols: SOCKS5 and HTTP CONNECT
- Measurements: sequential latency, concurrent request rate, 64 MiB transfer throughput, proxy CPU time, and peak RSS
- Repetitions: 5
- Requests per repetition: 50 sequential and 50 concurrent per implementation/protocol
- Concurrency: 10

All requests were forced through each proxy with curl's `--noproxy ''` option.

## Running The Proxies

Build Rust release binary and create direct-route configuration:

```sh
cargo build --release
printf 'upstreams = []\n' > benchmark/direct.toml
```

Run Rust with SOCKS5 listener:

```sh
target/release/rust-sproxy \
  --type socks5 \
  --listen 127.0.0.1:18081 \
  --config benchmark/direct.toml
```

Run Rust with HTTP CONNECT listener:

```sh
target/release/rust-sproxy \
  --type http \
  --listen 127.0.0.1:18081 \
  --config benchmark/direct.toml
```

Install pproxy in an isolated environment:

```sh
python3 -m venv /tmp/pproxy-venv
/tmp/pproxy-venv/bin/pip install pproxy uvloop
```

Run pproxy with SOCKS5 listener and direct routing:

```sh
/tmp/pproxy-venv/bin/pproxy -l socks5://127.0.0.1:18082
```

Run pproxy with HTTP listener and direct routing:

```sh
/tmp/pproxy-venv/bin/pproxy -l http://127.0.0.1:18082
```

Test SOCKS5 listener:

```sh
curl --noproxy '' --proxy socks5h://127.0.0.1:18081 https://example.com/
```

Test HTTP CONNECT listener:

```sh
curl --noproxy '' --proxy http://127.0.0.1:18081 --proxytunnel https://example.com/
```

Change port from `18081` to `18082` when testing pproxy.

Run default comparison benchmark:

```sh
scripts/compare-proxies.sh \
  --pproxy /tmp/pproxy-venv/bin/pproxy \
  --output benchmark/comparison.csv
```

Run five heavy comparison rounds:

```sh
for run in 1 2 3 4 5; do
  scripts/compare-proxies.sh \
    --pproxy /tmp/pproxy-venv/bin/pproxy \
    --requests 500 \
    --concurrency 50 \
    --payload-mib 512 \
    --output "benchmark/manual-run-${run}.csv"
done
```

## Initial Results

| Implementation | Protocol | Successful requests | Avg latency | Median latency | Avg p95 | Median request rate | Median throughput | Avg CPU | Avg peak RSS |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Rust | SOCKS5 | 500/500 | 1.921 ms | 1.936 ms | 2.274 ms | 586.53 req/s | 10,298.89 Mbit/s | 0.106 s | 4,666 KiB |
| pproxy | SOCKS5 | 500/500 | 2.191 ms | 2.213 ms | 2.887 ms | 486.63 req/s | 5,348.12 Mbit/s | 0.224 s | 23,434 KiB |
| Rust | HTTP CONNECT | 500/500 | 1.841 ms | 1.848 ms | 2.244 ms | 600.81 req/s | 10,908.02 Mbit/s | 0.114 s | 4,702 KiB |
| pproxy | HTTP CONNECT | 500/500 | 2.060 ms | 2.023 ms | 2.644 ms | 509.55 req/s | 5,850.18 Mbit/s | 0.208 s | 23,351 KiB |

Success totals combine 250 sequential and 250 concurrent requests. One pproxy SOCKS5 concurrency run produced a 92.35 req/s outlier; median request rate limits its effect.

## Initial Summary

- Rust used about one fifth of pproxy's memory.
- Rust had 11-12% lower average latency.
- Rust had 15-21% lower average p95 latency.
- Rust handled 18-21% more requests per second by median.
- Rust delivered about twice pproxy's median local throughput.

These localhost measurements compare implementation overhead. They do not represent internet throughput, and the local Python target can become the transfer bottleneck.

## Heavy Test Configuration

- Repetitions: 5
- Requests per repetition: 500 sequential and 500 concurrent per implementation/protocol
- Concurrency: 50
- Transfer payload: 512 MiB per implementation/protocol/repetition
- Total measured requests: 20,000
- Total large-transfer data requested: 10 GiB

## Heavy Test Results

> Historical result: target server used Python's default accept backlog of 5 while concurrency was 50. Request-rate results in this section are backlog-constrained and must not be used to compare proxy concurrency. Corrected results appear below.

| Implementation | Protocol | Successful requests | Avg latency | Avg p95 | Median request rate | Median throughput | Avg CPU | Avg peak RSS |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| Rust | SOCKS5 | 5,000/5,000 | 1.841 ms | 2.261 ms | 25.57 req/s | 12,631.44 Mbit/s | 0.924 s | 5,946 KiB |
| pproxy | SOCKS5 | 5,000/5,000 | 2.158 ms | 2.828 ms | 41.33 req/s | 5,889.56 Mbit/s | 1.876 s | 24,106 KiB |
| Rust | HTTP CONNECT | 5,000/5,000 | 1.806 ms | 2.180 ms | 26.21 req/s | 11,149.76 Mbit/s | 1.000 s | 6,094 KiB |
| pproxy | HTTP CONNECT | 5,000/5,000 | 2.074 ms | 2.740 ms | 44.28 req/s | 5,941.43 Mbit/s | 1.844 s | 24,167 KiB |

Success totals combine 2,500 sequential and 2,500 concurrent requests for each implementation/protocol.

## Heavy Test Comparison

SOCKS5 Rust results relative to pproxy:

- 14.7% lower average sequential latency
- 20.1% lower average p95 latency
- 38.1% lower median request rate at concurrency 50
- 114.5% higher median transfer throughput
- 50.7% lower CPU time
- 75.3% lower peak RSS

HTTP CONNECT Rust results relative to pproxy:

- 12.9% lower average sequential latency
- 20.4% lower average p95 latency
- 40.8% lower median request rate at concurrency 50
- 87.7% higher median transfer throughput
- 45.8% lower CPU time
- 74.8% lower peak RSS

## Interpretation

Rust retained lower sequential latency, roughly double transfer throughput, about half the CPU time, and about one quarter the memory under the heavier workload. However, pproxy completed the concurrency-50 request batches faster.

One likely contributor is dynamic configuration reload: `rust-sproxy` reads and parses its TOML file for every new connection, including all concurrent requests, while pproxy uses already-loaded routing configuration. The long wall time combined with low proxy CPU indicates waiting or scheduling contention rather than CPU saturation. Isolating that cost requires a separate benchmark mode that caches Rust configuration; current results intentionally measure shipped behavior.

Local Python HTTP server capacity, process scheduling, filesystem cache state, and loopback behavior also affect results. Use these numbers for relative local comparison, not expected internet bandwidth.

## Cached Configuration And Corrected Target

`rust-sproxy` now loads and parses TOML once, caching all routes for process lifetime. Configuration changes require restart. Benchmark target accept backlog was raised from Python's default 5 to 4096 so concurrency 50 measures proxy behavior instead of target SYN/backlog contention.

Configuration remained five repetitions, 500 sequential requests, 500 concurrent requests at concurrency 50, and a 512 MiB transfer per implementation/protocol/repetition.

| Implementation | Protocol | Successful requests | Avg latency | Avg p95 | Median request rate | Median throughput | Avg CPU | Avg peak RSS |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| Rust | SOCKS5 | 5,000/5,000 | 1.664 ms | 2.061 ms | 943.27 req/s | 11,736.97 Mbit/s | 0.824 s | 6,072 KiB |
| pproxy | SOCKS5 | 5,000/5,000 | 2.078 ms | 2.759 ms | 833.15 req/s | 6,409.55 Mbit/s | 1.750 s | 24,398 KiB |
| Rust | HTTP CONNECT | 5,000/5,000 | 1.610 ms | 2.001 ms | 902.18 req/s | 12,844.30 Mbit/s | 0.876 s | 6,186 KiB |
| pproxy | HTTP CONNECT | 5,000/5,000 | 1.945 ms | 2.685 ms | 829.46 req/s | 6,060.83 Mbit/s | 1.728 s | 24,322 KiB |

Corrected Rust results relative to pproxy:

- SOCKS5: 19.9% lower average latency, 25.3% lower p95, 13.2% higher median request rate, and 83.1% higher median throughput
- HTTP CONNECT: 17.2% lower average latency, 25.5% lower p95, 8.8% higher median request rate, and 111.9% higher median throughput
- Both protocols: about half the CPU time and one quarter the memory
- All 20,000 measured requests succeeded

Configuration caching and target-backlog correction were applied together, so this run does not isolate their individual contributions. A cached run against the old backlog remained slow, showing the target backlog was the primary cause of the earlier request-rate inversion.

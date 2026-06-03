# ngx-tickle benchmarks


[`ngx-tickle`](https://crates.io/crates/ngx-tickle) replaces ngx-rust’s
async task scheduler with one that **always enqueues** wakes to be run
on the nginx event loop thread instead of running them inline. This
allows wakers to be called safely from secondary threads, and enables
using tokio runtimes alongside nginx.

This report measures whether that costs throughput or latency, how the
[`batch_size`](https://docs.rs/ngx-tickle/0.2.5/ngx_tickle/fn.set_batch_size.html)
knob behaves, and how native nginx I/O compares to tokio I/O driven
through the same scheduler.

To that end, nginx-acme’s
[`PeerConnection`](https://github.com/nginx/nginx-acme/blob/main/src/net/peer_conn.rs)
implementation was imported, which brings in an nginx-native
implementation of the hyper traits necessary for a client.

## Summary

Across a pure-async workload (resolve) and a mixed nginx-I/O-plus-async
one (hyper), `tickle`’s scheduler matches ngx-rust’s within a few
percent at a sensible `batch_size`, adds no measurable heap, and keeps
latencies tight. The one real decision is the I/O back-end — native
nginx I/O is modestly faster than tokio I/O — and since both can be used
with ngx-tickle, it’s a per-use-case choice, not an upfront commitment.

## Experiments

### Resolve

Using the existing
[`Resolver`](https://docs.rs/ngx/0.5.1/ngx/async_/resolver/struct.Resolver.html)
Future, a name of the form `b{i:x}.fake.internal.` is resolved in an
unbound server running on localhost. Unbound is configured to locally
resolve any such query to the same response:

<div class="code-with-filename">

**unbound.conf**

```
  local-data: 'fake.internal. 1 IN A 127.0.0.1'
  local-zone: 'fake.internal.' redirect
```

</div>

The location is configured as follows:

<div class="code-with-filename">

**benchmark.conf**

```
location /benchmark/ {
    resolver 127.0.0.1:53 valid=1s ipv6=off;
    benchmark on;
}
```

</div>

`i` in the name template rotates back to 0 at 100_000.

This circumvents the nginx resolver cache and same-query coalescing for
request rates up to 100k rps. Every HTTP request corresponds to one DNS
query.

The handler then sets a response header with the elapsed time since task
start and finalizes with HTTP 204. See
[`resolve()`](../../examples/benchmark.rs) for details.

This async function is run with either
[`ngx::async_::spawn()`](https://docs.rs/ngx/0.5.1/ngx/async_/fn.spawn.html)
(`resolve/ngx`) or
[`ngx_tickle::spawn()`](https://docs.rs/ngx-tickle/0.2.5/ngx_tickle/fn.spawn.html)
(`resolve/tickle`), and directly compared. No extra thread is involved
in either case; everything stays on the nginx event loop thread.

The `resolve/tickle` test is repeated with the batch sizes 1, 8 (library
default), and 1024 (representing ”unbounded” — the maximum batch size
observed empirically is around 60). The ”unbounded” batch size is
closest to the behavior of
[`ngx::async_::spawn()`](https://docs.rs/ngx/0.5.1/ngx/async_/fn.spawn.html),
which runs most wakeups immediately inline, and does not try to pace
async wakeups vs. nginx event loop turns in any way.

### Hyper

A hyper client is constructed with either
[`PeerConnection`](https://github.com/nginx/nginx-acme/blob/main/src/net/peer_conn.rs)
or
[`TokioIo`](https://docs.rs/hyper-util/0.1.20/hyper_util/rt/tokio/struct.TokioIo.html)
IO implementations. The PeerConnection IO implementation uses either
[`ngx::async_::spawn()`](https://docs.rs/ngx/0.5.1/ngx/async_/fn.spawn.html)
(`hyper/ngx`) or
[`ngx_tickle::spawn()`](https://docs.rs/ngx-tickle/0.2.5/ngx_tickle/fn.spawn.html)
(`hyper/tickle`) for *both* the async handler task and hyper driver
task. These tests don’t involve any extra threads in either case, and
should represent a fair comparison between the ngx and tickle
schedulers.

The TokioIo implementation uses
[`ngx_tickle::spawn()`](https://docs.rs/ngx-tickle/0.2.5/ngx_tickle/fn.spawn.html)
for the async handler task, and the
[`async-compat`](https://crates.io/crates/async-compat) embedded runtime
for the driver task (`hyper/tokio`). Note that this combination is
impossible with
[`ngx::async_::spawn()`](https://docs.rs/ngx/0.5.1/ngx/async_/fn.spawn.html)
because it involves waking of the handler task from the compat runtime
thread. Comparing these results with `hyper/tickle` should allow
estimating the impact of a native nginx IO implementation versus a
generic tokio-provided one, a secondary goal of this benchmark.

The hyper client loads a [static JSON
file](../../examples/prefix/html/example.json) of non-trivial size that
still fits in a single page (3.6 KiB), from *the same nginx server*. The
async handler then decodes it using `serde_json` and records the total
time in a response header. See
[`hyper_client()`](../../examples/benchmark.rs) for details.

That design is deliberate: Loading from the same nginx server, and
having the async task do some work after wakeup by actually parsing the
file creates competition between nginx IO and async tasks. This creates
a more realistic scenario than the “pure-async“ workload of the resolve
tests, and can surface fairness issues between native nginx events and
async tasks.

- `hyper/tickle` vs `hyper/ngx` isolates the scheduler question.
- `hyper/tickle` vs `hyper/tokio` isolates the I/O question.

## Testing process

The workload groups **resolve** and **hyper** are measured
independently. Each goes through three passes, all driven with
`wrk`/`wrk2` at `-t3 -c100` (3 threads, 100 connections) for 120 s per
run:

1.  **Saturate** — `wrk` runs flat-out to establish each variant’s peak
    throughput.
2.  **Pace** — the group’s lowest saturation rps × 0.7 sets a fixed
    request rate for a `wrk2` run. `wrk2`’s open-loop pacing avoids
    *coordinated omission*: a closed-loop generator like `wrk` pauses
    sending while it waits out a stall, so it never records the latency
    of the requests it didn’t send — hiding the tail.
3.  **Heap** — the saturating `wrk` pass is repeated with nginx started
    under `heaptrack`; peak consumption is read back from
    `heaptrack_print`.

Each pass is repeated 3 times, against a fresh nginx instance per
repetition. The paced `wrk2` graphs use the per-percentile median across
the three reps.

## Experiment set-up

- **Host:** DigitalOcean `c-8` — 8 dedicated vCPU, Intel Xeon Platinum
  8168 @ 2.70 GHz, NixOS
- **Kernel:** 6.18.33
- **ngx-tickle:** 0.2.5
- **nginx**: configured as single-process, no workers. See
  [benchmark.conf](../../examples/prefix/conf/benchmark.conf) for
  details.

## Results

### 1. Scheduler overhead — how does tickle throughput compare to ngx-rust?

**It’s competitive.** On both workloads `tickle` matches `ngx`’s spawn
within a few percent at an appropriate `batch_size`:

- **resolve**: `tickle/8` (the default, ~29.7k) ties `ngx` (~30.0k)
  within ~1%; the batch extremes spread either side — `tickle/1024`
  ~33.5k, `tickle/1` ~23.8k (see the caveat below).
- **hyper**: `ngx` (~3.99k) ≈ `tickle/1` (~4.13k); the other batch sizes
  sit within ~3%.

> [!NOTE]
>
> The resolve test’s throughput numbers should be taken with a grain of
> salt — the fact that the handler is not doing any work after the
> completed resolution, and that there is no competing load on nginx
> otherwise makes it an ideal case for aggressive batching.
>
> The hyper test is more realistic and shows no such effect — if
> anything the smaller batch sizes do slightly better there, which §2
> picks up.

| workload | variant     | sat rps |
|----------|-------------|---------|
| hyper    | ngx         | 3994    |
| hyper    | tickle/1    | 4132    |
| hyper    | tickle/8    | 3927    |
| hyper    | tickle/1024 | 3925    |
| hyper    | tokio/1     | 3838    |
| hyper    | tokio/8     | 3654    |
| hyper    | tokio/1024  | 3644    |
| resolve  | ngx         | 30003   |
| resolve  | tickle/1    | 23796   |
| resolve  | tickle/8    | 29722   |
| resolve  | tickle/1024 | 33538   |

<img src="benchmark_report_files/figure-commonmark/cell-6-output-1.png"
width="523" height="387" />

<img src="benchmark_report_files/figure-commonmark/cell-6-output-2.png"
width="518" height="387" />

### 2. The `batch_size` knob — fairness vs throughput, and it points *both ways*

`batch_size` caps how many runnables a wake drains before returning to
the event loop. Low = finer interleaving with nginx’s own work; high =
less scheduling overhead, and fewer wakeup syscalls — pending wakes
coalesce into a single `eventfd` write, though the re-schedule when a
wake hits the batch limit can’t be coalesced. Which one wins is
**workload-dependent, and the two workloads disagree**:

- **resolve (exclusive)**: throughput rises monotonically with batch
  (~23.8k → ~29.7k → ~33.5k). And the paced latencies below show
  `tickle/1` paying for the coarse cycling: a median p99 of ~16 ms —
  with a wide rep spread (8–38 ms across the three reps, one rep’s p99.9
  near 90 ms) — against ~2.7 ms for every other variant. At this
  open-loop rate, draining a single runnable per wake makes the tail
  latency erratic. Its closed-loop “saturation” number hides this,
  because coordinated omission masks the tail. Here, bigger batch is
  strictly better on *both* axes.
- **hyper (self-loopback, mixed)**: throughput *falls* then flattens
  with batch (~4.13k → ~3.93k → ~3.93k) — finer interleaving lets
  nginx’s own upstream-serving work proceed between async drains. Here,
  smaller batch wins (paced tails stay tight across all three).

The *size* of that `tickle/1` tail is genuinely surprising — far bigger
than the throughput gap alone would predict, and noisy across reps. The
leading hypothesis is the “anemic” task: with almost no work per
request, `resolve` runs at a very high request — and therefore wake —
rate, and at `batch_size=1` each wake is its own event-loop round-trip,
leaving little slack. A transient stall then can’t be absorbed by
draining a larger batch; paced arrivals pile up, and `wrk2`’s
coordinated-omission correction records the whole backlog. A heavier
per-request task (as in `hyper`) lowers the rate, which is likely why
`hyper/1` shows none of this. This is a hypothesis, not a confirmed
mechanism.

The takeaway isn’t “pick N”; it’s that the optimal point is
workload-specific and can invert. The default `batch_size=8` lands in
the clean middle on both workloads.

| workload | variant     | 50.0  | 90.0  | 99.0   | 99.9   |
|----------|-------------|-------|-------|--------|--------|
| hyper    | ngx         | 1.222 | 1.923 | 2.555  | 3.051  |
| hyper    | tickle/1    | 1.307 | 2.035 | 2.729  | 3.365  |
| hyper    | tickle/1024 | 1.247 | 1.94  | 2.547  | 3.071  |
| hyper    | tickle/8    | 1.257 | 1.939 | 2.547  | 3.029  |
| hyper    | tokio/1     | 1.423 | 2.307 | 3.377  | 6.307  |
| hyper    | tokio/1024  | 1.359 | 2.147 | 2.953  | 3.665  |
| hyper    | tokio/8     | 1.363 | 2.151 | 3.015  | 3.835  |
| resolve  | ngx         | 1.234 | 2.051 | 2.833  | 5.563  |
| resolve  | tickle/1    | 1.453 | 2.535 | 16.231 | 23.903 |
| resolve  | tickle/1024 | 1.235 | 2.046 | 2.737  | 3.591  |
| resolve  | tickle/8    | 1.237 | 2.045 | 2.757  | 3.937  |

<img src="benchmark_report_files/figure-commonmark/cell-8-output-1.png"
width="651" height="394" />

<img src="benchmark_report_files/figure-commonmark/cell-8-output-2.png"
width="646" height="394" />

### 3. I/O back-end — native nginx vs tokio I/O

Holding the scheduler fixed (`tickle`) and varying only I/O, **native
nginx I/O is ~7–8% faster** in saturation (e.g. bs=1: 4.13k vs 3.84k;
bs=8: 3.93k vs 3.65k) and has a lower paced tail (~2.7 ms p99 vs ~3.4 ms
at bs=1). Surprisingly, native also uses *more* heap (~6 MB vs ~4.5 MB —
see *Peak heap* below).

*Read*: native I/O wins modestly on this workload. When maximum
performance is required, writing nginx-specific futures instead of using
generic tokio ones is worth the effort. Otherwise, or when there are no
nginx-native alternatives readily available, using generic
implementations from tokio (or libraries using them, like
[`reqwest`](https://crates.io/crates/reqwest)) might be enough.

### 4. Peak heap

Peak heap is **scheduler-independent**:

- **hyper**: `ngx` 6.12 MB ≈ `tickle` 6.05–6.10 MB, *flat* across
  batch_size — the scheduler adds no measurable footprint. `tokio` I/O
  uses less heap (~4.5 MB) than native (~6 MB) (which is surprising, but
  was not further investigated this time)
- **resolve**: every variant — `ngx`, `sync`, and all three `tickle`
  batch sizes — sits at **~22.1 MB**. Over the 120 s run each cycles the
  full 100k-name ring, so the nginx resolver cache fills to its
  cardinality cap (~100k nodes at ~220 B each). That ceiling is set by
  the test’s name ring, not by the scheduler or the async layer — a real
  workload with few distinct names wouldn’t pay it — and it lands in the
  same place regardless of which spawn or batch size drives the lookups.

*Read*: Neither scheduler incurs a measurable heap cost — expected,
since the scheduler only ever holds `Runnable`s queued in a channel, and
a `Runnable` (like its `Task<T>` handle) is just a single pointer into
the task’s one heap allocation. That allocation exists regardless of
which scheduler drives it.

| workload | variant     | peak_heap | peak_mb |
|----------|-------------|-----------|---------|
| hyper    | ngx         | 6.12M     | 6.12    |
| hyper    | tickle/1    | 6.08M     | 6.08    |
| hyper    | tickle/8    | 6.05M     | 6.05    |
| hyper    | tickle/1024 | 6.10M     | 6.1     |
| hyper    | tokio/1     | 4.56M     | 4.56    |
| hyper    | tokio/8     | 4.52M     | 4.52    |
| hyper    | tokio/1024  | 4.55M     | 4.55    |
| resolve  | ngx         | 22.14M    | 22.14   |
| resolve  | tickle/1    | 22.14M    | 22.14   |
| resolve  | tickle/8    | 22.14M    | 22.14   |
| resolve  | tickle/1024 | 22.14M    | 22.14   |

<img src="benchmark_report_files/figure-commonmark/cell-10-output-1.png"
width="763" height="409" />

## Appendix — raw data

**wrk**

| name | mode | batch_size | rep | r | path | rps | min | max | mean | stdev | 50% | 90% | 99% | 99.9% |
|----|----|----|----|----|----|----|----|----|----|----|----|----|----|----|
| hyper_ngx | wrk | null | 1 | null | /benchmark/hyper/ngx | 4008 | 8135 | 45712 | 24687 | 1955 | 24180 | 27398 | 30191 | 35525 |
| hyper_ngx | wrk | null | 2 | null | /benchmark/hyper/ngx | 3994 | 8106 | 45753 | 24772 | 1852 | 24266 | 27452 | 29821 | 32797 |
| hyper_ngx | wrk | null | 3 | null | /benchmark/hyper/ngx | 3990 | 7404 | 37666 | 24804 | 1828 | 24425 | 27446 | 29814 | 32308 |
| hyper_ngx | wrk2 | null | 1 | 2534 | /benchmark/hyper/ngx | 2532 | 243 | 5788 | 1258 | 497 | 1218 | 1929 | 2555 | 3051 |
| hyper_ngx | wrk2 | null | 2 | 2534 | /benchmark/hyper/ngx | 2532 | 245 | 5424 | 1265 | 497 | 1232 | 1923 | 2559 | 3115 |
| hyper_ngx | wrk2 | null | 3 | 2534 | /benchmark/hyper/ngx | 2532 | 247 | 4116 | 1256 | 490 | 1222 | 1912 | 2539 | 3035 |
| hyper_tickle | wrk | 1 | 1 | null | /benchmark/hyper/tickle | 4167 | 17742 | 34323 | 23744 | 1663 | 23400 | 26214 | 28189 | 29954 |
| hyper_tickle | wrk | 1 | 2 | null | /benchmark/hyper/tickle | 4132 | 17281 | 40714 | 23947 | 1774 | 23592 | 26398 | 29105 | 31547 |
| hyper_tickle | wrk | 1 | 3 | null | /benchmark/hyper/tickle | 4100 | 16654 | 39366 | 24136 | 1743 | 23909 | 26500 | 28823 | 32657 |
| hyper_tickle | wrk | 1024 | 1 | null | /benchmark/hyper/tickle | 3925 | 6899 | 40010 | 25211 | 1839 | 24842 | 27894 | 30152 | 31982 |
| hyper_tickle | wrk | 1024 | 2 | null | /benchmark/hyper/tickle | 3934 | 9093 | 45630 | 25154 | 1903 | 24711 | 27971 | 30368 | 32728 |
| hyper_tickle | wrk | 1024 | 3 | null | /benchmark/hyper/tickle | 3913 | 9298 | 44464 | 25289 | 1904 | 24854 | 28064 | 30348 | 32036 |
| hyper_tickle | wrk | 8 | 1 | null | /benchmark/hyper/tickle | 3953 | 10140 | 37027 | 25033 | 1787 | 24588 | 27672 | 30053 | 30857 |
| hyper_tickle | wrk | 8 | 2 | null | /benchmark/hyper/tickle | 3885 | 10376 | 40697 | 25470 | 1921 | 25092 | 28200 | 30519 | 34681 |
| hyper_tickle | wrk | 8 | 3 | null | /benchmark/hyper/tickle | 3927 | 8954 | 53569 | 25199 | 1891 | 24775 | 27828 | 30320 | 37422 |
| hyper_tickle | wrk2 | 1 | 1 | 2534 | /benchmark/hyper/tickle | 2532 | 268 | 5316 | 1330 | 508 | 1287 | 2010 | 2645 | 3193 |
| hyper_tickle | wrk2 | 1 | 2 | 2534 | /benchmark/hyper/tickle | 2532 | 267 | 5684 | 1373 | 533 | 1329 | 2089 | 2775 | 3365 |
| hyper_tickle | wrk2 | 1 | 3 | 2534 | /benchmark/hyper/tickle | 2532 | 270 | 9000 | 1350 | 526 | 1307 | 2035 | 2729 | 3499 |
| hyper_tickle | wrk2 | 1024 | 1 | 2534 | /benchmark/hyper/tickle | 2532 | 259 | 4504 | 1290 | 485 | 1257 | 1938 | 2541 | 3037 |
| hyper_tickle | wrk2 | 1024 | 2 | 2534 | /benchmark/hyper/tickle | 2532 | 255 | 4704 | 1282 | 489 | 1247 | 1940 | 2547 | 3071 |
| hyper_tickle | wrk2 | 1024 | 3 | 2534 | /benchmark/hyper/tickle | 2532 | 257 | 5988 | 1283 | 493 | 1247 | 1943 | 2549 | 3099 |
| hyper_tickle | wrk2 | 8 | 1 | 2534 | /benchmark/hyper/tickle | 2532 | 258 | 9784 | 1291 | 493 | 1257 | 1939 | 2553 | 3093 |
| hyper_tickle | wrk2 | 8 | 2 | 2534 | /benchmark/hyper/tickle | 2532 | 257 | 4340 | 1290 | 488 | 1259 | 1943 | 2547 | 3017 |
| hyper_tickle | wrk2 | 8 | 3 | 2534 | /benchmark/hyper/tickle | 2532 | 261 | 4484 | 1288 | 486 | 1255 | 1939 | 2531 | 3029 |
| hyper_tokio | wrk | 1 | 1 | null | /benchmark/hyper/tokio | 3838 | 15447 | 41235 | 25786 | 2677 | 25060 | 28838 | 35854 | 37238 |
| hyper_tokio | wrk | 1 | 2 | null | /benchmark/hyper/tokio | 3829 | 13851 | 41028 | 25841 | 2757 | 25195 | 28827 | 36310 | 38354 |
| hyper_tokio | wrk | 1 | 3 | null | /benchmark/hyper/tokio | 3875 | 13200 | 39435 | 25540 | 2576 | 24735 | 28478 | 35553 | 37110 |
| hyper_tokio | wrk | 1024 | 1 | null | /benchmark/hyper/tokio | 3661 | 8623 | 47883 | 27026 | 2943 | 26073 | 30518 | 37618 | 38849 |
| hyper_tokio | wrk | 1024 | 2 | null | /benchmark/hyper/tokio | 3636 | 9184 | 63902 | 27214 | 2880 | 26225 | 30512 | 38093 | 40178 |
| hyper_tokio | wrk | 1024 | 3 | null | /benchmark/hyper/tokio | 3644 | 8494 | 61471 | 27154 | 3103 | 26226 | 30749 | 38333 | 39818 |
| hyper_tokio | wrk | 8 | 1 | null | /benchmark/hyper/tokio | 3620 | 10150 | 47710 | 27336 | 3017 | 26349 | 30786 | 38221 | 41180 |
| hyper_tokio | wrk | 8 | 2 | null | /benchmark/hyper/tokio | 3654 | 10938 | 48886 | 27084 | 2958 | 26106 | 30476 | 37691 | 39930 |
| hyper_tokio | wrk | 8 | 3 | null | /benchmark/hyper/tokio | 3661 | 8187 | 51777 | 27028 | 2867 | 26098 | 30402 | 37607 | 39660 |
| hyper_tokio | wrk2 | 1 | 1 | 2534 | /benchmark/hyper/tokio | 2532 | 270 | 14856 | 1514 | 718 | 1423 | 2319 | 3669 | 6307 |
| hyper_tokio | wrk2 | 1 | 2 | 2534 | /benchmark/hyper/tokio | 2532 | 289 | 7444 | 1488 | 614 | 1423 | 2273 | 3315 | 4375 |
| hyper_tokio | wrk2 | 1 | 3 | 2534 | /benchmark/hyper/tokio | 2532 | 282 | 14256 | 1516 | 660 | 1446 | 2307 | 3377 | 6459 |
| hyper_tokio | wrk2 | 1024 | 1 | 2534 | /benchmark/hyper/tokio | 2532 | 278 | 5788 | 1396 | 540 | 1345 | 2117 | 2853 | 3599 |
| hyper_tokio | wrk2 | 1024 | 2 | 2534 | /benchmark/hyper/tokio | 2532 | 279 | 5848 | 1412 | 557 | 1359 | 2147 | 2953 | 3665 |
| hyper_tokio | wrk2 | 1024 | 3 | 2534 | /benchmark/hyper/tokio | 2532 | 273 | 5628 | 1422 | 557 | 1375 | 2151 | 2961 | 3771 |
| hyper_tokio | wrk2 | 8 | 1 | 2534 | /benchmark/hyper/tokio | 2532 | 281 | 5580 | 1434 | 563 | 1381 | 2171 | 3015 | 3835 |
| hyper_tokio | wrk2 | 8 | 2 | 2534 | /benchmark/hyper/tokio | 2532 | 267 | 6432 | 1412 | 566 | 1359 | 2145 | 3025 | 3965 |
| hyper_tokio | wrk2 | 8 | 3 | 2534 | /benchmark/hyper/tokio | 2532 | 277 | 6444 | 1414 | 555 | 1363 | 2151 | 2939 | 3641 |
| resolve_ngx | wrk | null | 1 | null | /benchmark/resolve/ngx | 30003 | 336 | 63675 | 3296 | 747 | 3114 | 3944 | 5373 | 7461 |
| resolve_ngx | wrk | null | 2 | null | /benchmark/resolve/ngx | 30073 | 209 | 66359 | 3286 | 719 | 3077 | 3938 | 5298 | 6935 |
| resolve_ngx | wrk | null | 3 | null | /benchmark/resolve/ngx | 29783 | 274 | 91942 | 3322 | 838 | 3108 | 4013 | 5187 | 6980 |
| resolve_ngx | wrk2 | null | 1 | 16605 | /benchmark/resolve/ngx | 16593 | 81 | 22000 | 1308 | 620 | 1245 | 2079 | 2917 | 5563 |
| resolve_ngx | wrk2 | null | 2 | 16605 | /benchmark/resolve/ngx | 16593 | 73 | 9896 | 1281 | 625 | 1220 | 2032 | 2833 | 7207 |
| resolve_ngx | wrk2 | null | 3 | 16605 | /benchmark/resolve/ngx | 16593 | 89 | 8124 | 1285 | 573 | 1234 | 2051 | 2815 | 3599 |
| resolve_tickle | wrk | 1 | 1 | null | /benchmark/resolve/tickle | 23796 | 1207 | 10725 | 4154 | 620 | 3849 | 5127 | 6067 | 7255 |
| resolve_tickle | wrk | 1 | 2 | null | /benchmark/resolve/tickle | 24012 | 1179 | 9735 | 4116 | 575 | 3841 | 5017 | 5830 | 6635 |
| resolve_tickle | wrk | 1 | 3 | null | /benchmark/resolve/tickle | 23722 | 1245 | 9584 | 4167 | 585 | 3880 | 5045 | 5977 | 6784 |
| resolve_tickle | wrk | 1024 | 1 | null | /benchmark/resolve/tickle | 33291 | 615 | 52333 | 2964 | 526 | 2897 | 3447 | 4425 | 5842 |
| resolve_tickle | wrk | 1024 | 2 | null | /benchmark/resolve/tickle | 33756 | 638 | 27053 | 2921 | 485 | 2771 | 3441 | 4442 | 5973 |
| resolve_tickle | wrk | 1024 | 3 | null | /benchmark/resolve/tickle | 33538 | 301 | 30716 | 2940 | 479 | 2781 | 3457 | 4429 | 5885 |
| resolve_tickle | wrk | 8 | 1 | null | /benchmark/resolve/tickle | 29722 | 460 | 9133 | 3324 | 591 | 3046 | 4083 | 5080 | 6227 |
| resolve_tickle | wrk | 8 | 2 | null | /benchmark/resolve/tickle | 30017 | 762 | 10280 | 3291 | 594 | 2978 | 4086 | 4912 | 5922 |
| resolve_tickle | wrk | 8 | 3 | null | /benchmark/resolve/tickle | 29705 | 465 | 9762 | 3325 | 610 | 3018 | 4113 | 5222 | 6122 |
| resolve_tickle | wrk2 | 1 | 1 | 16605 | /benchmark/resolve/tickle | 16593 | 98 | 93888 | 2568 | 7112 | 1453 | 2535 | 37535 | 86975 |
| resolve_tickle | wrk2 | 1 | 2 | 16605 | /benchmark/resolve/tickle | 16593 | 93 | 29504 | 1961 | 2516 | 1473 | 2553 | 16231 | 23903 |
| resolve_tickle | wrk2 | 1 | 3 | 16605 | /benchmark/resolve/tickle | 16593 | 94 | 22464 | 1578 | 1297 | 1399 | 2311 | 7847 | 17103 |
| resolve_tickle | wrk2 | 1024 | 1 | 16605 | /benchmark/resolve/tickle | 16593 | 98 | 7008 | 1287 | 566 | 1235 | 2047 | 2739 | 3591 |
| resolve_tickle | wrk2 | 1024 | 2 | 16605 | /benchmark/resolve/tickle | 16593 | 99 | 10432 | 1289 | 570 | 1237 | 2046 | 2737 | 3501 |
| resolve_tickle | wrk2 | 1024 | 3 | 16605 | /benchmark/resolve/tickle | 16593 | 97 | 11672 | 1278 | 574 | 1226 | 2026 | 2731 | 3835 |
| resolve_tickle | wrk2 | 8 | 1 | 16605 | /benchmark/resolve/tickle | 16594 | 92 | 10640 | 1297 | 574 | 1247 | 2045 | 2757 | 3937 |
| resolve_tickle | wrk2 | 8 | 2 | 16605 | /benchmark/resolve/tickle | 16593 | 93 | 7516 | 1287 | 570 | 1236 | 2042 | 2731 | 3777 |
| resolve_tickle | wrk2 | 8 | 3 | 16605 | /benchmark/resolve/tickle | 16593 | 95 | 11800 | 1295 | 597 | 1237 | 2049 | 2791 | 5275 |

**heaptrack**

| name           | batch_size | peak_heap |
|----------------|------------|-----------|
| hyper_ngx      | null       | 6.12M     |
| hyper_tickle   | 1          | 6.08M     |
| hyper_tickle   | 1024       | 6.10M     |
| hyper_tickle   | 8          | 6.05M     |
| hyper_tokio    | 1          | 4.56M     |
| hyper_tokio    | 1024       | 4.55M     |
| hyper_tokio    | 8          | 4.52M     |
| resolve_ngx    | null       | 22.14M    |
| resolve_tickle | 1          | 22.14M    |
| resolve_tickle | 1024       | 22.14M    |
| resolve_tickle | 8          | 22.14M    |

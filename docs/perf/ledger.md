# Optimization ledger

Every number below comes from a run made on the machine described under **Environment**, with the command printed
above the table that holds it. Nothing here is estimated, and no measurement was repeated until it gave a wanted
answer. Results that contradict the plan are marked **Contradicts the plan** and carry the size of the gap.

## Environment

| Item | Value |
| --- | --- |
| Machine | Apple M3 Max, arm64 |
| OS | macOS 27.2 (build 26B5086k), Darwin 27.2.0 |
| Toolchain | `rustc 1.98.1 (48a229cea 2026-09-01)`, `cargo 1.98.1 (797e8a9bc 2026-08-05)` |
| RUSTFLAGS | **cleared** on every invocation (`env -u RUSTFLAGS`); the login shell exports nightly-only `-Z` options and `-C target-cpu=apple-m3`, neither of which may influence a measurement |
| cargo config | `--config ~/.config/rust/config.dev.toml`, which only redirects `build.target-dir`; no profile overrides |
| Date | 2026-09-19 |
| Crates measured | sonic-rs 0.5.10, serde_json 1.0.151 (`float_roundtrip`), serde_path_to_error 0.1.20, dhat 0.3.3, bytes 1.12.1, json-escape-simd 3.1.2, divan 0.1.21, hyper 1.11.1, hyper-util 0.1.20, hyper-rustls 0.27.9, rustls 0.23.45, rustls-platform-verifier 0.7.0, tokio-rustls 0.26.5, h2 0.4.19 |

Measurement rules followed: no benchmark ran in parallel with another benchmark or with a build; no `RUSTFLAGS` and no
`target-cpu`; allocation numbers are taken on the **second identical call**, after one warm-up call of the same shape;
every dhat scenario is its own process, because dhat attributes the whole process to one profiler.

Reproduce everything from the repository root:

```sh
cd spikes/sonic-probe     && env -u RUSTFLAGS cargo --config ~/.config/rust/config.dev.toml build --release
cd spikes/encode-buffer   && env -u RUSTFLAGS cargo --config ~/.config/rust/config.dev.toml build --release
cd spikes/transport-probe && env -u RUSTFLAGS cargo --config ~/.config/rust/config.dev.toml build --release
cd spikes/alloc-inventory  && env -u RUSTFLAGS cargo --config ~/.config/rust/config.dev.toml build --release
```

The binaries land in the shared target directory that `config.dev.toml` names; the commands below write it as
`$TARGET/release/<name>`.

---

## Phase 0 foundation findings (from `.omc/handoffs/phase-0-notes.md`)

Carried into the ledger so that a reader of this file alone has the transport and dependency facts the spikes build on.
These six were established in T0.1-T0.3, not in this task.

| # | Finding | Consequence |
| --- | --- | --- |
| F1 | `hyper_rustls::HttpsConnectorBuilder::with_tls_config` asserts that `ClientConfig.alpn_protocols` is empty and aborts the process otherwise; it fills ALPN itself from `enable_http1()`/`enable_http2()` | the SDK's own `ClientConfig` must leave `alpn_protocols` empty |
| F2 | rustls-platform-verifier 0.7.0 has no `tls_config()`; the entry points are `BuilderVerifierExt::with_platform_verifier()` / `ConfigVerifierExt`, and `Verifier::new_with_extra_roots(roots, provider) -> Result<Self, TlsError>` | extra roots need the explicit `Verifier`, not hyper-rustls' own helper |
| F3 | The crypto provider must be named explicitly: `builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))` | a second provider entering the graph cannot make the process default ambiguous |
| F4 | A cold `http2_only(true)` hyper-util legacy client sent 50 concurrent h2c requests over exactly 1 accepted connection | the TLS `Auto` case was the open question S2b answers below |
| F5 | An rcgen self-signed certificate with SAN `127.0.0.1` works directly as a rustls trust anchor; no CA/leaf pair is needed | the TestServer needs one certificate, not a chain |
| F6 | `cargo deny check` is clean (advisories, bans, licenses, sources) for the workspace and for the full runtime set in `spikes/deps-probe`; duplicate versions are warnings only (`syn` 2.0.119 vs 3.0.6, `getrandom` 0.3.4 vs 0.4.3) | `crates/macros` takes syn 3 in Phase 4 |

---

## S1 - sonic-rs semantics

Binary: `spikes/sonic-probe`. One scenario per process run.

### S1(a) `serde_path_to_error` over `sonic_rs::Deserializer`

```sh
$TARGET/release/sonic-probe a-path
```

| Case | Codec | `error.path()` | `error.inner()` |
| --- | --- | --- | --- |
| `answers.spam.noul` removed | sonic-rs | `answers.spam` | ``missing field `noul` at line 1 column 101`` |
| `answers.spam.noul` removed | serde_json | `answers.spam` | ``missing field `noul` at line 1 column 101`` |
| `models[1].name` is the integer `123` | sonic-rs | `models[1].name` | `invalid type: integer 123, expected a borrowed string at line 1 column 99` |
| `models[1].name` is the integer `123` | serde_json | `models[1].name` | `invalid type: integer 123, expected a borrowed string at line 1 column 99` |

`serde_path_to_error::deserialize(&mut sonic_rs::Deserializer::from_slice(..))` compiles and tracks paths, and reports
the same path as it does over serde_json. Without it, the bare sonic-rs error carries a line/column and a three-line
ASCII caret excerpt of the input, but no path.

**Decision (plan section 5, S1(a) rule: "works -> use it on the failure path for generic `T`"):** use
`serde_path_to_error` on the failure path for generic `T`.

**Contradicts the plan, small:** AC-F6 requires ``field_path == "answers.spam.noul"`` for a missing `noul`. Neither
codec reports that: a *missing field* is reported at the containing struct (`answers.spam`), because that is where
serde raises it; the field name appears in the inner message instead. Both halves are available, so the SDK can render
`answers.spam.noul` by appending the missing field name from the inner error, but AC-F6 cannot be satisfied by
`error.path().to_string()` alone. Either the SDK composes the path, or AC-F6 is restated as
``path == "answers.spam" && inner contains "missing field `noul`"``. This is the next worker's call to make, in
`error.rs`.

**Open question:** the bare sonic-rs error embeds a multi-line excerpt of the input in its `Display`. For a `state`
carrying PII that excerpt would reach the user's logs. `codec.rs` should decide whether to render sonic errors
verbatim or to strip them to code plus position.

### S1(b) borrowed `&'de str` fields and integer map keys

```sh
$TARGET/release/sonic-probe b-borrow
```

| Target | Input | sonic-rs | serde_json |
| --- | --- | --- | --- |
| `&'a str` | `{"s":"friendly"}` | `Ok`, points into the input buffer | `Ok`, points into the input buffer |
| `&'a str` | `{"s":"a\"b\ncé"}` | `Err invalid type: string ..., expected a borrowed string` | same error |
| `Cow<'a, str>` | `{"s":"friendly"}` | `Ok`, borrowed, points into the input | not measured |
| `Cow<'a, str>` | `{"s":"a\"b\ncé"}` | `Ok`, **owned** (the unescaped form is a fresh allocation) | not measured |
| `HashMap<u32, f64>` | `{"0":0.1,"1":0.9}` | `Ok {0: 0.1, 1: 0.9}` | not measured |
| `BTreeMap<u32, f64>` | `{"0":0.1,"1":0.9}` | `Ok {0: 0.1, 1: 0.9}` | `Ok {0: 0.1, 1: 0.9}` |
| `BTreeMap<LevelKey, f64>`, newtype whose visitor parses `&str` | `{"0":0.1,"1":0.9}` | `Ok {LevelKey(0): 0.1, LevelKey(1): 0.9}` | same |

`LevelKey`'s visitor implements both `visit_str` and `visit_u64`; both codecs took `visit_str`, so an object key
reaches the visitor as text and the newtype parses it.

**Decision:** score levels can be stored as integers without allocating, either through `u32` directly or through a
newtype visitor; both work on both codecs. Borrowing a `&'de str` out of the response body is available but only for
values with no escape sequences, so any field that may contain an escape must be `Cow<'de, str>` or owned. Zero-copy
answer names are therefore possible but not unconditional.

### S1(c) constant C for AC-P2

```sh
$TARGET/release/sonic-probe c-borrowed
$TARGET/release/sonic-probe c-ignoredany
$TARGET/release/sonic-probe c-owned
```

Fixture: the upstream `RESULT` (`tests/test_clients.py:42-56`) as compact JSON, 364 bytes. Second identical call.

| Target | blocks | bytes |
| --- | ---: | ---: |
| Allocation-free borrowed probe (`&str`, `f64`, named fields, no `String`/`Vec`/map) | **0** | **0** |
| `serde::de::IgnoredAny` | 0 | 0 |
| Owned control (`String` + `HashMap<String, sonic_rs::Value>`), not a budget, only proof the harness measures | 8 | 1765 |

**C = 0 blocks, 0 bytes.** sonic-rs allocates nothing to decode a 364-byte document into a fully borrowed target, and
its skip path allocates nothing either, so the two do not differ. The owned control shows the counters are live.

**Consequence for AC-P2:** the budget `<= 14 + C` blocks is `<= 14` blocks. Every block in a decode is one the SDK's own
representation asked for; none is the codec's.

### S1(d) nesting

Each shape runs in its own process; the shell records the exit status, because a stack overflow terminates the process
rather than returning an error.

```sh
$TARGET/release/sonic-probe nest <shape> <depth> <stack-kib>   # stack-kib 0 = the main thread (8 MiB on macOS)
```

Depth 100,000, input `[[[...]]]` (200,000 bytes), and the same nested inside `{"legend": ...}`:

| Shape | main thread (8 MiB) | 2 MiB thread |
| --- | --- | --- |
| `sonic_rs::Value` | **SIGABRT, "has overflowed its stack"** | **SIGABRT** |
| `serde::de::IgnoredAny` through sonic-rs | **SIGABRT** | **SIGABRT** |
| `sonic_rs::OwnedLazyValue` | **SIGABRT** | **SIGABRT** |
| the same three inside a `legend` field | **SIGABRT** | **SIGABRT** |
| `serde_json::Value` | `Err recursion limit exceeded at line 1 column 128` | same |
| `IgnoredAny` through serde_json | `Ok` (its skip path is iterative) | same |

Highest depth at which the process still exits normally, found by bisection:

| Shape | release, 2 MiB | release, 8 MiB | dev, 2 MiB | dev, 8 MiB |
| --- | ---: | ---: | ---: | ---: |
| `sonic_rs::Value` | 9,409 | 37,120 | 43 | 175 |
| `IgnoredAny` (sonic skip path) | 8,785 | 34,649 | **24** | 97 |
| `OwnedLazyValue` | 8,784 | 34,647 | 24 | 96 |
| `Value` inside `legend` | 7,318 | 28,870 | 40 | 162 |
| `IgnoredAny` inside `legend` | 8,784 | 34,647 | not measured | not measured |
| `OwnedLazyValue` inside `legend` | 8,782 | 34,645 | not measured | not measured |

Stack consumed per nesting level, from the same bisection at four stack sizes (`IgnoredAny` shape):

| Stack | dev max depth | dev bytes/level | release max depth | release bytes/level |
| ---: | ---: | ---: | ---: | ---: |
| 512 KiB | 6 | 87,381 | 2,232 | 234 |
| 1 MiB | 12 | 87,381 | 4,416 | 237 |
| 2 MiB | 24 | 87,381 | 8,785 | 238 |
| 4 MiB | 49 | 85,598 | not measured | |
| 8 MiB | 97 | 86,480 | 35,000 | 239 |

**Result: sonic-rs 0.5.10 has no effective recursion limit on any path the SDK uses.** Its serde `Deserializer` does
carry one (`src/serde/de.rs:23`, `MAX_ALLOWED_DEPTH: u8 = u8::MAX`), but `deserialize_ignored_any` calls
`parser.skip_one(true)` directly (`src/serde/de.rs:889`) and the `Value` and `OwnedLazyValue` paths have parsers of
their own, so none of the three passes through `with_depth_limit`. The failure mode is `SIGABRT`, not an `Err`: it
cannot be caught, and it takes the whole process, not the request.

**Decision (plan section 5, S1(d) rule: "if the skip path or `OwnedLazyValue` recurses without a limit, cap depth in
`codec.rs` and add a fuzz seed"): `codec.rs` must cap depth, and the cap must be enforced BEFORE the bytes reach
sonic-rs.** A depth counter inside a serde `Visitor` cannot help, because the overflow happens inside sonic's own
parser and the visitor is never re-entered. The workable guard is an O(n) pre-scan of the response bytes that counts
`[`/`{` against `]`/`}` outside string literals and rejects the body above the cap. The 100,000-deep array is a fuzz
seed.

**Contradicts the plan, large, and it constrains the test suite as well as the SDK:** in an unoptimized build - which
is what `cargo test`, `cargo nextest` and `cargo fuzz` produce - the skip path costs ~85 KiB of stack per level and
survives only **24 levels on a default 2 MiB tokio worker thread**. A fuzz target seeded with a 100,000-deep array, as
plan section 7 R12 prescribes, will abort immediately in a debug build; so will any test that decodes a document more
than ~24 deep on a worker thread. The depth cap has to be low enough to be enforced before sonic sees the bytes in
every profile, and the alloc/fuzz harnesses need either a release profile or an explicit larger stack. Suggested cap:
128, matching serde_json's own limit, enforced by the pre-scan; it is 5x below the dev-profile 8 MiB main-thread
ceiling of 97 and far below every release ceiling, but note that 128 still exceeds the dev-profile 2 MiB worker
ceiling of 24, so the pre-scan is what protects that case, not the cap value.

### S1(e) appending into a non-empty `Vec<u8>`

```sh
$TARGET/release/sonic-probe e-append
```

Starting from a 9-byte prefix `{"state":`, two `sonic_rs::to_writer(&mut buffer, ..)` calls and two literal splices
produced `{"state":"a \"quoted\" state","model":"jev-latest"}`, which serde_json re-parses into the expected object.
`to_writer` appends; it does not clear.

**Decision:** confirmed. The body splice of plan section 3.3 step 1 and the scratch buffer of S6 can both write through
`&mut Vec<u8>` (the blanket `impl<W: WriteExt + ?Sized> WriteExt for &mut W`, `src/writer.rs:138`).

### S1(f) float parsing

```sh
$TARGET/release/sonic-probe f-float
```

`f64::to_bits` of each parse:

| Literal | sonic-rs | `str::parse` | serde_json | agree |
| --- | --- | --- | --- | --- |
| `0.1` | `0x3fb999999999999a` | `0x3fb999999999999a` | `0x3fb999999999999a` | yes |
| `1e-7` | `0x3e7ad7f29abcaf48` | same | same | yes |
| `0.30000000000000004` | `0x3fd3333333333334` | same | same | yes |
| `5e-324` | `0x0000000000000001` | same | same | yes |
| `-0.0` | `0x0000000000000000` | `0x8000000000000000` | `0x8000000000000000` | **NO** |
| `-0` | `0x0000000000000000` | `0x8000000000000000` | `0x8000000000000000` | **NO** |
| `-0.0e5` | `0x0000000000000000` | `0x8000000000000000` | `0x8000000000000000` | **NO** |
| `-1e-400` (underflow) | `0x8000000000000000` | same | same | yes |
| `1e-400` (underflow) | `0x0000000000000000` | same | same | yes |
| `1.7976931348623157e308` | `0x7fefffffffffffff` | same | same | yes |
| 30-digit mantissa `1.234567890123456789012345678901` | `0x3ff3c0ca428c59fb` | same | same | yes |
| 30-digit integer `123456789012345678901234567890` | `0x45f8ee90ff6c373e` | same | same | yes |
| `1e309` | `Err "Float number must be finite"` | `inf` | `Err` | both codecs reject |
| `-1e309` | `Err` | `-inf` | `Err` | both codecs reject |
| `0.000000000000000000000000000001` | `0x39b4484bfeebc2a0` | same | same | yes |

The same divergence inside a document, not only as a bare scalar:

| Input | sonic-rs | serde_json |
| --- | --- | --- |
| `{"s":-0.0}` into `struct { s: f64 }` | bits `0x0000000000000000` | bits `0x8000000000000000` |
| `{"s":-0.0}` re-rendered through a `Value` | `{"s":0.0}` | `{"s":-0.0}` |
| `[-0.0]` re-rendered through a `Value` | `[0.0]` | `[-0.0]` |

**Result: sonic-rs 0.5.10 drops the sign of a literal negative zero.** `-0.0`, `-0` and `-0.0e5` all decode to `+0.0`.
The sign is preserved when the value reaches zero by *underflow* (`-1e-400` gives `-0.0` correctly), so the bug is in
the literal-zero path, not in the sign handling generally. Both codecs reject values outside f64 range rather than
returning an infinity, and agree on every other case measured, including subnormals, the 30-digit mantissa and both
underflows.

**Contradicts the plan, small but blocking for one criterion:** AC-Q3 compares decoded numbers with serde_json
`float_roundtrip` by `f64::to_bits` and names `-0.0` as one of the generated forms. That comparison fails today. The
next worker must add negative zero to AC-Q3's enumerated list of accepted divergences, or AC-Q3 cannot pass. For the
API itself the impact is nil - `noul`, `confidence`, `score` and probabilities are never negative zero in a meaningful
sense - but it must be written down rather than discovered by a failing test.

---

## S5 - how dhat accounts for re-allocation

```sh
$TARGET/release/sonic-probe s5-realloc
```

A `Vec<u8>` grown through a ladder of sizes, with the pointer compared before and after each step:

| Step | blocks | bytes | curr_bytes | pointer moved |
| --- | ---: | ---: | ---: | --- |
| `vec![0u8; 1024]` | 1 | 1,024 | +1,024 | n/a (fresh) |
| grow 1,024 -> 4,096 | 1 | 4,096 | +3,072 | yes |
| grow 4,096 -> 65,536 | 1 | 65,536 | +61,440 | yes |
| grow 65,536 -> 1,048,576 | 1 | **1,048,576** | +983,040 | **no** |
| grow 1,048,576 -> 8,388,608 | 1 | 8,388,608 | +7,340,032 | yes |
| `shrink_to(1024)` while `len == capacity == 8,388,608` (a no-op in effect) | 1 | **8,388,608** | +0 | no |
| `truncate(1024)` + `shrink_to_fit()` from capacity 8,388,608 | 1 | **1,024** | -8,387,584 | yes |

**Conclusion, in one sentence: dhat charges every allocator call, including a `realloc` that keeps the pointer and even
one that does not change the size, as one block whose `total_bytes` contribution is the FULL NEW SIZE rather than the
growth delta - so AC-P1's rule "bytes allocated during the call <= 1.25x body length + 64 KiB" does catch a
re-allocation, because a buffer that grows from n to m inside the call contributes m, not m - n.**

Two details the next worker needs: `total_bytes` of a shrink is the new size, not the old, so a shrink is cheap in the
byte bound but still costs one block; and `curr_blocks` stays at +0 across a realloc, so a budget expressed in
`curr_blocks` would not see re-allocation at all.

---

## S6 - body encode buffer

Binary: `spikes/encode-buffer`. Body shape
`{"state":<state>,"model":"jev-latest","questions":<297 bytes of pre-serialized JSON>}`. States: English-like text with
straight quotes, newlines and multi-byte characters (`é`, `€`, `—`) at 1 KB / 64 KB / 1 MB, and an object
`{"subject": <64 B>, "body": <1 MB>}`.

### Variants

| Name | Strategy |
| --- | --- |
| `i` | one fresh `Vec<u8>` per call, pre-sized from the previous call's post-encode `capacity()` |
| `ii` | exact-size two-pass: measure the escaped length, allocate exactly that, write once with a hand-written scalar escaper. Object states fall through to `iii` |
| `ii-simd` | `ii` with `json_escape_simd::escape_into` in place of the scalar escaper |
| `iii` | thread-local retained scratch, taken out of the thread-local (never borrowed across the caller's `Serialize`), decayed hint `hint = max(len, hint - hint/16)`, shrink to the `8 * hint` ceiling, then an exact copy into a right-sized body buffer |
| `iii-1shot` | `iii`, but the shrink goes in ONE step down to `hint` instead of down to the ceiling |
| `iv` | `ii` for string states, `iii` for object states |
| `iv-1shot` | `ii` for string states, `iii-1shot` for object states |

### Correctness first

```sh
$TARGET/release/encode-buffer verify
```

All seven variants produce byte-identical bodies for all four states. The scalar escaper's output equals
`sonic_rs::to_string` for the English-like filler and for `"`, `\`, `/`, every control character in
U+0000-U+001F, U+007F (DEL), U+00A0 (no-break space), U+2028 (line separator), U+2029 (paragraph separator),
the astral character U+1F600 and the non-ASCII sample `é€—` (U+00E9, U+20AC, U+2014); `escaped_len` predicts the
exact written length in every case. `json_escape_simd` also matches sonic-rs byte for byte.

Those characters are named in notation rather than written out: the corpus itself lives in
`spikes/encode-buffer/src/main.rs`, and putting raw control bytes in a Markdown file turns it into something
`file` calls data, git diffs as binary and grep skips.

### `json-escape-simd`: can it write into a caller-provided buffer?

**Yes, but not into an exactly-sized one, which is what variant (ii) needs.** `escape_into(value, dst)` appends to a
caller `Vec<u8>`, but its first statement is `dst.reserve(value.len() * 6 + 32 + 3)`, because its SIMD kernels perform
speculative full-register stores past the logical end (escape-simd 0.1.0 `src/json.rs:18-25`, with that reasoning in
its own comment). That is the same `len * 6 + 35` reservation the variant exists to avoid, so the crate cannot serve as
an exact-size escaper. It is measured below to show that, not because it can work.

### Variant x state, second identical call

```sh
for v in i ii ii-simd iii iii-1shot iv iv-1shot; do
  for s in s1k s64k s1m obj1m; do $TARGET/release/encode-buffer variant "$v" "$s"; done
done
```

| Variant | State | body bytes | blocks | bytes allocated | bytes / body | retained capacity | hint | retained / hint |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| i | 1 KB text | 1,385 | 2 | 6,212 | 4.49x | 0 | 6,188 | - |
| i | 64 KB text | 67,121 | 2 | 393,284 | 5.86x | 0 | 393,260 | - |
| i | 1 MB text | 1,068,796 | 2 | 6,291,524 | 5.89x | 0 | 6,291,500 | - |
| i | 1 MB object | 1,068,884 | 2 | 6,291,611 | 5.89x | 0 | 6,291,587 | - |
| ii | 1 KB text | 1,385 | **1** | **1,385** | **1.00x** | 0 | 0 | - |
| ii | 64 KB text | 67,121 | 1 | 67,121 | 1.00x | 0 | 0 | - |
| ii | 1 MB text | 1,068,796 | 1 | 1,068,796 | 1.00x | 0 | 0 | - |
| ii | 1 MB object (falls through to iii) | 1,068,884 | 1 | 1,068,884 | 1.00x | 6,291,587 | 1,068,884 | 5.89x |
| ii-simd | 1 KB text | 1,385 | 3 | 7,597 | 5.49x | 0 | 0 | - |
| ii-simd | 64 KB text | 67,121 | 3 | 460,405 | 6.86x | 0 | 0 | - |
| ii-simd | 1 MB text | 1,068,796 | 3 | 7,360,320 | 6.89x | 0 | 0 | - |
| ii-simd | 1 MB object | 1,068,884 | 1 | 1,068,884 | 1.00x | 6,291,587 | 1,068,884 | 5.89x |
| iii | 1 KB text | 1,385 | **1** | **1,385** | **1.00x** | 6,188 | 1,385 | 4.47x |
| iii | 64 KB text | 67,121 | 1 | 67,121 | 1.00x | 393,260 | 67,121 | 5.86x |
| iii | 1 MB text | 1,068,796 | 1 | 1,068,796 | 1.00x | 6,291,500 | 1,068,796 | 5.89x |
| iii | 1 MB object | 1,068,884 | 1 | 1,068,884 | 1.00x | 6,291,587 | 1,068,884 | 5.89x |
| iv | 1 KB text | 1,385 | 1 | 1,385 | 1.00x | 0 | 0 | - |
| iv | 64 KB text | 67,121 | 1 | 67,121 | 1.00x | 0 | 0 | - |
| iv | 1 MB text | 1,068,796 | 1 | 1,068,796 | 1.00x | 0 | 0 | - |
| iv | 1 MB object | 1,068,884 | 1 | 1,068,884 | 1.00x | 6,291,587 | 1,068,884 | 5.89x |

`iii-1shot` and `iv-1shot` are identical to `iii` and `iv` on a single repeated size; they differ only in the
mixed-size sequence below.

AC-P1 asks for `<= 2` blocks, bytes allocated `<= 1.25 x body + 64 KiB`, and retained scratch `<= 8x` the decayed hint,
for BOTH string and object states:

| Variant | blocks <= 2 | bytes within bound | retained <= 8x hint | meets AC-P1 |
| --- | --- | --- | --- | --- |
| i | yes (2) | **no**: 393,284 > 149,437 at 64 KB, 6,291,524 > 1,401,531 at 1 MB | n/a | **no** |
| ii | yes (1) | yes (1.00x) | yes (5.89x) | **yes** |
| ii-simd | **no (3)** | **no** (6.89x at 1 MB) | yes | **no** |
| iii | yes (1) | yes (1.00x) | yes (4.47-5.89x) | **yes** |
| iii-1shot | yes (1) | yes | yes | **yes** |
| iv | yes (1) | yes | yes | **yes** |
| iv-1shot | yes (1) | yes | yes | **yes** |

Variant `i` is the one the 6x reservation was expected to sink, and it does: the hint that tracks post-encode
`capacity()` removes the re-allocation (2 blocks, not 3) but the buffer it allocates is the 6x one, so every call
allocates ~5.9x the body it sends.

### Mixed-size sequence: 1 MB, then sixteen 1 KB calls

```sh
$TARGET/release/encode-buffer mixed <variant>
```

| Call | State | `i` blocks / bytes | `iii` blocks / bytes / retained | `iii-1shot` blocks / bytes / retained | `iv` blocks / bytes / retained |
| ---: | --- | --- | --- | --- | --- |
| 0 | 1 MB | 3 / 6,291,533 | 3 / 7,360,305 / 6,291,500 | 3 / 7,360,305 / 6,291,500 | 1 / 1,068,796 / 0 |
| 1 | 1 KB | 2 / 6,291,524 | 1 / 1,385 / 6,291,500 | 1 / 1,385 / 6,291,500 | 1 / 1,385 / 0 |
| 2 | 1 KB | 2 / 6,291,524 | 1 / 1,385 / 6,291,500 | 1 / 1,385 / 6,291,500 | 1 / 1,385 / 0 |
| 3 | 1 KB | 2 / 6,291,524 | 1 / 1,385 / 6,291,500 | 1 / 1,385 / 6,291,500 | 1 / 1,385 / 0 |
| 4 | 1 KB | 2 / 6,291,524 | 1 / 1,385 / 6,291,500 | 1 / 1,385 / 6,291,500 | 1 / 1,385 / 0 |
| 5 | 1 KB | 2 / 6,291,524 | **2 / 6,193,553** / 6,192,168 | **2 / 775,406** / 774,021 | 1 / 1,385 / 0 |
| 6 | 1 KB | 2 / 6,291,524 | **2 / 5,806,545** / 5,805,160 | 1 / 1,385 / 774,021 | 1 / 1,385 / 0 |
| 7 | 1 KB | 2 / 6,291,524 | **2 / 5,443,729** / 5,442,344 | 1 / 1,385 / 774,021 | 1 / 1,385 / 0 |
| 8 | 1 KB | 2 / 6,291,524 | 2 / 5,103,585 / 5,102,200 | 1 / 1,385 / 774,021 | 1 / 1,385 / 0 |
| 9 | 1 KB | 2 / 6,291,524 | 2 / 4,784,705 / 4,783,320 | 1 / 1,385 / 774,021 | 1 / 1,385 / 0 |
| 10 | 1 KB | 2 / 6,291,524 | 2 / 4,485,753 / 4,484,368 | 1 / 1,385 / 774,021 | 1 / 1,385 / 0 |
| 11 | 1 KB | 2 / 6,291,524 | 2 / 4,205,481 / 4,204,096 | 1 / 1,385 / 774,021 | 1 / 1,385 / 0 |
| 12 | 1 KB | 2 / 6,291,524 | 2 / 3,942,729 / 3,941,344 | 1 / 1,385 / 774,021 | 1 / 1,385 / 0 |
| 13 | 1 KB | 2 / 6,291,524 | 2 / 3,696,401 / 3,695,016 | 1 / 1,385 / 774,021 | 1 / 1,385 / 0 |
| 14 | 1 KB | 2 / 6,291,524 | 2 / 3,465,465 / 3,464,080 | 1 / 1,385 / 774,021 | 1 / 1,385 / 0 |
| 15 | 1 KB | 2 / 6,291,524 | 2 / 3,248,961 / 3,247,576 | 1 / 1,385 / 774,021 | 1 / 1,385 / 0 |
| 16 | 1 KB | 2 / 6,291,524 | 2 / 3,045,993 / 3,044,608 | 1 / 1,385 / 774,021 | 1 / 1,385 / 0 |

**Contradicts the plan, and it is a defect in the rule rather than in the code:** the decay-and-shrink rule as plan
section 3.3 step 1 states it - "shrunk when it exceeds 8x the recent body size", with the hint decaying a sixteenth per
call - re-allocates the whole scratch **on every call** from the moment the condition first holds. In the sequence
above, variant `iii` allocates between 3.0 MB and 6.2 MB per 1 KB request for twelve consecutive calls, and it would
keep doing so for dozens more. The cause is that shrinking exactly to the ceiling leaves the capacity at the ceiling
while the hint keeps decaying, so the condition holds again on the next call.

Shrinking in ONE step down to the hint (`iii-1shot`) fixes it: one re-allocation of 775,406 bytes at call 5, then
1 block / 1,385 bytes for every call after it. The condition only becomes true again after the hint has decayed by
another factor of eight, so the number of re-allocations after an outlier is logarithmic rather than linear.

Variant `i` never shrinks at all: it keeps allocating 6,291,524 bytes for a 1,385-byte body indefinitely, because its
hint tracks `capacity()` and therefore never decays.

### Body-type conversion

```sh
$TARGET/release/encode-buffer bytes-convert
```

Second identical shape, for a 1 KiB and a 1 MiB payload (both sizes gave identical counts):

| Source | conversion blocks / bytes | first `clone()` blocks / bytes | second `clone()` blocks |
| --- | ---: | ---: | ---: |
| `Bytes::from(Vec)`, `len == capacity` | 0 / 0 | **1 / 24** | 0 |
| `Bytes::from(Vec)`, `len != capacity` (a codec-grown buffer) | **1 / 24** | 0 / 0 | 0 |
| `BytesMut::freeze()`, `len == capacity` | 0 / 0 | **1 / 24** | 0 |

**Confirms plan section 3.3 step 5:** the first `clone()` of a `Vec`-backed `Bytes` costs 1 block of 24 bytes (the
shared header), and skipping the clone when `max_retries == 0` saves exactly that. Handing over an exactly-full buffer
is free; handing over a buffer with slack pays the same 24 bytes immediately, whether or not it is ever cloned, which
is a second reason for the body buffer to be exactly sized.

### Wall clock (report-only)

```sh
cd spikes/encode-buffer && env -u RUSTFLAGS cargo --config ~/.config/rust/config.dev.toml bench
```

divan 0.1.21, timer precision 41 ns, 100 samples each, no RUSTFLAGS, run alone. Median of a second identical call:

| Variant | 1 KB text | 64 KB text | 1 MB text | 1 MB object |
| --- | ---: | ---: | ---: | ---: |
| i | 187.2 ns | 9.415 µs | 152.9 µs | 152.7 µs |
| ii (scalar two-pass) | 1.853 µs | 115.9 µs | **1.874 ms** | 165.1 µs |
| ii-simd | 780.9 ns | 44.24 µs | 743.3 µs | 165.0 µs |
| iii | **187.2 ns** | **10.62 µs** | **165.2 µs** | 165.7 µs |
| iii-1shot | 187.2 ns | 10.58 µs | 166.7 µs | 165.5 µs |
| iv | 1.874 µs | 115.7 µs | 1.920 ms | 168.1 µs |
| iv-1shot | 1.937 µs | 115.5 µs | 1.939 ms | 165.5 µs |

No acceptance criterion depends on these; they are here because they change the recommendation. The hand-written
scalar escaper is **11x slower** than sonic-rs on a 1 MB string (1.874 ms against 165.2 µs) and **10x slower** on 1 KB
(1.853 µs against 187.2 ns); `json-escape-simd` is faster than the scalar loop but still 4.5x slower than routing the
same string through sonic-rs into a pre-grown buffer.

### S6 decision

The plan's rule: among the variants meeting AC-P1's bounds for both string and object states, the tie-break on this
macOS machine is bytes allocated per call, because instruction counts need Linux and valgrind.

Variants meeting the bounds: `ii`, `iii`, `iii-1shot`, `iv`, `iv-1shot`. **All five allocate exactly the body length
per call (1.00x), so the stated tie-break does not separate them.** The rule is therefore exhausted, and the
recommendation below rests on the next measurements in the table, stated explicitly rather than folded in silently:

**Recommended winner: `iii-1shot` - the thread-local retained scratch with a decaying hint and a one-shot shrink.**

- It is the fastest variant at every state size (187 ns / 10.6 µs / 167 µs / 166 µs), tied with `i` and 10-11x ahead of
  every two-pass variant, because the state string goes through sonic-rs' SIMD writer instead of a scalar loop.
- It allocates 1 block of exactly the body length per call, the joint best in the table.
- It needs no second escaper implementation. `ii` and `iv` require a hand-written escaper that must stay byte-identical
  to the codec for ever - the sort of thing AC-F7(a) already has to prove for the derive macro - and it buys nothing
  that `iii-1shot` does not already have.
- The `-1shot` shrink is a departure from the plan's wording and is required: see the mixed-size table.

Cost of the recommendation, stated plainly: `iii-1shot` retains up to ~6x the largest recent body per runtime worker
thread (6.29 MB after a 1 MB state), where `iv` retains nothing for string states. The decay plus the one-shot shrink
bounds that over time and it satisfies AC-P1's `<= 8x hint` rule, but on a 16-worker runtime a burst of 1 MB states
leaves ~100 MB retained until the hint decays. If that matters more than 11x the encode CPU, `iv-1shot` is the
alternative, and the choice belongs to the lead, not to this spike.

**Open question for the freeze worker:** an absolute ceiling on the retained scratch (drop it entirely when a body
exceeded, say, 256 KiB) was not measured. It would bound the retained memory without the 11x CPU of the two-pass path,
and it is a cheaper experiment than either variant here.

**No variant failed the bounds for reasons that implicate D2.** sonic-rs' 6x reservation is fully absorbed by a
retained scratch plus an exact copy, so the escalation clause of AC-P1 ("if no sonic-rs based variant meets these
bounds, execution STOPS") is not triggered.

---

## S2a - TLS, ALPN and extra roots

Binary: `spikes/transport-probe`. The live endpoint is contacted **without an API key**; 403 is the expected answer and
no credential is read, set or sent anywhere in this crate.

```sh
cd spikes/transport-probe && env -u RUSTFLAGS cargo --config ~/.config/rust/config.dev.toml build --release
$TARGET/release/transport-probe s2a
```

The client is built the way the SDK will build it: hyper-util legacy `Client` over a hyper-rustls connector
(`default-features = false`, features `http1`, `http2`, `tls12`, `aws-lc-rs`) with a `rustls::ClientConfig` of this
crate's own, `builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))`, the platform verifier,
and `alpn_protocols` left **empty**.

| # | Case | Result |
| --- | --- | --- |
| 1 | `GET https://api.typesafe.ai/v1/models`, no `Authorization`, plain platform verifier | status **403**, version **HTTP/2.0** |
| 3 | loopback TLS TestServer, **no** extra root | handshake refused: `invalid peer certificate: Other(OtherError("“rcgen self signed cert” certificate is not trusted: -67843"))`; the server accepted the TCP connection (+1) and the client dropped it |
| 2 | loopback TLS TestServer, extra root = the server's certificate | status **200**, version **HTTP/2.0**, 1 connection |
| 2 | the SAME extra-roots config against the live API | status **403**, version **HTTP/2.0** |

Response headers of the unauthenticated live call, verbatim:

```
content-length: 118
content-type: application/json
date: Fri, 18 Sep 2026 19:06:33 GMT
server: istio-envoy
x-envoy-upstream-service-time: 7
x-typesafe-request-id: req_01a0b5e9ce0b7e9394746c6818917706
```

Body: `{"detail":{"error_type":"authentication_error","message":"Must supply an API key! Check your request and try again."}}`

**Decision:** the configuration works. ALPN negotiates h2 against both the live API and the loopback server;
`Verifier::new_with_extra_roots` ADDS to the operating system store rather than replacing it, which is what
`ClientBuilder::add_root_certificate` needs; and a client that has not added the root is refused, so the negative case
is a real check and not a silent pass.

**Stated explicitly: this proves `add_root_certificate` on macOS only. Linux and Windows remain unproven until CI can
run them**, and rustls-platform-verifier takes a different code path on each (`src/verification/apple.rs`,
`.../linux.rs`, `.../windows.rs`). The Phase 0 exit gate says "CI green on 3 OSes"; push is not authorized in this run,
so that part of the gate cannot be closed here.

Two facts worth carrying into Phase 2: the request-id header is spelled **`x-typesafe-request-id`**, and the 403 body
matches AC-F4's expected `error_type` and message exactly, so that criterion is confirmed against the live server
rather than against the documentation.

## S2b - cold fan-out connection count

```sh
$TARGET/release/transport-probe s2b
```

64 concurrent `GET` requests against the HTTP/2 + TLS TestServer, 10 repetitions, each repetition on a **brand-new**
client. The base URL is an IP literal, so no second socket is raced across resolved addresses. Connections are counted
twice independently: by the server at accept time, and inside the client by a connector wrapper whose counter goes up
when a stream is produced and down when it is dropped. The two counts agree in every row.

| Case | warm | min | median | max | client opened | live 1 s later |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| A: `Auto` (`enable_http1().enable_http2()`, pool default) | no | **64** | **64** | **64** | 64 | 1 |
| A: `Auto` | yes | 0 | 0 | 0 | 1 | 1 |
| B: `http2_only(true)`, ALPN h2 only | no | **1** | **1** | **1** | 1 | 1 |
| B: `http2_only(true)` | yes | 0 | 0 | 0 | 1 | 1 |

min/median/max are the connections the server accepted for the 64-way fan-out itself; "warm = yes" means one request
completed first, and then the 64 opened **zero** new connections in both modes. 2,580 requests were served in total
across the four cases.

The **minimum of case A cold varies between runs**: 64 in the run tabulated above, 3 in the lead's re-run of the same
binary (median 64, max 64, client_opened 64 in both). It depends on how far the first handshake gets before the other
63 tasks are polled, which is a scheduling race. The decision does not rest on it: what matters is that A's median and
maximum are 64 while B was 1/1/1 in both runs.

**Decision (plan section 5, S2b rule: "if `http2_only` yields 1 and `Auto` yields > 1, the default for `https` base
URLs is `Http2Only`, with `Auto` as the documented knob"): the rule fires exactly. `Http2Only` is the default for
`https` base URLs; `Auto` is the documented knob for HTTP/1.1-only proxies; `http://` base URLs always use `Auto`.**

Two details for Phase 2. Under `Auto` the pool does collapse to a single connection afterwards - 64 opened, 1 alive a
second later - so the cost is 64 TLS handshakes on the cold path, not 64 connections held. And AC-P4(c) is already
visible here: after one warm-up request, 64 concurrent calls open zero new connections under either mode, which is why
`warm_up()` is worth documenting before a fan-out even once `Http2Only` is the default.

## S4 - the server's HTTP/2 SETTINGS

```sh
$TARGET/release/transport-probe s4
```

Read twice from `api.typesafe.ai:443`, with no credential. First by writing the HTTP/2 preface by hand and decoding the
server's first SETTINGS frame off the TLS stream, which gives every parameter; then through the `h2` crate, which is
what hyper's HTTP/2 client uses underneath.

ALPN negotiated `h2` on both connections.

| Parameter | Value |
| --- | ---: |
| `SETTINGS_HEADER_TABLE_SIZE` | 4,096 |
| `SETTINGS_ENABLE_CONNECT_PROTOCOL` | 0 |
| `SETTINGS_MAX_CONCURRENT_STREAMS` | **1,024** |
| `SETTINGS_INITIAL_WINDOW_SIZE` | **16,777,216** (16 MiB) |
| `SETTINGS_MAX_FRAME_SIZE` | not sent, so the protocol default 16,384 applies |
| `SETTINGS_MAX_HEADER_LIST_SIZE` | not sent, so unlimited |

Immediately after its SETTINGS the server sent a connection-level `WINDOW_UPDATE` on stream 0 of **+25,100,289**, which
takes the connection send window to 25,165,824 bytes (24 MiB) from the protocol default of 65,535.

Through `h2`:

| Reading | Value |
| --- | ---: |
| `Connection::max_concurrent_send_streams()` | 1,024 |
| first `poll_capacity` grant on a fresh stream, after `reserve_capacity(16 MiB)` | **409,600** |
| `SendStream::capacity()` after that grant | 409,600 |

**Decision (plan section 5, S4 rule): neither branch of the rule fires, and nothing changes client-side.**
`SETTINGS_INITIAL_WINDOW_SIZE` is 16 MiB, far above the 1 MiB threshold, so there is no upload ceiling to document -
and it is the server's receive window in any case, which a client must not try to raise.
`SETTINGS_MAX_CONCURRENT_STREAMS` is 1,024, far above 100, so there is no fan-out limit to document either: the SDK's
own concurrency will be the binding constraint long before the server's.

One finding that is not in the plan's rule and matters for a large `state`: the 409,600-byte first grant is **not** the
server's window. It is `h2`'s own client-side send buffer cap, `proto::DEFAULT_MAX_SEND_BUFFER_SIZE = 1024 * 400`
(h2 0.4.19 `src/proto/mod.rs:47`), settable through `h2::client::Builder::max_send_buffer_size`. A multi-megabyte body
is therefore written in 400 KiB instalments as capacity is released, regardless of the 16 MiB the server advertises.
That is correct behaviour and needs no change, but it is the number to look at if a large upload is ever found to be
slower than the link allows - not the server's window.

**Keep-alive behaviour is NOT measured here.** Whether the load balancer counts an HTTP/2 PING as activity needs an
authenticated idle test (plan section 8 step 13), which is out of scope for this task.

---

## AC-P0 - itemized allocation inventory

Binary: `spikes/alloc-inventory`. Every step of plan section 3.3 prototyped and measured on the **second identical
call**, after one warm-up pass of the same shape. The body encoder is not re-implemented here: the crate path-depends
on `spikes/encode-buffer` and measures the S6 winner itself.

```sh
cd spikes/alloc-inventory && env -u RUSTFLAGS cargo --config ~/.config/rust/config.dev.toml build --release
$TARGET/release/alloc-inventory steps
$TARGET/release/alloc-inventory decode
$TARGET/release/alloc-inventory verify
```

### Steps 1 to 5: request assembly

| Step | Item | blocks | bytes |
| --- | --- | ---: | ---: |
| 1 | body encode, 1 KB string `state`, S6 winner (`iii-1shot`) | **1** | 1,385 |
| 2 | retain the body for retry (`Bytes::clone`) | **1** | 24 |
| 3 | clone the base `HeaderMap` (6 headers) | **2** | 656 |
| 4 | build the `http::Request` from a pre-parsed `Uri` (headers moved in, not re-cloned) | **0** | 0 |
| 4a | clone the pre-parsed `Uri` on its own, second time | 0 | 0 |
| 4b | clone a freshly parsed `Uri`, **first** time | **0** | 0 |
| 4c | clone that same `Uri`, second time | 0 | 0 |
| 5 | `BodyExt::collect` + `to_bytes` on a single-frame body | **1** | 128 |

The body was 1,385 bytes and the thread's retained scratch was 6,188 bytes afterwards.

Three of these were open questions and are now settled. Cloning an `http::Uri` is **free even the first time** - unlike
`Bytes`, it does not pay a shared-header allocation on first clone - so step 4 costs nothing at all once the body's
own clone is accounted for under step 2. The `HeaderMap` clone costs **2** blocks, which is exactly what AC-P6 assumed.
`BodyExt::collect` costs **1** block for its frame queue, which is also what AC-P6 assumed.

### Steps 6 to 8: decode

| Step | Representation | blocks | bytes |
| --- | --- | ---: | ---: |
| 6 | prototype visitor -> `Vec<(String, Answer)>`, single pass, order independent, pre-sized | **14** | 554 |
| 7 | naive comparator: `#[serde(tag = "type")]` answers in a `HashMap<String, _>`, maps throughout | **26** | 2,672 |
| 8 | derived typed struct (`Ticket { spam, tone, quality }`), the `#[derive(QuestionSet)]` shape | **10** | 275 |

The plan's three budget questions, all on the same codec and the same fixture:

| Question | Measured | Verdict |
| --- | --- | --- |
| is (6) <= 14 + C blocks, with C = 0 from S1(c)? | 14 <= 14 | **yes, exactly** |
| is (6) <= 0.7 x (7)? | 14 <= 18.2 (the ratio is 0.54) | **yes** |
| is (8) < (6)? | 10 < 14 | **yes** |

The prototype's 14 blocks are the plan's own item list, one for one: 1 model `String`, 1 answers `Vec`, 3 question-name
`String`s, 1 choice `String`, 1 choice-probabilities `Vec`, 2 option-name `String`s, 1 legend `Vec`, 3 legend
`String`s, 1 score-probabilities `Vec`. `usage`, every `f64`, every score level and every probability key cost nothing:
levels are parsed from the object key text straight into `u32`.

**Warning for the freeze worker: (6) meets its budget with ZERO headroom.** One extra `String`, one extra `Vec`, one
`Box` in the answer representation and AC-P2 fails. The 0.7x ratio, by contrast, has room (0.54 measured).

### Correctness of the prototype visitor

```sh
$TARGET/release/alloc-inventory verify
```

Decoded twice from a document whose answers, and whose fields within every answer object, are in a different order from
the fixture, and which carries one extra answer of the unknown type `"prediction"`:

| Check | Result |
| --- | --- |
| wire order: sonic-rs result == serde_json result | **true** |
| shuffled order: sonic-rs result == serde_json result | **true** |
| shuffled order yields the same answers, compared as sets | **true** |
| shuffled order yields the same answers, compared in order | false, by design (see below) |
| `model` and `usage` unchanged | true |
| answers kept from the shuffled document | `["quality", "tone", "spam"]` |
| the unknown `"prediction"` answer was dropped | **true** |
| answer count | 3, for 3 questions asked |

The visitor is order independent in the sense that matters: `type` may arrive after the data fields, and it does in the
shuffled document, where `probabilities` is read before the answer type is known. The score's probability keys are then
read as names and converted to levels at dispatch. That path is exercised by the shuffled document and costs nothing in
the measured case, where `type` comes first.

**A design point the next worker has to decide:** every container keeps **wire order**, so a response whose keys arrive
in another order produces the same pairs in another order, and two decodes compare unequal as sequences while comparing
equal as sets. Either the SDK sorts `legend` and the score `probabilities` by level when it builds them - cheap, at
most ten elements, and it makes equality and `Debug` output stable - or every test that compares answers has to
normalize first. This is not covered by any current acceptance criterion.

### Proposed frozen budget table

Proposed only. The lead and a verifier freeze it; nothing here is adopted by this spike.

Every number is from the tables above, with the plan's rule applied: tightening is free, loosening needs a ledger entry
and the user's sign-off. Where a measurement left no headroom, the budget is set at the measurement and said to be
tight.

**AC-P1 - body encode, second identical call, variant `iii-1shot`**

| Bound | Plan as written | Proposed | Measured |
| --- | --- | --- | --- |
| blocks, encode alone | <= 2 | **<= 1** | 1 |
| blocks, including retaining the body for retry | <= 3 (2 + 1) | **<= 2** | 2 |
| bytes allocated during the call | <= 1.25 x body + 64 KiB | **<= 1.05 x body + 4 KiB** | 1.00 x body |
| retained scratch | <= 8x the decayed hint | **<= 8x the decayed hint** (unchanged) | 4.47x - 5.89x |

Carve-out that has to be written into the criterion: a call on which the scratch **shrinks** allocates one extra block
of at most the decayed hint (measured: 775,406 bytes on call 5 of the mixed sequence). AC-P1 is stated for a repeated
identical call, where no shrink happens; the shrink is bounded by the retained-scratch rule instead. Without this
sentence the criterion is ambiguous the first time a test mixes sizes.

**AC-P2 - decode of the 3-answer fixture**

| Bound | Plan as written | Proposed | Measured |
| --- | --- | --- | --- |
| blocks | <= 14 + C | **<= 14** (C = 0, measured in S1(c)) | 14, **tight** |
| bytes | not specified | **<= 700** | 554 |
| ratio to the naive comparator | <= 0.7x | **<= 0.7x** (unchanged; the ratio depends on serde's version as well as on ours) | 0.54x |

**AC-P3 - derived typed decode** uses fewer blocks than AC-P2: 10 < 14, confirmed.

**AC-P6 - full call through an in-memory service, second identical call, 1 KB string `state`, default retry policy
(body retained), no per-call headers**

| Item | blocks | bytes |
| --- | ---: | ---: |
| body encode (step 1) | 1 | 1,385 |
| body retention for retry (step 2) | 1 | 24 |
| `HeaderMap` clone (step 3) | 2 | 656 |
| request assembly from the pre-parsed `Uri` (step 4) | 0 | 0 |
| `collect` frame queue (step 5) | 1 | 128 |
| decode (step 6) | 14 | 554 |
| **total** | **19** | **2,747** |

Proposed AC-P6 budget: **<= 19 blocks** above the floor of calling the same service directly with a pre-built request,
for that pinned scenario. The plan's own assumptions inside AC-P6 - "`HeaderMap` clone = 2, frame queue of `collect` =
1" - are both confirmed rather than assumed.

### Open questions this inventory does not answer

- The floor itself (what an in-memory service costs when called directly with a pre-built request) is not measured
  here; AC-P6 is a difference, and the subtrahend belongs to the Phase 5 harness.
- Every number is for a 1 KB string `state`. An object `state` adds the retained scratch of variant `iii-1shot` to the
  picture but not to the per-call block count; a 1 MB `state` moves only the bytes, not the blocks.
- `tracing` is off in all of these runs. A subscriber's own allocations are not part of any budget here, which matches
  AC-P6's wording ("no `tracing` subscriber").

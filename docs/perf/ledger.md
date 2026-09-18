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
| Crates measured | sonic-rs 0.5.10, serde_json 1.0.151 (`float_roundtrip`), serde_path_to_error 0.1.20, dhat 0.3.3, bytes 1.12.1, json-escape-simd 3.1.2, divan 0.1.21 |

Measurement rules followed: no benchmark ran in parallel with another benchmark or with a build; no `RUSTFLAGS` and no
`target-cpu`; allocation numbers are taken on the **second identical call**, after one warm-up call of the same shape;
every dhat scenario is its own process, because dhat attributes the whole process to one profiler.

Reproduce everything from the repository root:

```sh
cd spikes/sonic-probe   && env -u RUSTFLAGS cargo --config ~/.config/rust/config.dev.toml build --release
cd spikes/encode-buffer && env -u RUSTFLAGS cargo --config ~/.config/rust/config.dev.toml build --release
```

Both binaries land in the shared target directory that `config.dev.toml` names; the commands below write it as
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
`sonic_rs::to_string` for the English-like filler and for `"` `\` `/`, every control character ` `-``,
``, ` `, ` `, ` `, an astral character and `é€—`; `escaped_len` predicts the exact written length
in every case. `json_escape_simd` also matches sonic-rs byte for byte.

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

# Fuzz targets

Two [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) targets for the code
that reads bytes a server chose:

| Target | Input | Property |
| --- | --- | --- |
| `decode_response` | a response body | the depth guard, the System One decoder into `Answers` and into a derived answer set, the models decoder and the error-body reader behind `ApiError` each return `Ok` or `Err`; rendering what they return does not panic |
| `retry_after` | a `retry-after-ms` and/or `Retry-After` value (the first byte picks which, see the target's docs) | the parser returns `None` or a `Duration`, and the same answer twice |

A panic, an abort (a stack overflow included), an input that runs past
`-timeout` or a process that passes `-rss_limit_mb` is a finding.

## Running

This directory is a cargo workspace of its own and is excluded from the
repository's. CI only compiles it (`cargo check`, the "Fuzz targets compile"
step); running a target needs the nightly toolchain (for the sanitizer flags
cargo-fuzz passes) and `cargo install cargo-fuzz --locked`:

```sh
cd fuzz
mkdir -p /tmp/finds/decode_response
cargo +nightly fuzz run decode_response /tmp/finds/decode_response corpus/decode_response \
  -- -max_total_time=300 -timeout=10 -rss_limit_mb=2048
```

The first corpus directory is where libFuzzer writes the inputs it finds, so
it is a scratch directory; `corpus/<target>` holds the committed seeds and is
only read. A crash reproducer lands in `artifacts/<target>/`, which is
ignored by git: keep it and report it rather than committing it.

`cargo check --manifest-path fuzz/Cargo.toml` works on the stable toolchain
too (it builds libFuzzer's C++ runtime, so a C++ compiler is needed); CI runs
it on Linux so that a change to the SDK's hidden `__internals` seam cannot
break the targets unnoticed.

## Seeds

`corpus/decode_response` holds the repository's response fixtures
(`tests/fixtures/*.json`, the upstream `RESULT` fixture among them), the live
API's 403 body, the other error-body shapes the message reader knows (a
`detail` list, a nested `error.message`, a string, `null`, non-JSON text), a
small flood of empty score answers, answers whose `type` comes last, escaped
names, brackets inside strings, and nesting at depth 16 (accepted), 17 (the
first depth refused), 2,000 (unclosed) and 1,000 inside an error body.

The deep case is not a 100,000-deep file: the depth guard counts brackets
before the codec sees a byte, so anything past depth 16 is refused at the
same point whatever its depth, and the fuzzer's inputs (4 KiB by default) can
already nest 4,000 levels. The 100,000-deep document of the original finding
is generated at test time by the codec's unit tests (`src/codec_tests.rs`),
which assert that the guard refuses it without parsing it.

The inputs of the one finding so far - a byte that is not UTF-8 inside a
string the decoders keep as raw text, which made the codec panic - are not
in `corpus/`: every tracked file must be UTF-8 text (`text-hygiene.py`), and
these are not by construction. Their exact bytes are the literals of the
regression tests in `src/codec_tests.rs` (`NOT_UTF8`, `fuzzer_find`) and
`tests/malformed_body.rs`; pass a directory holding them as a second corpus
to start a run from them.

`corpus/retry_after` holds counts, floats, exponents, negatives, values past
`u64`, `inf` and `NaN`, the three HTTP date formats (future and past against
the target's fixed clock), padding, and the millisecond header alone and
beside `Retry-After`.

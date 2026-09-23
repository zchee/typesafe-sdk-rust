# Line coverage

Measured at commit `86a4ba4` with
`cargo llvm-cov nextest -p typesafe-sdk-rust --all-features --fail-under-lines 85 --show-missing-lines`;
every line number below is a line of that commit.

The published crate `typesafe-sdk-rust` is held to 85% line coverage by CI's
`coverage` job. This page records the measured total and every line the tests
do not reach, with the reason it is not reached.

The CI form (`.github/workflows/ci.yaml`, job `coverage`) is the same command
without `--show-missing-lines`. The list below was measured on macOS arm64
with rustc 1.98.1 and cargo-llvm-cov.

## Total

| Lines | Missed | Line coverage | Regions | Missed | Functions | Missed |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 4,287 | 151 | **96.48%** | 6,744 | 318 | 741 | 32 |

A line counts as covered when any test executes it in any instantiation of the
code it belongs to.

The summary's 151 missed lines are more than the 134 this page lists because
llvm-cov's summary counts a line as missed when one instantiation of its
function does not run it, even when another instantiation does, while
`--show-missing-lines`, like this page's rule above, lists only the lines no
instantiation runs.

## Uncovered lines

"Cheap to cover" marks a line a short test would reach.

### `src/__internals.rs`

| Lines | Why |
| --- | --- |
| 50-52, 124-130, 135-141, 146-151, 161-163 | Thin wrappers that exist for the benchmarks and the fuzz targets (`check_depth`, `decode_list_models`, `api_error`, `parse_retry_after`, `backoff_seconds`). Neither the benchmarks nor the fuzz targets run under the coverage command; the code each wrapper forwards to is covered by the unit tests. |

### `src/client.rs`

| Lines | Why |
| --- | --- |
| 94-96 | `Client::from_env`, one line calling `builder().build()`. Reading the process environment is tested through the injected lookup `Config::resolve` takes; a test of `from_env` itself would have to set a variable, which `std::env::set_var` makes `unsafe` in edition 2024 and the crate forbids. The live tests call the same `build()`. |

### `src/codec.rs`

| Lines | Why |
| --- | --- |
| 468 | `Detail::Opaque`: the path-tracking pass accepted what the first pass refused. Both passes read the same text with the same type, so it is a guard against the two disagreeing, not a path any input is known to take. |
| 529 | `Segment::Unknown`: a map key `serde_path_to_error` could not capture. No response type here has such a key. |
| 689 | A transcode failure the parser raised rather than the target serializer. The text of a `RawJson` is one complete JSON value by construction, so the parser cannot fail on it. |
| 719 | A `RawJson` asked to write itself twice through one splice; the SDK's encoder asks once. Defensive. |
| 736-738, 926-928, 1066-1068, 1214-1216 | `Visitor::expecting`, which serde calls only to word a type-mismatch message. These visitors accept every JSON value, so no mismatch is reported through them. |
| 748-750, 756-758, 780-781, 783-785, 787-788 | Transcoder arms for `i128`, `u128`, `Option` and newtype values. The transcode reads the value with the codec's `deserialize_any`, which never produces these (sonic-rs 0.5.10 `serde/de.rs`); the arms exist because a `Visitor` must answer every shape. |
| 772-774, 776-778 | Transcoder `null` (`visit_unit`, `visit_none`). **Cheap to cover**: a `RawJson` holding `null` written through serde_json (`src/codec_tests.rs`). |
| 958-960, 962-964, 966-968, 974-976, 982-984, 986-988, 990-992, 994-995 | `RawJson` read by a foreign deserializer that answers the raw-text request with a bare scalar instead of itself. The tests use serde_json, which hands itself over (the `visit_newtype_struct` path, covered); the `u64` and `f64` arms are reached by the existing tests. **Cheap to cover**: `bool`, `i64`, `null` the same way. `i128`/`u128` need a deserializer that produces them. |
| 1075-1077, 1079-1081, 1087-1089, 1091-1093, 1105-1108, 1110-1112, 1114-1115, 1117-1119, 1121-1122 | `Render`, the renderer for a `RawJson` read by a foreign deserializer: negative integers, `i128`/`u128`, floats, `null`, `Option` and newtype values. serde_json produces no `i128`/`u128`/`Option`/newtype through `deserialize_any`. **Cheap to cover**: a negative integer, a float and `null` inside a `RawJson` read by serde_json. |

### `src/de.rs`

| Lines | Why |
| --- | --- |
| 893, 906 | Level-keyed probabilities on a choice, or option-keyed ones on a score. Probabilities are read in the shape the answer's `type` already chose, or held raw until it is known, so the two mixed states cannot arise; the arms keep the `match` exhaustive. |

### `src/models.rs`

| Lines | Why |
| --- | --- |
| 56-58 | `Debug` for the `Models` resource handle. **Cheap to cover**: `format!("{:?}", client.models())` (`src/models_tests.rs`). |

### `src/name.rs`

| Lines | Why |
| --- | --- |
| 101-103 | `visit_string`, an owned `String` from a deserializer. The codec hands names over borrowed or as `&str`, never as an owned `String`; only a foreign deserializer reaches it. |

### `src/retry.rs`

| Lines | Why |
| --- | --- |
| 321 | A response-validation error whose headers name a delay, retried because a caller's predicate asked for it. **Cheap to cover** with a predicate test (`src/retry_tests.rs`). |

### `src/telemetry.rs`

| Lines | Why |
| --- | --- |
| 234-237 | The labels a failed-call event gives an API, response-validation, invalid-request or configuration error. The event tests assert the transport failures only. **Cheap to cover** (`src/telemetry_tests.rs`). |
| 263 | The elapsed time of an event with no start instant (`-`). Every event the tests record has one. |
| 325 | A logged text holding bytes that are not UTF-8, written as U+FFFD. **Cheap to cover** (`src/telemetry_tests.rs`). |

### `src/transport/hyper.rs`

| Lines | Why |
| --- | --- |
| 179-181 | `Debug` for the default transport's response future. **Cheap to cover**. |
| 243-245 | A connect timeout mapped to `ErrorKind::Timeout`. A loopback connection is refused or accepted at once, never left pending, so no test reaches the connect timeout; the detection it depends on is unit-tested. README's "What is not measured" lists it. |

# E02 consuming RGB normalization

Date: 2026-10-16. Status: implemented and verified locally; targeted/full native tests and bounded allocation probe passed. Scope: remove conversion copies and shorten decoded-image lifetime without changing pixels, image limits or admission topology. Full final continuation gates are recorded in the session handoff.

## Implementation and evidence

The [E02 memory audit](e02-memory-audit.md) identified a decoded image, RGBA conversion copy and new RGB buffer held together. `normalize_rgb_image` now returns the owned RGB8 buffer directly, consumes an existing RGBA8 buffer rather than cloning it, and drops the original decoded image after thumbnailing. Other color types still use the existing image-library conversion into RGBA8; the exact integer white-background alpha formula is unchanged.

Header dimensions, decode-allocation limits, EXIF application, resize predicate/algorithm, pixel limits, JPEG quality and metadata stripping are unchanged. Parser decoding remains inside its existing blocking closure with its owned permit. No runtime concurrency, task queue, thread, model, checkpoint, schema, dependency or live configuration is changed.

The pinned [image 0.25.10 source](https://github.com/image-rs/image/blob/v0.25.10/src/images/dynimage.rs) confirms that `into_rgba8` moves RGBA8 directly and otherwise delegates to the existing conversion. Decoded images are tagged sRGB by that version; returning decoded RGB therefore preserves the old output color metadata. With implicit alpha 255 the old white composite was an exact identity for RGB8. Tests also compare color-space metadata rather than assuming only raw pixels matter.

Pre-edit upstream impact for `normalize_rgb_image`, including tests, reported **CRITICAL**: seven direct callers, eleven affected symbols and three summarized process roots. This warning was reported before editing. Actual parser decode, legacy normalization and PDF/image callers were reviewed. New test/reference symbols were initially UNKNOWN because the index predated them; their test-only scope was inspected manually. Raw impact reports and logs are private under ignored `artifacts/e02-rgb-memory-2026-10-16/`.

Independent review found no semantic regression under the pinned decoder behavior. The review also considered moving model admission before crop preparation. That would serialize useful CPU/model overlap and checkpoint hits behind model requests. Current W×R prepared-JPEG retention is finite existing prefetch behavior, not a newly proven correctness defect; its memory envelope remains gated. No new semaphore, queue or unmeasured capacity setting was introduced.

## Targeted verification

| Check | Observed result |
|---|---|
| Preparation tests | 14 passed |
| Worker library | 99 passed; one disposable Redis test ignored |
| Inference-contract target | 20 passed; one Poppler test ignored by the ordinary command |
| Layout-pipeline target | 14 passed |
| Strict worker/all-targets Clippy | Passed |
| Final ordinary workspace suite | 309 passed; 90 ignored |
| Explicit serial suite on disposable services | 90 passed; zero ignored |
| Strict workspace/all-targets Clippy | Passed |
| Full Python script suite | 42 passed, including the separate boundary harness controls |
| Source comment guards | 130/105 files; zero violations |

These targeted runs overlap and are not additive test counts. The two new tests compare actual normalization with the frozen prior conversion algorithm, not with the current normalizer itself. Eleven encoded fixtures cover RGB/RGBA 8-bit PNG, JPEG and WebP plus grayscale, grayscale-alpha and RGB/RGBA 16-bit PNG. Each runs with and without thumbnailing, giving 22 color/resize cases. Mixed alpha includes transparent, almost transparent, midpoint and opaque values. Eight JPEG EXIF orientations each run with and without thumbnailing, giving 16 further cases. Every case checks exact RGB pixels/dimensions and encoded JPEG bytes; color cases also check output color metadata. Existing input bounds, orientation, alpha, PDF/parser, cancellation and raw-output regressions remain required.

## Bounded allocation probe

The ignored `conversion-allocation-probe.rs` reproduces the old/new conversion tails and source-drop behavior. It clones the same prebuilt input inside both measured intervals so allocation and deallocation accounting is balanced. It performs six serial iterations for RGB8/RGBA8 at 64×48 and 512×384, both unchanged and half-edge resized. Exact output bytes, dimensions and color metadata match; all measured allocations are released after output cleanup.

| 512×384 source | Thumbnail | Original peak requested bytes | Consuming peak requested bytes | Retained output bytes, both |
|---|---|---:|---:|---:|
| RGB8 | No | 1,966,080 | 589,824 | 589,824 |
| RGBA8 | No | 2,162,688 | 1,376,256 | 589,824 |
| RGB8 | 256×192 | 1,081,344 | 737,280 | 147,456 |
| RGBA8 | 256×192 | 1,327,104 | 983,040 | 147,456 |

Without resizing, measured conversion peaks scale from 10 to 3 bytes per RGB source pixel and 11 to 7 bytes per RGBA source pixel, including the balanced input clone. The returned RGB buffer remains three bytes per output pixel. Smaller fixtures have the same relationships. These are requested/live allocation bytes for reproduced conversion primitives; decoder buffers, EXIF work, compressed input, JPEG output, allocator overhead, RSS, cgroups, IPC and model calls are outside the measured interval. No real workload or maximum-size fixture was run, and no throughput or whole-parser memory gain is claimed.

## Remaining work and rollback

Fresh representative worker/parser/cgroup measurements, allocator A/B, real-model quality/capacity and the [production execution plan](production-execution-plan.md) gates remain open. Conversion savings do not authorize raising W/R or memory limits on the pressured host. Existing raw evidence, checkpoint identities and crop bytes must remain intact through further optimizations.

The explicit suite used only the preexisting disposable verify PostgreSQL/Redis cluster. Read-only preflight/postflight identity, privilege and dependency checks passed, and the OOM Redis returned to its original unlimited/noeviction configuration. Its 90 tests include 88 database/Redis fixtures plus one RSS diagnostic and one Poppler pipeline. The separate [boundary matrix](a06b-boundary-execution-trace.md) used historical images rather than this new normalization binary; its success cannot replace current-source Linux packaging verification.

Rollback is a source/image revert with no storage migration; it restores prior temporary allocations. Current-source packaged Linux verification remains distinct from native tests or historical parser-image boundary tests.

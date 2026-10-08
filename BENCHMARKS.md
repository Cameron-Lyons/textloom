# Performance measurements

Measured locally on 2026-10-07 with Rust 1.99.0, Cargo's default release profile,
Linux x86_64, and an Intel Core Ultra 5 325. These are elapsed-time averages from
the standalone harnesses, not statistical confidence intervals. CPU frequency,
background work, fonts, accessibility activation, and document contents affect
results. CI runs these harnesses as smoke tests without timing thresholds.

```sh
cargo bench --locked --bench editing
cargo bench --locked --features egui --bench rendering
```

## egui frames

The same `benches/rendering.rs` harness was run against baseline commit
`2ac6269dacf515648fb8cc66f2d315b94d193115` and the updated implementation. For
the baseline, copy this new harness into that checkout and declare its
`harness = false` / `required-features = ["egui"]` Cargo benchmark target.

The workload uses unique short Unicode paragraphs, an 800×600 headless egui
viewport inside a vertical `ScrollArea`, and three warmup frames. Accessibility
is inactive. The 100-paragraph case averages 1,000 idle frames; the 10,000 case
averages 100. Each edit iteration inserts one character in the middle paragraph,
renders, undoes, and renders again; the reported value divides the whole
iteration by two. Painting/renderer submission to a native GPU is not measured.

| Paragraphs | Operation | Before (µs) | After (µs) |
| ---: | --- | ---: | ---: |
| 100 | Idle frame | 23.78 | 9.34 |
| 100 | Local edit + frame | 41.02 | 25.38 |
| 10,000 | Idle frame | 1,717.50 | 8.29 |
| 10,000 | Local edit + frame | 2,584.83 | 228.26 |

The 10,000-paragraph sample is about 207× faster while idle and 11× faster for
local edits. Repeated runs during development ranged around 7–10 µs idle and
160–241 µs per edit/frame. The widget retains paragraph galleys and geometry,
checks an immutable content token, detects egui font-cache resets through a
small cached galley, and paints only visible paragraphs. An edit still checks
paragraph identities and rebuilds geometry across the document; initial layout
and font/DPI/appearance changes still lay out every paragraph.

## Unicode navigation

The fixed phrase `café 👩‍💻 नमस्ते 🇺🇸 ` repeats 10,000 times: 460,000 UTF-8
bytes. The caret starts 90% through the paragraph. Vertical movement uses two
identical paragraphs. The baseline scanned Unicode text on every operation.
The updated harness reports first-use index construction separately from
repeated queries (2,000 validation/word iterations and 1,000 vertical pairs).

| Operation | Before average (µs) | After cold (µs) | After warm (µs) |
| --- | ---: | ---: | ---: |
| Validate caret | 2,645.18 | 3,404.36 | 0.02 |
| Word backward + forward | 10,382.12 | 3,733.81 | 0.15 |
| Paragraph down + up | 5,573.70 | 3,548.68 | 0.01 |

Word timing warms grapheme validation before measuring the first word query;
vertical timing warms the starting paragraph first. First-use caches allocate
and scan the required text, so a single cold validation can cost more than the
old scan. Warm queries use indexed boundaries. Grapheme, scalar, and word
indexes are independently lazy and shared through formatting/history snapshots.
ASCII grapheme/scalar offsets use no indexes; ASCII word queries still use
Unicode segmentation dynamically. Shared index memory is outside the retained
text/formatting estimate returned by `history_bytes()`.

## Editing and replacement

The unchanged local-edit workload averages 2,000 one-character insert/undo
pairs in short Unicode paragraphs. The updated all-feature release run measured
1.03 / 1.08 / 1.09 µs per pair for 100 / 10,000 / 100,000 paragraphs. This
confirms that localized editing does not copy the entire document.

The new batch workload replaces 10,000 occurrences of `match` with
`replacement`, then undoes the operation. After one warmup iteration, 50
iterations averaged 446.91 µs per replace-all/undo pair. Batch replacement
builds each affected paragraph once and produces one history delta, rather
than repeatedly rebuilding a paragraph or shifting later match coordinates.
Explicit search temporarily allocates a flattened text copy.

# Performance measurements

Measured locally on October 7–9, 2026 with Rust 1.99.0, Cargo's default release
profile, Linux x86_64, and an Intel Core Ultra 5 325. These are elapsed-time averages from
the standalone harnesses, not statistical confidence intervals. CPU frequency,
background work, fonts, accessibility activation, and document contents affect
results. The rendering table reports the median of three runs; each run averages
repeated operations. CI runs these harnesses as smoke tests without timing thresholds.

```sh
cargo bench --locked --bench editing
cargo bench --locked --features egui --bench rendering
```

## egui frames

The same `benches/rendering.rs` harness was run against baseline commit
`2ac6269dacf515648fb8cc66f2d315b94d193115` and updated source commit
`ff211806c4400ba26b090b1d7e50c343c739cd4d`. For
the baseline, copy this new harness into that checkout and declare its
`harness = false` / `required-features = ["egui"]` Cargo benchmark target.

Both sources were compiled in separate Cargo target directories, then run
sequentially with the same toolchain, release profile, lockfile dependencies,
features, and harness. Use distinct target directories for each checkout to
prevent Cargo from reusing identically named artifacts across revisions:

```sh
CARGO_TARGET_DIR=/tmp/textloom-bench-baseline cargo bench --manifest-path /path/to/baseline/Cargo.toml --locked --features egui --bench rendering
CARGO_TARGET_DIR=/tmp/textloom-bench-current cargo bench --manifest-path /path/to/current/Cargo.toml --locked --features egui --bench rendering
```

The workload uses 100 or 10,000 unique populated Unicode paragraphs plus one
trailing empty paragraph, an 800×600 headless egui viewport inside a vertical
`ScrollArea`, and three warmup frames. Accessibility is inactive and the widget
is unfocused. The 100-paragraph case averages 1,000 idle frames; the 10,000 case
averages 100. Each edit iteration inserts one character in the middle paragraph,
renders, undoes, and renders again; the reported value divides the whole
iteration by two. Shape tessellation and submission to a native GPU are not measured.

| Populated paragraphs | Operation | Before (µs) | After (µs) |
| ---: | --- | ---: | ---: |
| 100 | Idle frame | 26.32 | 9.39 |
| 100 | Local edit + frame | 45.81 | 26.96 |
| 10,000 | Idle frame | 1,909.14 | 9.40 |
| 10,000 | Local edit + frame | 2,882.31 | 244.33 |

The 10,000-paragraph sample is about 203× faster while idle and 12× faster for
local edits. The three isolated runs ranged from 9.05–10.19 µs idle and
239.71–247.31 µs per edit/frame. The widget retains paragraph galleys and geometry,
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
ASCII grapheme/scalar offsets use no indexes. The source measured here still
scanned ASCII words on each query; the October 8 changes below cache those queries
too. Shared index memory is outside the retained
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
This measurement used a flattened text copy for explicit search; single-paragraph
searches now borrow text, as measured below.

## October 8 cleanup measurements

The expanded `benches/editing.rs` harness was run against a snapshot of the
working tree before this cleanup and the updated source. Both used Rust 1.99.0,
the same locked dependencies, default features, and separate target directories.
The table reports the median of three sequential runs for each version; values
are elapsed-time averages in microseconds per operation.

| Operation | Before (µs) | After (µs) |
| --- | ---: | ---: |
| ASCII word backward + forward, cold | 654.12 | 791.77 |
| ASCII word backward + forward, warm | 676.57 | 0.09 |
| 1,000 long literal matches | 2,396.17 | 326.87 |
| 10,000 replacements + undo | 394.10 | 327.41 |

The ASCII workload repeats `A short paragraph with ASCII words. ` 10,000
times (360,000 bytes), starts the caret 90% through the paragraph, and averages
2,000 warm word-movement pairs. First use now builds a lazy word index; subsequent
queries use binary searches. Cold navigation includes index construction and
can cost more than one scan. ASCII grapheme/scalar navigation remains index free.

The literal workload searches a 1,088-byte query repeated 1,000 times in a
single paragraph and averages 50 searches. Borrowing the paragraph avoids a text
copy, and reusing one literal searcher avoids rebuilding its query preprocessing
for every accepted match. Rejected grapheme or word boundaries still retry
overlapping candidates. These timings describe the stated workloads, not every
search pattern or document size.

## Accessibility navigation

The AccessKit workload in `benches/editing.rs` is enabled with
`cargo bench --locked --features accesskit --bench editing`. It builds a tree
for 100 or 10,000 populated `aé👩‍💻` paragraphs and a trailing empty paragraph.
After initial tree construction, it measures 2,000 position encode/decode
roundtrips near the document end and 200 pairs of forward/backward caret moves
and tree updates. Both sources used the same harness, locked dependencies, and
release profile on October 8; values are medians of three sequential runs.

| Populated paragraphs | Operation | Before (µs) | After (µs) |
| ---: | --- | ---: | ---: |
| 100 | Position roundtrip | 0.12 | 0.05 |
| 10,000 | Position roundtrip | 34.74 | 0.04 |
| 100 | Paired selection updates | 16.72 | 0.32 |
| 10,000 | Paired selection updates | 2,110.16 | 6.83 |

Content validation compares a shared identity token instead of scanning all
paragraphs. Run-ID lookups use a cached index, rebuilt after content edits.
Selection/label updates reuse paragraph nodes and their run index. The editor
node's child list still needs to be cloned for selection updates, so that work
continues to scale with paragraph count. Initial trees and content edits still
visit every paragraph; this benchmark does not measure platform screen readers.

The editing harness also checks ordinary literal queries on 100,000 short
Unicode paragraphs, including matches, misses, and queries longer than every
paragraph. Those searches now borrow paragraph text and skip paragraphs shorter
than the query. At that point, cross-paragraph, case-insensitive, and whole-word
queries used the flattened-text path; the next section measures the updated
paragraph-local search.

## October 9 first-pass release-candidate search

The expanded `benches/editing.rs` harness was copied into the October 8 baseline
commit `1ee61fe` and run against that source and the updated 1.0 working tree.
Both used Rust 1.99.0, the same lockfile, default features, Cargo's default release
profile, and separate target directories on the same Linux machine. The table
reports medians of three sequential runs of each version after the automated
checks completed. Values are elapsed-time averages in microseconds per operation.
These measurements precede the selection-near navigation update described below.

| Operation | Before (µs) | After (µs) |
| --- | ---: | ---: |
| 100,000 paragraph literal matches | 9,772.00 | 9,758.32 |
| 100,000 paragraph lowercase whole-word matches | 75,432.10 | 65,228.91 |
| 100,000 short paragraphs, long lowercase whole-word query miss | 61,856.12 | 5,879.30 |
| Find next from document start, 100,000 paragraphs | 45,248.51 | 0.38 |
| Find previous near document start, 100,000 paragraphs | 45,130.72 | 0.61 |
| 10,000 replacements + undo | 276.93 | 290.96 |

The navigation workload repeats `MATCH match café\n` 100,000 times plus a
trailing empty paragraph, with case-insensitive whole-word search for `match`.
Each of 100 iterations resets the caret to document start for find-next or the
start of paragraph 1 for find-previous. Navigation retains only its candidate
ranges and stops when the answer is known. These results measure finding matches
near the beginning. At this first-pass stage, navigation still visited matches
from document start; backward wrapping, missing queries, and queries with
paragraph breaks could scan the entire document.

The full-search workloads repeat `A short paragraph with Unicode café 👩‍💻.\n`
100,000 times. The literal query is `Unicode`, the lowercase whole-word query is
`unicode`, and the long miss uses the 1,088-byte repeated query from the literal
search benchmark. Full matches/misses average 20 operations, except lowercase
whole-word matches, which average 10. Queries without paragraph breaks now
prepare lowercase text and word boundaries independently for each paragraph,
avoiding a full-document temporary copy. Needles longer than the lowercase
paragraph are skipped before word indexing or literal-search preprocessing;
length comparison follows lowercase expansion so `İ` still matches `i\u{307}`.

Batched replacement averages 50 replace-all/undo operations after a warmup.
Its full-harness median increased by 5.1% in that first-pass comparison. The
improvements target find navigation and paragraph-local search; they do not
imply faster timings for every operation. Initial Unicode indexes, query size,
caret position, and background work still affect results.

## October 9 continuation: search near the selection

The expanded navigation workload was compared against the first-pass 1.0
snapshot and the continuation source. Each source was built in an independent
Cargo target directory; distinct binary hashes were verified before running.
Both used Rust 1.99.0, locked dependencies, default features, and the default
release profile on the same Linux machine. The `search_navigation()` workload
was isolated from `benches/editing.rs`, with each timed loop capped at 20
iterations for both sources. The table reports the median of three sequential
runs per source, in microseconds per operation.

| Operation, 100,000 paragraphs | First pass (µs) | Continuation (µs) |
| --- | ---: | ---: |
| Find next near middle | 15,292.91 | 0.55 |
| Find previous near middle | 14,140.65 | 0.47 |
| Find next near end | 29,222.06 | 0.50 |
| Find previous near end | 28,284.63 | 0.45 |
| Find next wrapped from document end | 28,329.03 | 0.44 |
| Find previous wrapped from document start | 28,381.12 | 0.47 |

The text and search options are the same `MATCH match café\n` workload used
above. Middle queries start in paragraph 50,000; end queries start in paragraph
99,999. Find-next starts just after `MATCH` and selects `match`; find-previous
starts at `match` and selects `MATCH`. The wrap cases start at the document's
final caret or initial caret and select the first or last accepted match.

These repeated-paragraph queries improved from roughly 14–29 ms to about 1 µs.
The small after-values are sensitive to timer resolution and background work;
their last digits do not indicate a precision guarantee. Navigation now starts
at the selection boundary paragraph and traverses paragraph indexes in the
requested direction. Wrapping searches a bounded second interval, so wrapping
backward from the beginning can find a match directly in the last populated
paragraph. Within each visited paragraph, matching still proceeds forward from
its beginning, preserving nonoverlapping matches and complete lowercase context.

The results describe nearby matches in short paragraphs. Missing queries can
still visit every paragraph, large visited paragraphs still require text scans,
and queries containing paragraph breaks retain the flattened document-wide
match stream. The optimization does not make every search constant time.

## October 9 continuation: whole-word searches with no matches

Two copies of the same source snapshot were compared, differing only in
`src/search.rs`. The baseline used the search implementation at commit
`15bdda0182b6bf01877c39be1acef6c0536306ac`; the updated source defers word-boundary
indexes and lowercase coordinate maps until a literal candidate appears.
Both were built with Rust 1.99.0, locked dependencies, default features,
Cargo's default release profile, and separate Cargo target directories on the
same machine. Distinct benchmark binary hashes were verified before timing.

The four full-search workloads from `benches/editing.rs` were isolated in an
otherwise identical standalone harness. Each source ran three times, alternating
baseline and updated binaries. The table reports the median of those runs, in
microseconds per search. Each run averages 20 missing searches and 10 matching
searches without an explicit warmup. The document repeats
`A short paragraph with Unicode café 👩‍💻.\n` 100,000 times. Missing queries use
`absent`; matching queries use `Unicode` with case-sensitive whole-word search
or its lowercase equivalent with case-insensitive whole-word search.

| Operation, 100,000 paragraphs | Before (µs) | After (µs) |
| --- | ---: | ---: |
| Whole-word query, no matches | 48,780.87 | 3,372.72 |
| Lowercase whole-word query, no matches | 59,961.75 | 7,737.62 |
| Whole-word query, 100,000 matches | 59,849.26 | 59,858.88 |
| Lowercase whole-word query, 100,000 matches | 64,018.01 | 64,447.86 |

The missing queries improved by about 14× and 8× on this workload. Matching
queries retained similar timings because their Unicode indexes are still
needed. Every paragraph is still visited, case-insensitive searches still build
lowercase text, and paragraphs with rejected literal candidates can still need
word indexing. This change avoids preprocessing paragraphs without candidates;
it does not cache whole-word results across searches.

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

## October 9 continuation: IME previews and import ownership

The expanded editing and rendering harnesses were copied into baseline commit
`b348a4552e2d4b22fdcd55063fe4a105e79a80cb` and compared with the cleanup source.
Both used Rust 1.99.0, locked dependencies, the `egui` feature, Cargo's default
release profile, and separate target directories on the same Linux machine.
Distinct binary hashes were verified before timing. The table reports medians
of three alternating baseline/current runs of each complete harness, in
microseconds per operation.

| Workload | Before (µs) | After (µs) |
| --- | ---: | ---: |
| 100 paragraphs, preedit update + frame | 47.08 | 31.05 |
| 10,000 paragraphs, preedit update + frame | 3,299.91 | 331.50 |
| 100,000 paragraphs, plain-text fragment import | 16,940.19 | 14,573.24 |
| 100,000 paragraphs, native document snapshot import | 26,147.35 | 23,311.19 |

Preedit uses the same unique Unicode paragraphs and 800×600 scrolling viewport
as the idle/edit workload above. After those measurements, the widget receives
focus, starts composition at the middle paragraph, and warms three frames.
Timed updates alternate `preedit x` and `preedit`, with the preedit selection
covering the complete text. The 100-paragraph case averages 1,000 updates; the
10,000-paragraph case averages 100. Assertions verify composition remains active
and the committed document remains unchanged. This measures warm repeated
preedit frames; it excludes native IME interaction and GPU submission.

The 10,000-paragraph preedit sample is about 10× faster. The preview now retains
its paragraph layout cache across text updates, avoiding repeated layout jobs
for unchanged paragraphs. Preview construction still clones the document's
paragraph references, and cache updates still visit document geometry.

Import repeats `A short paragraph with Unicode café 👩‍💻.\n` 100,000 times
plus a trailing empty paragraph. Snapshot bytes are encoded before timing.
Each case averages 20 complete import/destruction operations. Imports move the
paragraph vector from their temporary document or fragment, removing a vector
allocation and paragraph reference-count updates. Span normalization also
reuses owned vectors for ASCII or uniform Unicode text. Existing idle/edit
timings remained in a similar range; these measurements do not establish a
latency improvement for every operation.

## October 9 continuation: cursor-only previews and accessibility edits

A snapshot containing the preceding IME-preview/import cleanup was compared
with the cursor and accessibility continuation. The expanded harnesses were
identical in both sources and built in separate target directories with Rust
1.99.0, locked dependencies, the `egui,accesskit` features, and Cargo's default
release profile on the same Linux machine. Distinct binary hashes were checked
before three alternating baseline/current runs of each complete harness. The
table reports medians in microseconds per operation.

| Workload | Before (µs) | After (µs) |
| --- | ---: | ---: |
| 100 paragraphs, preedit cursor + frame | 13.01 | 7.57 |
| 10,000 paragraphs, preedit cursor + frame | 292.03 | 7.70 |
| 100 paragraphs, accessible local edit + frame | 351.47 | 152.13 |
| 10,000 paragraphs, accessible local edit + frame | 60,715.67 | 19,726.24 |
| 100 paragraphs, direct AccessKit edit/update pair | 22.84 | 2.39 |
| 10,000 paragraphs, direct AccessKit edit/update pair | 2,456.45 | 58.62 |

Cursor movement follows the focused preedit workload above, fixes text at
`preedit`, warms three frames, then alternates native selections `0..3` and
`0..7`. Iteration counts remain 1,000 for 100 paragraphs and 100 for 10,000.
Assertions verify active composition and unchanged committed content. Cursor
changes now update preview coordinates without cloning the document or
rebuilding its text and layout; the 10,000-paragraph sample is about 38× faster.

Accessible frames use a fresh egui context with AccessKit enabled, the same
unique Unicode paragraphs and scrolling viewport, three warmup frames, and an
unfocused widget. Each iteration inserts one character into the middle
paragraph, renders, undoes, and renders again; timing is divided by two.
Rendering asserts that editor and accessibility errors remain empty. Snapshots
share unchanged paragraphs' text runs and local geometry across edits, reducing
the 10,000-paragraph sample by about 3×. Each frame still publishes all paragraph
and run nodes, and content changes still rebuild the run-location index.

Direct AccessKit uses the existing `aé👩‍💻` paragraph workload. Each of 200
iterations inserts a character near the document end, updates the tree, undoes,
and updates again; its reported time covers the complete pair. Equal-count
updates affecting at most one paragraph now retain paragraph storage and change
only the affected run-index entries. This sample improved by about 42× at
10,000 paragraphs. Content updates still visit paragraph references and rebuild
the editor's child list; larger or structural edits retain occurrence-aware
matching. These headless measurements exclude native assistive technology and
GPU submission.

## October 9 continuation: literal navigation, sparse replacement, and decoding

A snapshot containing the preceding cursor-only preview and accessibility
cleanup was compared with this continuation. Identical expanded harnesses were
built in separate target directories using Rust 1.99.0, locked dependencies,
the `egui,accesskit` features, and Cargo's default release profile on the same
Linux machine. Distinct binary hashes were verified before three alternating
baseline/current runs of each complete harness. Values below are medians in
microseconds per operation.

| Workload | Before (µs) | After (µs) |
| --- | ---: | ---: |
| 100,000 paragraphs, native document snapshot import | 21,641.39 | 18,530.15 |
| One long Unicode paragraph, native snapshot import | 2,614.24 | 1,317.86 |
| Long Unicode paragraph, nearby literal find-next | 2,402.24 | 0.23 |
| Long Unicode paragraph, nearby literal find-previous | 2,397.14 | 0.22 |
| Sparse replace-all + undo, 10,000 untouched 29-byte paragraphs | 773.47 | 563.89 |
| Sparse replace-all + undo, 10,000 untouched 1,160-byte paragraphs | 2,738.20 | 2,081.20 |
| 100 paragraphs, accessible local edit + frame | 163.35 | 139.50 |
| 10,000 paragraphs, accessible local edit + frame | 22,330.94 | 19,083.58 |

The long snapshot contains `café 👩‍💻. ` repeated 100,000 times: 1,900,000
UTF-8 bytes in one uniformly styled paragraph. Bytes are encoded before timing;
50 iterations include decoding and destruction. The many-paragraph case keeps
the preceding workload and 20 iterations. Import validates borrowed text before
retaining it and merges validated spans in place. CR/LF rejection checks bytes
instead of decoding every Unicode scalar. UTF-8 checks, canonical span rules,
error precedence, and lazy navigation indexes remain intact. The long sample
uses about half the previous time; the many-paragraph sample uses about 14% less.

Literal navigation repeats `café match 👩‍💻 ` 50,000 times, making a
1,200,000-byte paragraph. The caret starts near the match at repetition 45,000;
each of 100 iterations resets it and finds the next or previous match. Full
paragraph grapheme validation is warmed before timing. Case-sensitive queries
without whole-word matching can scan directly from the selection when their
first Unicode scalar occurs only once in the query, which rules out overlapping
matches. Other queries retain the original forward scanner, and all candidates
are validated against the original paragraph's grapheme boundaries. The tiny
warm averages include selection reset and are sensitive to timer resolution;
these results exclude cold index construction and do not describe missing,
whole-word, case-insensitive, overlapping, or cross-paragraph queries.

Sparse replacement places `match` in the first and last paragraphs, surrounding
10,000 unchanged paragraphs containing `untouched café 👩‍💻. ` repeated once
or 40 times. After one replace/undo warmup, each sample averages 50 replacements
of both matches with `replacement`, followed by undo. The history byte limit is
64 MiB so the larger contiguous undo delta remains available. Complete untouched
paragraphs stay shared until a text join requires materializing them; joins still
normalize grapheme boundaries and formatting. These samples use 24–27% less
time. Searching, paragraph vectors, and contiguous history deltas still scale
with the intervening document. Dense replacement did not improve in this run:
10,000 replacements plus undo measured 297.16 µs before and 310.50 µs after.

Accessible frames retain the preceding workload and iteration counts. Text runs
now register hover widgets directly within their paragraph UI, avoiding a
separate UI allocation per run while preserving hierarchy, geometry, enabled
state, and focus behavior. Both samples use about 15% less time. Every frame
still publishes all nodes; native assistive technology and GPU submission are
excluded. Ordinary idle, edit, and preedit frames remained in a similar range.

## October 9 continuation: word-index prefixes and dense replacement

A snapshot containing the preceding literal-navigation, sparse-replacement,
decoding, and hover-widget cleanup was compared with this continuation. Both
sources used identical expanded harnesses, Rust 1.99.0, locked dependencies,
the `egui,accesskit` features, Cargo's default release profile, and separate
target directories on the same Linux machine. Distinct binary hashes were
verified before three alternating baseline/current runs of each complete
harness. The table reports medians in microseconds per operation.

| Workload | Before (µs) | After (µs) |
| --- | ---: | ---: |
| Long Unicode paragraph, sparse whole-word find | 7,637.80 | 97.54 |
| Long Unicode paragraph, whole-word find-next from start | 7,530.34 | 0.23 |
| 100,000 paragraphs, whole-word find with matches | 47,239.33 | 37,380.18 |
| 100,000 paragraphs, lowercase whole-word find with matches | 57,604.07 | 48,152.90 |
| 10,000 ASCII matches, replace-all + undo | 290.54 | 229.50 |
| 10,000 styled Unicode matches, replace-all + undo | 6,960.98 | 6,627.06 |
| 100 paragraphs, accessible local edit + frame | 127.19 | 117.94 |
| 10,000 paragraphs, accessible local edit + frame | 17,216.56 | 15,766.88 |

Sparse whole-word search uses one 1,250,006-byte paragraph: `match ` followed by
`other café 👩‍💻. ` repeated 50,000 times. The case-sensitive whole-word query
is `match`, whose only result is `0..5`. Grapheme validation is warmed before
timing. Full find averages 50 searches; find-next averages 100 searches, each
resetting selection to the start. Word-boundary indexes now extend through the
prefix needed by candidates, retaining earlier boundaries for overlapping
retries. The complete find still scans the remaining text for other literals,
but avoids segmenting and allocating a word index for that suffix; this sample
is about 78× faster. The tiny warm find-next average is sensitive to timer
resolution and excludes initial grapheme indexing. Unicode segmentation can
inspect beyond the candidate to determine a boundary, later candidates still
extend the index, and case-insensitive search still lowercases the complete
visited text.

The existing many-paragraph whole-word workloads keep their short Unicode
paragraphs, matching query, and iteration counts. They use about 21% less time
for case-sensitive matching and 16% less for lowercase matching because each
paragraph's trailing words do not need boundary indexing after its final
candidate. Word-index storage is still local to each search; no results are
cached between queries.

The ASCII replacement workload remains `match ` repeated 10,000 times. The new
styled Unicode case repeats `match café 👩‍💻 ` 10,000 times in one
240,000-byte paragraph, alternating bold and italic runs. Its canonical TLFR
snapshot is assembled and decoded before timing to avoid thousands of setup
edits. Both cases replace every `match` with `replacement`, undo, warm one
pair, then average 50 pairs. Replacement prepares normalized chunks once and
reuses the valid, ordered, nonoverlapping ranges emitted by search. Debug
assertions retain internal validity checks; public range-edit validation is
unchanged. ASCII replacement uses about 21% less time. The styled Unicode
sample uses about 5% less time, with segmentation and paragraph rebuilding
still dominating its cost. Sparse replacement remained in a similar range.

Accessible frames retain the preceding workload and iteration counts. Cached
run nodes own text, character lengths, style properties, and line links.
Publication clones their properties and updates current bounds and scaled
character geometry. Builder-only scalar offsets and row/style fields are
discarded after construction; static node property metadata remains cached per
run. Every frame still publishes all nodes and clones owned strings and arrays.
Both samples use about 7–8% less time; native assistive technology and GPU
submission remain outside these measurements.

The expanded serialization controls measured 100,000-paragraph native export
at 2,751.85 / 2,715.82 µs, HTML export at 2,445.61 / 2,456.89 µs, and a
10,000-span native export at 26.84 / 26.55 µs before/after. Their implementations
were unchanged in the final comparison; these differences do not establish
export gains. Independent TLFR fixtures and HTML tests continue to verify the
persisted bytes and markup.

## October 9 continuation: local Unicode boundaries and native IME ranges

A snapshot containing the preceding changes was compared with the bounded
grapheme checks and egui input cleanup. Both sources used identical expanded
harnesses, Rust 1.99.0, locked dependencies, the `egui,accesskit` features,
Cargo's default release profile, and separate target directories on the same
Intel Core Ultra 5 325 Linux machine. Distinct binary hashes were verified.
Three complete runs of each harness alternated baseline/current, reversing
their order in the second run. The table reports medians in microseconds.

| Workload | Before (µs) | After (µs) |
| --- | ---: | ---: |
| Long Unicode paragraph, middle insert + undo | 3,481.82 | 17.42 |
| Long regional-indicator run, middle insert + undo | 286.74 | 276.75 |
| 10,000 regional flag literal matches | 355.98 | 348.83 |
| 100,000 short Unicode paragraphs, literal matches | 10,264.14 | 4,497.95 |
| 10,000 styled Unicode matches, replace-all + undo | 7,663.22 | 5,341.92 |
| Long native preedit, cursor at start + frame | 44.82 | 37.67 |
| Long native preedit, cursor at end + frame | 205.28 | 93.54 |
| 10,000 paragraphs, accessible local edit + frame | 18,072.73 | 19,094.63 |
| Sparse replace-all + undo, 10,000 unchanged 1,160-byte paragraphs | 2,363.41 | 2,667.84 |

The long editing case uses `café 👩‍💻 नमस्ते 🇺🇸 ` repeated 10,000 times
in one 460,000-byte paragraph. Selection starts at the middle phrase boundary;
each sample averages 200 single-character insertions followed by undo. The
original source and its selection are retained across pairs. Previously, every
rebuilt Unicode paragraph allocated and populated its full grapheme index to
validate the new caret. Validation, style lookup, and snapping now reuse an
existing index or inspect up to 64 UTF-8 bytes on either side of the queried
scalar boundary. Missing context initializes the original full index once,
which subsequent queries reuse. The ordinary long-paragraph editing sample is
about 200× faster; copying paragraph text still scales with its size, and
nonuniform style normalization can still require full segmentation.

The contextual controls use `🇺🇸` repeated 10,000 times in one 80,000-byte
paragraph. Editing inserts `🇨🇦` at the midpoint and undoes it, averaging 100
pairs. Its new caret requires regional-indicator parity beyond the local
window, so each rebuilt paragraph falls back to its full index. All-match
search warms one query, then averages 50 searches with 10,000 results each.
Both controls stayed in a similar range. Tests additionally cover long emoji,
combining-mark, and Indic contexts, cold and indexed snapping at every byte,
joined editing carets, and index reuse. Bounded attempts avoid repeatedly
rescanning the complete prefix of a contextual run.

The existing short-paragraph literal and styled replacement workloads retain
their inputs and iteration counts. Boolean boundary checks avoid eager
indexing for ordinary local contexts. Literal matching uses about 56% less
time; styled Unicode replacement uses about 30% less time in this comparison.
These results describe their measured cache and match patterns.

The native IME case uses `café 👩‍💻 ` repeated 10,000 times: 180,000 UTF-8
bytes and 90,000 scalars. After three warm frames, each sample averages 100
native Preedit events with the same text and a caret at scalar zero or the end.
Event text is cloned inside timing, and frames retain the cached preview.
Scalar-range conversion now walks through the requested endpoint once,
instead of counting the complete text and independently scanning both
endpoints. Start/end frames use about 16% / 54% less time. Preview comparison,
text copying, caret geometry, and rendering remain part of these measurements.
Word-selection snapping also reuses paragraph boundary queries; word
segmentation still visits the prefix through the selected word.

Cold-query timing reflects the change in when indexes are built. The initial
position validation dropped from 3,466.17 to 0.91 µs. The first paragraph
down/up pair, timed after setting selection, increased from 3,455.26 to
6,641.14 µs: selection previously built the source paragraph's full index,
whereas the movement now builds both paragraphs' indexes. Warm down/up pairs
remained at 0.04 µs. Grapheme counting and index conversion continue to require
full indexing.

Other controls show the practical limits of this change. Accessible edit
frames took about 6% longer. The larger sparse-replacement median increased
about 13%, with current samples ranging from 2.29 to 6.28 ms versus baseline
2.28 to 2.40 ms. Import, export, and ordinary idle frames stayed close; the
results establish gains for the workloads above, with no general frame-time
improvement claimed. Full release checks passed, including all eight feature
combinations, optimized search/replacement oracles, native-host tests, Clippy,
docs, and tests/compilation from the extracted Cargo package.

## October 9 continuation: adjacent grapheme movement and structural layouts

The preceding source was compared with shared boundary queries for adjacent
grapheme movement and a smaller structural layout lookup. Both versions used
the same expanded harnesses, Rust 1.99.0, locked dependencies, the
`egui,accesskit` features, and Cargo's default release profile on the Intel
Core Ultra 5 325 Linux machine. Saved baseline and current executables came
from separate build directories, with distinct SHA-256 hashes verified before
measurement. Three complete runs of each harness alternated baseline/current,
reversing the order for the second run. The table reports medians in microseconds.

| Workload | Before (µs) | After (µs) |
| --- | ---: | ---: |
| Long Unicode paragraph, first grapheme backward/forward pair | 3,259.41 | 1.13 |
| Long Unicode paragraph, repeated grapheme backward/forward pair | 0.09 | 0.05 |
| Long Unicode paragraph, insert + backspace + two undos | 3,286.53 | 31.90 |
| Long Unicode paragraph, insert + delete + two undos | 3,278.41 | 32.11 |
| 100 paragraphs, split or undo + frame | 26.73 | 22.51 |
| 10,000 paragraphs, split or undo + frame | 511.84 | 193.54 |
| 10,000 paragraphs, idle frame | 8.32 | 8.45 |
| 10,000 paragraphs, local edit + frame | 203.58 | 192.49 |
| Long regional-indicator run, middle insert + undo | 263.44 | 260.84 |

Navigation uses the existing 460,000-byte paragraph, with the caret 90% through
it. The first backward/forward pair is measured once per run after selection
validation; subsequent pairs average 2,000 operations. The baseline builds the
complete grapheme index on its first move. Adjacent movement now snaps from
one byte before or after the requested offset, reusing the existing bounded
boundary checks. Ordinary local movement does not allocate the full index.
Tiny warm timings and single-query cold timings remain sensitive to timer
resolution. Grapheme counting and index conversions still build full indexes;
long contextual sequences still fall back to the shared index.

The deletion cases start at the paragraph's midpoint. Each averages 200 cycles:
insert `x`, delete the preceding or following grapheme, then undo both edits.
Deleting immediately after insertion previously indexed every newly rebuilt
Unicode paragraph. Both samples now use about 100× less time. Paragraph text
copying and nonuniform span normalization retain their existing costs.

The structural frame cases use the existing unique Unicode paragraphs and
headless scrolling widget. Each sample averages 1,000 split/undo pairs at 100
paragraphs or 100 pairs at 10,000 paragraphs, with one frame after each action.
Unchanged cache entries before and after the affected range remain in the
paragraph vector; only its changed middle needs a lookup table. Font or
appearance invalidation recreates entries directly. The larger sample uses
about 62% less time. Geometry rebuilding still visits the complete document,
and native rendering is outside this measurement.

Import/export, search/replacement, idle rendering, word navigation, and the
regional-indicator controls stayed in similar ranges. Unicode tests compare
cold and indexed navigation at every byte with complete segmentation, including
overflow offsets and long contextual clusters. Structural layout tests compare
cached geometry, text, markers, and offsets with a fresh cache after splits,
undo, rich paste, appearance changes, and full replacement. All automated
release checks passed offline, including the eight feature combinations,
optimized Unicode and search/replacement checks, native-host tests, Clippy,
docs, and extracted-package validation.

## October 9 continuation: shared IME snapshots and export cleanup

The preceding source was compared with internally shared composition snapshots
and consolidated document/fragment plain-text export. Both used identical
expanded harnesses, Rust 1.99.0, locked dependencies, `egui,accesskit`, and the
default release profile on the same Intel Core Ultra 5 325 Linux machine.
Baseline and current sources used separate build directories; saved executable
SHA-256 hashes were distinct. Three complete runs alternated baseline/current,
reversing the order for the second run. The table reports medians in microseconds.

| Workload | Before (µs) | After (µs) |
| --- | ---: | ---: |
| Long preedit, idle frame | 21.32 | 6.63 |
| Long preedit, identical update + frame | 23.06 | 10.69 |
| Long preedit, cursor update + frame | 72.38 | 70.51 |
| Long preedit, native cursor at start + frame | 34.27 | 19.26 |
| Long preedit, native cursor at end + frame | 85.39 | 71.29 |
| Long preedit, changed text + frame | 40,818.64 | 40,082.71 |
| 10,000 paragraphs, idle frame | 7.76 | 7.72 |
| 10,000 paragraphs, local edit + frame | 195.83 | 192.30 |
| 10,000 paragraphs, split or undo + frame | 196.18 | 191.58 |

The long-preedit fixture repeats `café 👩‍💻 ` 10,000 times: 180,000 UTF-8
bytes and 90,000 scalars. After three warm frames, idle, identical-update, and
cursor-update samples average 200 frames each. Cursor updates alternate between
the beginning and end. Text updates average 20 frames, alternating the fixture
and an appended `x`, with the cursor at the start. Each iteration checks the
preedit byte length; full text equality is checked after timing. Existing native
event workloads retain their 100-frame samples, including event text cloning.

The widget now retains an immutable composition snapshot instead of cloning its
string every frame. Its preview shares that snapshot and uses pointer equality
before comparing changed text. Identical updates retain the same snapshot.
Shared updates copy incoming text directly, preserving earlier values; exclusive
updates reuse their text buffer. The public `Composition` type and borrowed
editor accessor retain their existing interfaces. Idle and identical-update
frames use about 69% and 54% less time. Native cursor-at-start/end samples use
about 44% and 17% less time.

Cursor-only updates still compute preview positions through the requested
prefix, and changed text still rebuilds the preview and shapes the changed
paragraph. Those controls stayed close to the baseline. Ordinary edit/split
frames, import/export, and search/replacement also remained in similar ranges;
the export consolidation establishes shared implementation, without an export
speedup claimed. These are headless measurements, excluding native rendering.

Regression checks retain snapshots across valid and invalid updates, cursor
changes, commit, cancel, and undo. They also verify exclusive buffer reuse,
preview snapshot sharing, and unchanged preview document/layout reuse. Automated
release checks cover all feature combinations and the extracted Cargo package.

## October 9 continuation: localized inline formatting

The preceding source was compared with paragraph-local style patching. Both
used the same expanded harnesses, Rust 1.99.0, locked dependencies,
`egui,accesskit`, and the default release profile on the Intel Core Ultra 5 325
Linux machine. The checkout was built before and after the change; saved
executables had distinct SHA-256 hashes. Three complete runs alternated
baseline/current, reversing the order for the second run. Medians are in
microseconds.

| Workload | Before (µs) | After (µs) |
| --- | ---: | ---: |
| 10,000 style runs, local unchanged formatting | 56.70 | 0.09 |
| 10,000 style runs, local formatting + undo | 44.72 | 12.91 |
| 10,000 style runs, full unchanged formatting | 69.18 | 9.70 |
| 10,000 style runs, full formatting + undo | 56.73 | 53.78 |
| 10,000 style runs, clear formatting + undo | 42.85 | 42.42 |
| One style run, local formatting + undo | 0.24 | 0.24 |
| One style run, clear formatting + undo | 0.22 | 0.25 |
| 10,000 paragraphs, idle frame | 8.33 | 8.40 |
| 10,000 paragraphs, local edit + frame | 199.46 | 198.67 |

The larger formatting fixture repeats `match café 👩‍💻 ` 10,000 times,
with alternating bold/italic runs covering 240,000 UTF-8 bytes. Local selection
covers four ASCII graphemes inside its middle run. Unchanged commands request
underline removal, averaging 5,000 local or 500 full-selection operations.
Changed commands add underline and undo, averaging 1,000 local or 100 full
operations. Clear-formatting/undo averages 100 operations. Snapshot equality is
checked outside timing; local undo also verifies selection restoration.

Formatting now locates the selected runs with binary search and checks whether
any style changes before allocating replacement runs. Actual changes patch
the affected runs and copy canonical prefix/suffix runs directly, merging their
boundaries. Paragraph text and navigation indexes remain shared. Small
unchanged selections avoid rebuilding unrelated runs, while local changed
formatting uses about 71% less time in this fixture. Full unchanged formatting
uses about 86% less time; it still visits all selected styles. Changed formatting
still copies the complete run array, and clearing a full paragraph still
processes every run.

Full changed formatting and clear-formatting controls stayed close. The
one-run clear-formatting median rose by 0.03 µs. Most editing, import/export,
search, and large-document frame controls remained close, but the 100-paragraph
idle/local-edit frame medians rose from 7.36/21.03 to 8.21/25.33 µs, and the
long literal-miss median rose from 213.74 to 258.83 µs. These samples establish
the formatting gains above; they do not establish a general search or rendering
speedup. Single-query cold and submicrosecond timings remain sensitive to timer
resolution.

Regression checks compare every valid range in mixed Unicode paragraphs and
empty paragraphs with per-grapheme styles, including color changes and clearing
attributes. They verify canonical run merging, shared text/indexes, unchanged
allocations/content identity, and undo/redo. Editor checks preserve directional
selections and redo through unchanged commands, and update typing style when
only paragraph breaks are selected. Automated release checks cover all feature
combinations and the extracted Cargo package.

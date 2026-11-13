# Corpus: how well the renderer draws real pages

This directory scores the renderer against WebKit on two sets of pages: about 130
public sites picked from a published top-sites ranking, and 349 Web Platform Tests.
The live web changes from one hour to the next, so every site is captured once and
replayed byte for byte afterwards. The same frozen bytes go to WebKit, which supplies the
reference, and to our renderer.

Captures and results are large and stay out of git, under `$CORPUS_DIR`
(default `/Users/gahn/projects/1111-render/.omo/corpus`, inside the ignored `.omo/`).

## Running

Build first (`cargo build --release`); the scripts use `$CARGO_TARGET_DIR/release/render`,
or `target/release/render`, or `--bin`.

```sh
sh tools/corpus/run.sh --jobs 6            # score everything; writes $CORPUS/results/<date>/
sh tools/corpus/run.sh --only naver        # one site (id substring), no change to the others
sh tools/corpus/run.sh --skip-wpt          # sites only
bun tools/corpus/run.ts --compare <dirA> <dirB>   # per-page score differences between two runs
```

A run writes:

- `pages/<id>.json`: one per site with the score, its three parts, the words WebKit shows that
  ours lacks, the boxes that moved, our script errors next to WebKit's, CSS the parser dropped,
  requests that missed the frozen store, and the causes the classifier assigned.
- `png/<id>-{ours,webkit}-{0,1}.png` for every failing site (top screen and one scroll of
  800 px), and `png/wpt/` for failing reftests (our test and reference renders).
- `summary.json`: every number in the report, every page score and every WPT result.
- `report.md`: overall score and pass rate, WPT pass rates by directory, the ranked
  failure causes with example PNGs, and every page sorted by score.

A full run takes a few minutes; `--jobs` sets how many sites (and WPT tests) run at once,
each site with three renderer processes.

## Refreshing the frozen corpus

```sh
bun tools/corpus/select.ts tranco.csv --list-id <id> --count 120   # rewrite sites.json
bun tools/corpus/freeze.ts --jobs 4                                 # capture every site again
bun tools/corpus/wpt.ts fetch && bun tools/corpus/wpt.ts select     # WPT at WPT_COMMIT in wpt.ts
bun tools/corpus/wpt.ts webkit --jobs 4                             # WebKit's own WPT results
```

`sites.json` records the Tranco list id and, for every domain in the window, whether it was
included and why not: infrastructure domains without a page, adult sites, login walls, HTTP
403/451 (blocked or region-restricted), bot challenges, empty interstitials and duplicates.
Korean portals (naver.com, daum.net, namu.wiki) and a few kinds of site a person would
try (MDN, Wikipedia, usa.gov, Hacker News, Craigslist, W3Schools) are always added.

`freeze.ts --only <id>` re-captures one site. Every capture fetches live: be polite and do
not loop it. Results from before a refresh are not comparable with results after it.

## How freezing works

`lib/replay.ts` gives each original host its own loopback port (`127.0.0.1:22000 + 150 *
site + n`). Every URL in HTML, CSS, JS, JSON and SVG that names a host, absolute or
protocol-relative, is rewritten to that host's port (`lib/rewrite.ts`), so pages resolve
relative URLs and build URLs from `location.origin` as they would live.

Recording: WebKit loads the page through the recording server and scrolls 800 px, then our
renderer loads it twice (scroll 0 and 800). A request the store lacks is fetched live once
(at most 6 at a time per host) and stored with its status and headers. The store then
holds what either engine asked for. Sealing fixes the host order: hosts that were requested
first, then hosts only named in text, both sorted. Replay therefore serves identical bytes
on every run.

Replay answers from the store only. A request whose query string differs from every stored
one (a cache buster) gets the stored response for the same path. Anything else is a 404,
logged as a miss. Our renderer runs with `RENDER_LOOPBACK_ONLY=1`, which refuses any
connection off the loopback, so a URL the rewrite missed cannot reach the network.

What both engines see differs from the live site in these ways:

- every page is served from `http://127.0.0.1:<port>`; scripts that test the host name
  behave as they would on a foreign host;
- a script injected at the top of `<head>` fixes `Date` at 2026-11-13 12:00 UTC plus
  `performance.now()`, seeds `Math.random`, reports `en-US`, and collects uncaught errors
  into `window.__corpusErrors`; a style rule zeroes animation and transition durations, so
  pages show their end state;
- `(prefers-color-scheme: dark)` becomes a query that never matches and `color-scheme`
  becomes `light` (the WebKit host runs in dark mode; our renderer always answers light);
- `integrity` attributes and CSP meta tags are removed (the rewritten bytes would fail
  them), as are `Set-Cookie`, CSP, HSTS and frame headers; CORS is allowed everywhere;
- video and audio bodies, and bodies over 12 MB, are not stored.

The WebKit reference (`sites/<id>/ref/`) is taken at freeze time, from the sealed store
alone: the page loads, the run waits until no request has arrived for 1.5 s, then it reads
the words and boxes, takes the top screenshot, scrolls 800 px, and takes the second. Run
scores therefore depend only on our renderer and the frozen bytes.

## The score

Each site is measured on the first two screens at 1280x800, the region `y < 1600`:

- **Text (40%)**: every word WebKit shows in the region (a Range per word of every visible
  text node), matched to a word our renderer laid out (`RENDER_TEXT_RUNS=1` with `--boxes`
  prints each text fragment; words inside a fragment get x positions in proportion to their
  characters). A word gets half credit for being there and half for its place: full
  within 8 px, falling to none at 100 px. Recall is weighted by characters. Precision is the
  share of our words in the region that matched a WebKit word. The part is their F-measure.
- **Layout (30%)**: the boxes of landmarks (`header nav main footer aside h1-h3 img video
  iframe canvas svg form input button select textarea table`, up to 80) and of the 60 largest
  other blocks in the region, addressed by `:nth-of-type` paths. Each scores the IoU of the two
  rects, 0 when ours has no box or no such element. The part is the mean weighted by the
  square root of visible area.
- **Pixels (30%)**: SSIM over 8x8 windows of the two screenshots scaled to 320x200
  grayscale, averaged over the two screens.

`score = 100 * (0.4 text + 0.3 layout + 0.3 pixels)`. A crash, panic or timeout (90 s) gives
the failed parts 0.

### Pass threshold

A page passes at 58 or more (`--pass` overrides it). The value comes from 13 pairs of
screenshots (ours next to WebKit, top screen) that were judged by eye before the cut was
chosen. They span the scores 54 to 89: github.com 54.5, nginx.org 59.3, telekom.com 59.8,
gravatar.com 60.4, microsoft.com 62.0, netflix.com 66.3, slack.com 69.7, gov.uk 70.8,
bing.com 71.5, intuit.com 74.5, cloudflare.com 76.3, google.com 83.0 and vimeo.com 89.3.
Every page from nginx.org up, apart from bing.com and intuit.com, looked right to a reader:
the same content in the same places, with smaller defects such as a wrong font, a missing
icon or a column that wraps differently. github.com, missing its hero, did not. Two pages
looked wrong and still scored above the cut. On bing.com the background photo is missing,
and on intuit.com the brand logos are drawn huge. Both pages are sparse: few words and
boxes, so text and layout agree while the pixels do not. vimeo.com is not usable: WebKit
shows the site's own hydration error there, because the page refuses the loopback host. At
58 the verdict agrees with the eye on 10 of the 12 usable pairs; both misses are false passes,
so the pass rate is an upper bound. Generate pairs with
`--pass 101`, which keeps every page's PNGs.

## Determinism

The frozen bytes and the WebKit reference do not change between runs. WPT results were
identical in every run so far, as were the scores of most sites. The renderer runs page
timers on the wall clock and lets 1 s of idle time pass before it draws. On the few sites
whose scripts run for 5 to 20 s, how much script finishes in that time depends on CPU
load, so their scores move by up to a few points between runs.
`bun tools/corpus/run.ts --compare A B --tolerance <points>` lists every difference and fails
when one exceeds the tolerance.

## WPT

`wpt-tests.json` lists the tests at the pinned commit: up to 18 reftests from each of 18
directories (CSS 2 box model, floats, positioning, backgrounds, borders, text, line boxes and
selectors; css-flexbox, css-grid, css-backgrounds, css-text, css-box, css-display,
selectors), spread evenly over each directory's sorted list, without paged, interactive or
animated tests, plus 25 `dom/nodes` testharness tests.

Reftests render at 800x600. A test passes when our test render equals our reference render,
within its `<meta name=fuzzy>` allowance, or differs from it for a mismatch test.
Testharness tests pass when the harness reports OK and every subtest passes; our run reads
the result from the console line a replacement `testharnessreport.js` prints. WebKit runs
the same tests once (`wpt-webkit.json`), and the report also gives our pass rate on the
tests WebKit passes.

## Failure causes

`lib/classify.ts` names why a failing site lost points:

| category | evidence |
|---|---|
| process crash/panic/timeout | exit status, `panicked at`, the 90 s limit |
| uncaught script errors | `[js]` error lines from ours that WebKit did not also throw, numbers and URLs removed |
| CSS dropped by our parser | `RENDER_CSS_DROPS=1` counts declarations that produced nothing, skipped at-rules, unknown pseudo-classes and pseudo-elements, and `revert` read as `initial`. Drops that a blank page also shows, and drops that cannot change a still screenshot (cursor, transitions, other engines' prefixes...), are left out |
| unsupported elements | `video`, `canvas`, `iframe`, inline `svg` and form controls whose box is off by IoU < 0.5 |
| missing images/fonts | `<img>` that ours did not draw (with the WebP/GIF responses it was served, which it does not decode), and requests that missed the store |
| DOM differs from WebKit | a box WebKit has whose element our DOM lacks |
| layout diffs | a box off by IoU < 0.5, labelled by what placed it: `position`, float, grid or flex item, table, or block flow |
| text not laid out | over 20% of WebKit's characters absent from ours |

A failing page's lost points (100 minus its score) are split equally over the categories found
on it, then equally over the causes inside each category. The report ranks causes by the
points they took and gives each one's share of all points failing pages lost. The split is
a heuristic: it says where to look, not how much a fix would gain.

## Debug flags in the renderer

All are off unless set to `1`; when off, nothing in the output changes.

- `RENDER_TEXT_RUNS`: with `--boxes`, an element with no box of its own reports the rect of its
  picture (for `<img>`), and every text fragment follows the boxes as `text x y w h | text`.
- `RENDER_CSS_DROPS`: prints `[css-drop] <kind> <name> <count>` lines to stderr at exit.
- `RENDER_LOOPBACK_ONLY`: refuses connections to anything but loopback addresses.

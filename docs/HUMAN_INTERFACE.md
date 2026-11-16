# Human interface review

Between 2026-11-15 and 2026-11-16 Shadowroom's interface went through a measured
hill-climb. Fresh reviewers critiqued the app, the findings were fixed in rounds,
and after each round a separate, fresh set of reviewers re-scored it on the same
rubrics. This page records the method, the scores, the decisions taken, and what
is still open. Raw scores are in
[benchmarks/hi-scores.json](benchmarks/hi-scores.json), contrast results in
[benchmarks/hi-contrast.json](benchmarks/hi-contrast.json), and latency in
[PERFORMANCE.md](PERFORMANCE.md#human-interface-responsiveness-2026-11-16).

The goal was to refine the existing identity, not replace it: Geist Mono
throughout, near-black neutral surfaces, hairline borders, a single blue accent
that means "you are here", a neutral image canvas, and colour reserved for
decisions. Every reviewer checked for drift from that identity in every round.

## Method

**Evidence.** [tools/hi-audit/capture_states.py](../tools/hi-audit/capture_states.py)
drives the release binary through about 120 states on a private X server:
empty library, catalog, review, compare, survey, focus mode, every overlay and
dialog, Import, export pickers, and save failures. It covers Shadowroom Dark and
Light at 1440 × 900, Dark at 1024 × 680, and a pass with the catalog locked.
The same states were captured for every round, so reviewers compared like with
like. Reviewers also read the code at each commit with `git show`, to judge
keyboard paths, focus, Escape order, motion and anything a still image can't show.

**Reviewers.** Each surface had its own reviewer:

| Surface | Rubric |
| --- | --- |
| Visual design | 14 weighted criteria (typography, spacing, colour discipline, state clarity, image-first canvas, density, thumbnail decisions, icons, empty/loading/error craft, motion, light parity, polish, keyboard hints, novelty within identity), 0–5, with Photo Mechanic, Lightroom Classic, Narrative Select and Zed scored on the same rubric from public material |
| Accessibility and platform | checks in 8 categories (contrast pairs, keyboard, pointer, text, colour independence, motion, assistive technology, platform conventions) against WCAG 2.2 AA, the macOS HIG, the GNOME HIG and KDE HIG; percent passed |
| Review, Compare and Survey | Nielsen's 10 heuristics plus visual polish and expert efficiency, 0–5 |
| Catalog and window chrome | same |
| Import and export | same |
| Overlays, dialogs and feedback | same |

Re-scoring reviewers were told not to read the implementers' notes and to judge
only from captures and code. They kept the original criteria, scales and anchors,
copied earlier columns unchanged, and recorded every finding as resolved,
partial, unresolved or regressed.

Reference-app scores are rough (±0.5–1): they come from public screenshots and
documentation, not hands-on use. Timings in the capture manifests were taken
while the host was heavily loaded by unrelated builds, so they were not compared.

## Scores

Commits: before `c0760d5`, r1 `3a5567b`, r2 `e355fe4`, r3 `05c6fc1`, r4 `837512e`, r5 `72b936e`.

| Surface | before | r1 | r2 | r3 | r4 | r5 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Visual design, weighted (0–5) | 2.78 | 3.44 | 3.60 | 3.85 | 3.93 | **4.18** |
| Accessibility and platform (% of checks) | 50.6 | 62.8 | 67.1 | 66.8 | 77.4 | **76.5** |
| Review, Compare and Survey (0–5) | 2.67 | 3.21 | 3.75 | 4.04 | 4.17 | **4.42** |
| Catalog and chrome (0–5) | 2.42 | 2.73 | 3.43 | 3.72 | 3.90 | **4.08** |
| Import and export (0–5) | 2.08 | 2.29 | 3.17 | 3.25 | 3.75 | **4.17** |
| Overlays and dialogs (0–5) | 2.54 | 3.54 | 3.52 | 3.79 | 3.79 | **4.00** |

The four surface rows average all 12 criteria; Nielsen-only means are in the JSON.

**Design against the references** (same rubric, reference scores fixed):
Photo Mechanic 2.77, Lightroom Classic 3.26, Narrative Select 3.37, Zed 4.21.
Shadowroom's whole-rubric 4.18 benefits from two photo criteria that Zed isn't
scored on. On the 11 criteria Zed is scored on, Shadowroom is at 3.99. The
remaining gap is mostly typography and spacing.

**Accessibility by category:**

| Category | before | r1 | r2 | r3 | r4 | r5 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Contrast (71 pairs × 2 themes) | 75.4 | 100 | 100 | 97.1 | 97.1 | 97.1 |
| Keyboard | 57.9 | 63.2 | 73.7 | 73.7 | 78.9 | 78.9 |
| Pointer | 57.1 | 71.4 | 71.4 | 71.4 | 71.4 | 71.4 |
| Text | 33.3 | 33.3 | 33.3 | 33.3 | 77.8 | 77.8 |
| Colour independence | 60 | 80 | 80 | 80 | 80 | 80 |
| Motion and feedback | 60 | 80 | 90 | 90 | 90 | 90 |
| Assistive technology | 14.3 | 14.3 | 28.6 | 28.6 | 57.1 | 57.1 |
| Platform conventions | 46.7 | 60 | 60 | 60 | 66.7 | 60 |

The contrast row is strict. Four choice-group outline pairs fail by design (see
decision 1); counting them as decorative gives 100% from r3 on. High Contrast
Dark and Light pass every comparable pair.

## What each round changed

- **Round 1: feedback, colour roles, dialogs.**
  - About 30 completed-action messages had been written but only shown while
    quitting. They became transient status-bar notices with Undo, Reveal or Retry.
  - "Saving…" stopped flickering on preference writes. That removes a 0.9 s
    second repaint after every filter or view change (902 ms → 160 ms to settle).
  - Blue was reserved for location; selection now always outranks hover; photo
    badges got theme-independent colours, and labels got initials.
  - Dialogs got one frame and one button hierarchy. Preferences became keyboard
    operable, with a guarded draft.
- **Round 2: Import, culling efficiency, responsiveness.**
  - Import got a current-tile highlight separate from the import set, byte-weighted
    progress with speed and time left, and full keyboard use.
  - Exports were scoped to the current view and given a count.
  - Zoom moved to the pointer. Compare became usable at 1024 × 680, and the
    auto-advance toggle and position stayed visible at every width.
  - Frame tracing went in, preview textures are prepared before navigation, and
    short fades honour Reduce motion.
- **Round 3: regressions and the cull ledger.**
  - Palette order, the single modal scrim, a Tab trap in dialogs, and the
    decision note on the photo.
  - The cull ledger (every frame's decision on one rail), filled label chips with
    strips, and hairline segment borders.
- **Round 4: depth and platform.**
  - The inspector culling line (exposure, capture time, camera, lens).
  - Focus mode without chrome, peaking styles, a clipping overlay, zoom presets,
    and photo and folder search.
  - Open Folder separated from Import from Card; a second card queues while one
    copies; a pulled card pauses and resumes; import History; notifications.
  - Text size, a 720 × 480 minimum window, the native file chooser, High Contrast
    themes, a rebuilt image menu, F6 pane cycling, and unsaved-decision markers.
- **Round 5: precision and signature.**
  - One clipping rule shared by overlay and readout, with highlight stripes that
    no longer read as "reject".
  - Overlays stay visible while the next one is prepared.
  - The ledger shape-codes decisions, notches skipped frames and brackets the
    visible filmstrip.
  - A lights-out level of Focus mode, with a typed decision stamp.
  - Warning and label-blue hues moved away from rating amber and the accent.
  - A type-role pass so every text size is a named role.
  - Exports keep their context; per-card queue controls; a remounted card is
    recognised by its filesystem UUID; each card keeps its result.

## Decisions

1. **Choice-group outlines are hairlines.** Segmented controls are named by their
   text, so their outline is decorative. The selected segment's bar and the
   keyboard focus ring carry the state at 3:1 or more, and a test checks this in
   all four themes. All six reviewers agreed. A strict reading of WCAG 1.4.11
   still counts the outline pairs, so both scores are reported.
2. **Short windows give the photo the filmstrip's height.** Below 600 px tall the
   filmstrip yields to the ledger band, which stays clickable. At 800 × 520 the
   photo goes from 19% to 34% of the window. Reviewers agreed.
3. **Export pickers.** GPUI 0.2.2's Linux portal wrapper hard-codes the dialog
   title and can't take a start folder, so round 5 kept exports in the built-in
   picker, which shows the count, size and destination rule. Five reviewers
   agreed. The accessibility reviewer disagreed: the platform chooser is the only
   file UI a screen reader can reach. Round 6 therefore calls the portal directly,
   with a title and start folder, and keeps the built-in picker as the fallback.

## Still open

- **Screen readers.** GPUI 0.2.2 has no accessibility tree (AccessKit,
  AT-SPI or NSAccessibility). Names, roles and announcements need an
  AccessKit-enabled GPUI. Stable element ids are already in place for that.
- **Keyboard focus outside dialogs.** Toolbar, decision-bar, inspector and Import
  buttons are reachable through shortcuts and the palette, not by Tab.
- **Input methods.** Text fields bypass the platform input method, so IME,
  dictation and on-screen keyboards don't work in them.
- **Scaling and targets.** Text size scales type, not spacing or control height.
  Some targets (20 px stars, 12 px scrollbars) are below 24 px.
- **Catalog at scale.** The folder list is flat. Trash and Duplicates lack batch
  actions.
- **Colour vision.** Label pairs still collide under colour-vision simulation.
  The letter or key on each label carries the meaning.

## Reproduce

See [tools/hi-audit/README.md](../tools/hi-audit/README.md) for the screenshot
harness and the contrast audit, and
[PERFORMANCE.md](PERFORMANCE.md#human-interface-responsiveness-2026-11-16) for
`scripts/native-responsiveness.py`.

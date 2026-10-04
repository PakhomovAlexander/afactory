# The afactory design system

One identity for the three places `af` is seen: the website, the terminal, and GitHub.
The four files in `logo/source/` are the design; every other file here is derived from them
by `gen.py`, and every colour below is one of the seven the logo uses or a shade computed
from two of them. Change the design in `logo/source/`, run `python3 brand/gen.py`, and copy
what changed to the website (`website/af/README.md` says which four files).

```
brand/
├── README.md          this brand book
├── tokens.css         the tokens as CSS custom properties, dark first, light under a media query
├── tokens.json        the same tokens for tools (colour per theme, type scale, spacing, radius)
├── gen.py             derives everything below from logo/source/
├── ascii.txt          the worker and the wordmark in printable ASCII, for terminals
├── github-labels.sh   recolours the repository's issue labels onto the palette
├── logo/
│   ├── source/        the four CorelDRAW exports, verbatim (white/black × colour/mono)
│   ├── wordmark.svg   letters in currentColor, workers in colour: inline it in HTML
│   ├── wordmark-on-dark.svg, wordmark-on-light.svg   letters baked, for <img>
│   ├── wordmark-mono.svg   letters currentColor, workers grey: one-colour print
│   └── wordmark-one-ink.svg   everything one colour, eyes cut out: stamps, etching
├── worker/            the three workers, cropped from the wordmark, exact vectors + 202px PNGs
├── mark/              the pixel worker on a 16-cell grid: favicons, app icons, avatars
└── banner/            README banners (dark, light), the GitHub social preview, the OG card
```

## What the mark says

The wordmark reads AFACTORY in blocky capitals, and three small workers are at work on it:
a pink one inside the counter of the A, a green one inside the C, a blue one standing on the
O. The workers are the product: many sandboxed Workers, one kernel. Each worker is a factory
silhouette (a chimney, a body, a plinth) with two ink eyes; the three differ in colour and
in the eyes. Pink has plain eyes, green has lashes along the top, blue has a notch at the
bottom. Keep all three whenever the wordmark appears; never recolour one to match a page.

Voice: the product is `af` in code and in running text, "afactory" when the full name is
needed, and "Afactory" only at the start of a sentence. The tagline is *agent pipelines made
fast*, lowercase, no full stop; on the website's hero it becomes the sentence *Agent pipelines
made fast.* Nouns of the kernel keep their capitals as `CONTEXT.md` defines them: Worker,
Task, Snapshot, Finding, Provider.

## Colour

Seven primitives, taken from the logo files:

| token | value | what it is |
|---|---|---|
| `ink` | `#0F0F0F` | the dark ground, the letters on paper, every worker's eyes |
| `paper` | `#F6EDE4` | the light ground, the letters on ink |
| `white` | `#FFFFFF` | only inside the source wordmark for a white ground; not a UI ground |
| `grey` | `#999999` | the workers in one-colour print |
| `blue` | `#5195F5` | the actor: running, selected, focus, links on ink |
| `pink` | `#EE366A` | the accent and the fail signal: buttons, focus ring, the mark, errors |
| `green` | `#36EEA8` | the pass signal: verified, ready, ok |

The triad is the three workers, and it is also the three states a Worker's output can be in.
Blue is what is happening, green is what passed, pink is what failed or needs a person. Pink
doubles as the accent because the first worker in the wordmark is pink and because a call to
action and a refusal both want the eye. Nothing else is coloured: the rest of any surface is
ink, paper, and the greys between them.

Dark is the home ground. On ink all three triad colours pass as text (blue 6.4:1, pink 4.9:1,
green 12.7:1). On paper none of them do, so the light theme keeps the triad for fills and
switches text roles to `blue-deep #0D65E2`, `pink-deep #D51249` and `green-deep #0A7B50`, each
4.5:1 on paper. Text on any triad fill is always ink (`on-accent`). Hover lifts a fill toward
paper (`pink-soft #EF5780`, `blue-soft #7AABF1`, `green-soft #66EEB7`); a link on ink brightens
to `blue-soft`, on paper deepens to `#0B56C0`.

Grounds are steps between ink and paper: `surface` 4%, `raised` 8%, `line` 12%, `border` 18%
of the way. Text is `text` (the other primitive), `text-2` at 78% and `muted` at 62%, which is
6.8:1 on ink and 5.1:1 on paper. `tokens.json` carries the exact values per theme with a usage
note on each; `tokens.css` is the same as custom properties named `--af-*`.

Rules:

- A page ground is `bg`; a window or card one step off it is `surface`; a hovered row or a
  title bar is `raised`. Never a fourth step.
- Status is a word plus a colour, never colour alone: "verified" in `ok`, "refused" in `fail`,
  "running" in `active`. Green and pink are far apart in lightness (12.7 vs 4.9 on ink), so
  the pair survives colour blindness; keep the word anyway.
- The focus ring is 2px solid `focus` (pink) with a 3px offset on every ground.
- Selection is `selection` (blue) with `on-selection` (ink) text.
- No gradients, no shadows except the terminal window's drop shadow, no transparency tints.

## The logo

Files and when to use each:

- `logo/wordmark.svg`: inline it in HTML where the page controls `color`; the letters take
  the current text colour and the workers keep theirs. This is the hero on the website.
- `logo/wordmark-on-dark.svg` and `wordmark-on-light.svg`: for `<img>`, where currentColor
  cannot inherit. README banners and cards use these baked versions.
- `logo/wordmark-mono.svg`: one ink plus grey workers, for a print job with one colour.
- `logo/wordmark-one-ink.svg`: everything one colour with the eyes cut out, for a stamp, an
  etching or a sticker cut.
- `logo/source/*.svg`: the originals. Do not edit them by hand outside the design tool.

Clear space around the wordmark is the height of a worker's body on every side (about a
quarter of the wordmark's height). Below 160px wide the workers lose their eyes: use the
mark instead. Never retype the wordmark in a font, stretch it, rotate it, outline it, or place
it on a ground other than ink, paper or a photograph dark enough for paper letters. The wordmark
on ink uses paper letters, never white, except in the source file for white grounds.

## The worker

`worker/worker-{pink,green,blue}.svg` are the three workers cut from the wordmark, exact.
`worker/worker.svg` is the pink one with its body in currentColor and its eyes in ink, for a
tinted worker in a UI (a Worker row that takes the row's state colour). The 101px PNGs in
`worker/source/` are the designer's exports; the 202px PNGs beside the SVGs are renders.

**The pink worker is the mascot.** When one worker stands for afactory (a favicon, an avatar,
a cover, a spot illustration beside the wordmark) it is the pink one, and it stands to the
left of the wordmark, never to its right. Green and blue appear only inside the wordmark, in a
row of three, or as a state tint in a UI. A page may show the mascot once, three workers as a
row (always pink, green, blue, in that order, as in the wordmark), or none. It is never
animated except for a state change (idle to running to done), never given limbs, a mouth or a
speech bubble, and never drawn in a fourth colour except grey in print and currentColor in a
UI that tints it by state.

## The mark

`mark/mark.svg` is the worker redrawn on a 16-cell grid (chimney 4 wide, body 12, plinth 16,
eyes 3 wide by 5 tall), pink by default; `mark-green.svg` and `mark-blue.svg` are the same
grid. It is transparent, so it works on a light or a dark tab bar. Render it only at whole
multiples of 16px (16, 32, 48, 64, 128, 512) so the cells stay square; `gen.py` shows how the
180px Apple icon is an 11x render padded on an ink tile. `mark-tile.svg` puts the pink worker on
an ink tile with a 3-cell radius (`radius-tile`, 18.75%) for contexts that need an opaque icon:
app icons, avatars, the web manifest.

The website's header shows the mark at 32px beside the wordmark at 28px tall (168px wide, its
minimum). The name is never typed as a word where the wordmark can stand.

## Type

One face everywhere: Fira Code, variable 300 to 700, with `ui-monospace, SFMono-Regular, Menlo,
Consolas, monospace` behind it. The website subsets it; the TUI and GitHub use whatever
monospace the reader has. Ligatures are off inside code and recorded output so `->` and `&&`
show what was typed; on in prose.

| style | size / leading / weight | use |
|---|---|---|
| `hero` | 56px / 1.06 / 600, tracking -0.045em | the one sentence under the wordmark; clamps to 32px on a phone |
| `h2` | 32px / 1.15 / 600, tracking -0.02em | section titles |
| `h3` | 24px / 1.25 / 600 | card and step titles |
| `lede` | 19px / 1.7 / 400 | the paragraph under a title |
| `body` | 16px / 1.65 / 400 | running copy; the site clamps 15 to 17px |
| `ui` | 15px / 1.2 / 500 | buttons, nav, tabs |
| `eyebrow` | 13px / 1.4 / 500, tracking 0.08em, uppercase | section labels, in `accent-text` |
| `meta` | 13px / 1.5 / 400, tracking 0.02em | captions, the tagline, footers, in `muted` |
| `code` | 15px / 1.6 / 400, no ligatures | commands and output |

Running text stays near 65 characters wide. Headings are sentence case. Nothing is set in
all caps except the eyebrow and the status words a terminal already prints in caps.

## Space and radius

A 4px grid: `space-1` 4, `space-2` 8, `space-3` 12, `space-4` 16, `space-5` 24, `space-6` 32,
`space-7` 48, `space-8` 64, `space-9` 96. The page gutter is `space-4` on a phone and `space-6`
on a desktop; sections breathe `space-8` to `space-9`; a card pads `space-4`; a button pads
`space-3` by 19px.

Radii: `radius-0` for everything pixel (the mark, the workers, progress bars), `radius-1` 4px
for inputs, chips and the focus ring, `radius-2` 6px for buttons, `radius-3` 8px for panels and
the terminal window, `radius-tile` 18.75% for the icon tile.

## The website (af.apkhmv.xyz)

`tokens.css` is copied verbatim to the top of the site's stylesheet; the site's own names
(`--color-bg`, `--color-accent`, …) are aliases of `--af-*` beneath it. Dark is the default and
`prefers-color-scheme: light` switches to paper; the showcase terminal pins the dark tokens on
its own element so a terminal stays ink in both themes. Its three window dots are the three
workers: pink, blue, green.

The hero inlines `wordmark.svg` at up to 780px wide, then the `hero` sentence, the `lede`, a
pink primary button and a ghost button, then a `meta` line. The header and the footer are the
32px mark and the wordmark at 28px; the name is never typed. Links are `link`; the eyebrow is `accent-text`; a primary button is `accent`
with `on-accent` text and lifts to `accent-hover`; a ghost button has a `border` border and
takes `accent-text` on hover.

Favicons come from `mark/` (16 and 32 transparent, 180 and 512 on the ink tile), the Open Graph
card from `banner/banner-dark.svg` cut to 1200×630 on ink. The website's `af/scripts/icons.sh`
regenerates them from its copies.

## The TUI

The browser that opens on bare `af` (docs/design/tui.md) paints printable ASCII only and takes
the terminal's own foreground and ground, so it looks native in any terminal theme. It never
colours text on that ground, because no colour chosen inside the program reads on every ground:
pink text is 3.4:1 on a light terminal, green 1.5:1. Colour comes only as a fill with ink on it,
where the contrast holds whatever the theme (ink on blue 6.4:1, on pink 4.9:1, on green
12.7:1): the status line, the state chips and the mascot. Colour is detected once, in
`crates/af/src/tui/paint.rs`:

| `NO_COLOR` | `COLORTERM` | palette | status line | chips: active, ok, fail |
|---|---|---|---|---|
| set | any | mono | reverse video, bold too with an error | the word alone, fail in bold |
| unset | anything else | ansi | reverse video; black on bright red with an error | black on the terminal's bright blue, green, bright red (`30;104`, `30;42`, `30;101`) |
| unset | `truecolor` or `24bit` | true | ink on `blue`; ink on `pink` with an error | ink on `active-fill`, `ok-fill`, `fail-fill` |

A chip is a state word with a space either side, on its tone's fill. It takes the place of
spaces the plain row already had, so a row's text is the same in every palette, and it keeps its
fill on the cursor row. The tones are the triad's: blue for what is happening (a running Task
or stage, reserved Attempts), green for what passed (a done Task, an `[ok]` stage, an
`authenticated` Provider, Attempts settled ok), pink for what failed or needs a person (a failed
Task or stage, a Task awaiting approval, a Provider that cannot be used, Attempts settled
failed). Chips stand on the Task header's STATE word, the stage marks `[ok]` `[..]` `[!!]`, the
progress that ends a Task's bar row (right-aligned, so the chips stand in one column), a
Provider's STATUS, and the Workers pane's Attempt counts above zero. An error row starts with
an `error` chip (`refused` for a refused preview, `warning` for a warning) and gives its message
in bold. The status line is the blue actor (it names the mode and the selected node) and turns
pink while it carries an error.

Everything else is the terminal's text with bold for titles, dim for muted rows, reverse for
the cursor row and underline for the unfocused cursor. Bars are `#` and `.` in the text colour,
folds are `v` and `>`, branches are `+--` and `'--`. Nothing moves but a state change and the
spinner of a read still running.

For a banner in a terminal (a version screen, a help header) use `ascii.txt`: the 11×6 worker
and the 5-row block wordmark. Never a figlet face. The TUI's help header puts the worker to the
left of the name; under truecolor its `#` cells are pink on pink and its eyes ink, a solid pixel
worker whose copied text is still the drawing.

## GitHub

- The README opens with `banner/banner-dark.svg` and `banner/banner-light.svg` in a `<picture>`
  that follows the reader's theme, then the badges, then the first paragraph. The banner is the
  title; there is no `# Afactory` heading above it.
- Badges are shields.io `flat-square` with `labelColor=0F0F0F`; the value colour is the state
  colour of what the badge reports: green for a passing build, blue for a version, pink for
  the licence (the one thing to notice). Never the default blue and green of shields.
- The social preview is `banner/social-preview.png` (1280×640): upload it by hand in the
  repository's settings, under Social preview, because GitHub has no API for it.
- Issue labels take the palette in four groups, `github-labels.sh` applies them: pink for
  something wrong (`bug`, `invalid`), blue for something to do (`enhancement`, `question`,
  `documentation`), green for something settled or welcoming (`help wanted`, `good first
  issue`, `accessibility`), grey for housekeeping (`duplicate`, `wontfix`), ink for tooling
  (`dependencies`, `rust`, `github_actions`).
- The repository avatar, when it is not the owner's own, is `mark/favicon-512.png`.
- Release notes and issue templates are plain Markdown in the voice above; no emoji, no
  banners inside them.

## Do not

- Retype AFACTORY in a font, or set the wordmark in a colour other than paper, ink, grey or
  currentColor.
- Separate a worker from its eyes, add a fourth worker colour, or animate one for decoration.
- Use yellow, orange or purple anywhere; the palette has three hues and they are spoken for.
- Put pink text on paper below 19px bold; use `pink-deep`.
- Round a pixel asset.
- Colour text on the terminal's own ground in the TUI: colour there is a fill with ink on it.

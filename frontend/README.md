# Lug frontend

React + Vite + Tailwind. `npm run build` performs TypeScript checking and writes
production assets to `../web/`, which the Rust server serves. Building locally
does not deploy anything.

## Local preview and checks

```sh
npm ci
npm run test:ui
npm exec vite preview -- --host 127.0.0.1 --port 4173 --strictPort
```

`test:ui` builds production output, then uses a separate preview server on port
4174. The tests cover light/dark themes, English/Chinese, responsive layouts,
STIX loading, favicon, keyboard focus, text contrast, demo modes, copying,
documentation navigation and console dialogs. Console API responses are mocked;
these are frontend checks, not backend or GitHub OAuth integration tests.
The existing `npm test` suite remains the separate legacy integration harness.
Screenshots are written to `test-results/ui/` (not committed).

The preview follows the operating system's color scheme, just like production.
No manual theme/font debug toolbar or preview-only JavaScript is shipped.
To compare themes locally, use the OS appearance setting or browser DevTools
Rendering → Emulate CSS media feature `prefers-color-scheme`.

## Lab 530 visual language

- Source of truth: the `lab530-style` skill's confirmed design/template.
- Light: ivory `#faf9f6`, navy `#031931`, secondary text `#64707d`.
- Dark: near-black `#111418`, warm light text `#f2f0e9`, secondary `#a0a8b2`.
- Titles: STIX Two Text, weight 400, tracking `0.055em`; primary titles use
  `clamp(36px, 5vw, 48px)`. Chinese has explicit Songti/STSong/SimSun fallbacks.
- UI text uses system sans-serif; code keeps Geist Mono. Semantic tokens in
  `src/styles.css` also cover forms, warnings, errors, code blocks and dialogs.
- Lug uses only the STIX Two Text wordmark, with no separate icon or square L,
  in navigation, the footer and the calculation demo.
  Keep the hero free of crests. The footer pairs the Lug wordmark with a fine
  vertical divider and the Lab 530 crest/wordmark linking to `https://by530.com/`.
  Keep the approved AI positioning and inline `By Lab 530` credit. The crest
  does not appear in the navigation, docs or console.
- Use grids for alignment, not visible cell boundaries. Separate feature rows,
  metrics and page sections with whitespace and typography, not decorative rules.
  Retain purposeful lines: table rows, input/button affordances, focus and selected
  states, calculation-flow connectors, and the footer's brand divider.
- No gradients, decorative shadows, animated entrances or font comparisons.

### Font and favicon assets

`src/assets/stix-two-text.woff2` is a real WOFF2 conversion of the skill's
`assets/starter/assets/fonts/stix-two-text.ttf`, with names and Unicode cmap
verified after conversion. No external font requests. The original OFL license
is copied into the production distribution from
`public/assets/stixtwotext-OFL.txt`; retain it when distributing the font.
The license applies to the font, not to Lug's product mark or the application.

Conversion (with FontTools and Brotli installed):

```python
from fontTools.ttLib import TTFont
font = TTFont("stix-two-text.ttf")
font.flavor = "woff2"
font.save("stix-two-text.woff2")
```

`src/assets/lab530-crest.svg` is an unmodified copy of the skill's crest, with
its original `0 0 1254 1254` viewBox and whitespace preserved. Only this image
uses `brightness(0) invert(1)` in dark mode. The crest is a brand asset, not
covered by the font's OFL license.

`src/assets/favicon.svg` uses plain `Lug` lettering on a transparent background,
with its own system-theme media query. Its self-contained SVG text uses STIX
where available and Georgia/serif as fallbacks, without external font requests. The three HTML entry points share the same font preload, SVG favicon and
light/dark `theme-color` metadata. Vite hashes these assets under `/assets/`;
WOFF2 and SVG work with the existing backend resource routes.

Deployment requires explicit approval for each update; see `../deploy/README.md`.

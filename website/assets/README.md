# Website artwork

`social-preview.svg` is the editable source for `../og.png` (1200 × 630).
The original diagram shows existing content, a complete updated file and the
single new part to upload. It is an illustration, not a transfer benchmark.
Its colors and typography follow root `design.md`.

The SVG contains text, not outlined glyphs. Rendering needs Manrope, IBM Plex
Sans and IBM Plex Mono; a missing font must not silently change the exported
artwork. The preview uses Manrope 650/750, Plex Sans 400/500 and Plex Mono 400.
The browser fonts and their OFL licenses are documented in [fonts/README.md](fonts/README.md).

Rebuild from the repository root with `rsvg-convert` and Fontconfig. The command
below downloads official TTFs at the same pinned Google Fonts revision for the
renderer only. They remain under ignored `target/`; they are not website payload.
The isolated font configuration prevents substitution by machine-local fonts.

```sh
mkdir -p target/social-preview/font-cache
curl -fLsS 'https://raw.githubusercontent.com/google/fonts/e7db6ca603fd4862010383ff17ffd6cc4ffac1c0/ofl/manrope/Manrope%5Bwght%5D.ttf' -o target/social-preview/Manrope.ttf
curl -fLsS 'https://raw.githubusercontent.com/google/fonts/e7db6ca603fd4862010383ff17ffd6cc4ffac1c0/ofl/ibmplexsans/IBMPlexSans%5Bwdth,wght%5D.ttf' -o target/social-preview/IBMPlexSans.ttf
curl -fLsS 'https://raw.githubusercontent.com/google/fonts/e7db6ca603fd4862010383ff17ffd6cc4ffac1c0/ofl/ibmplexmono/IBMPlexMono-Regular.ttf' -o target/social-preview/IBMPlexMono.ttf
python3 - <<'PY'
from pathlib import Path
from xml.etree.ElementTree import Element, SubElement, ElementTree
directory = Path('target/social-preview').resolve()
config = Element('fontconfig')
SubElement(config, 'dir').text = str(directory)
SubElement(config, 'cachedir').text = str(directory / 'font-cache')
ElementTree(config).write(directory / 'fonts.conf', encoding='utf-8', xml_declaration=True)
PY
FONTCONFIG_FILE="$PWD/target/social-preview/fonts.conf" rsvg-convert --width 1200 --height 630 --output website/og.png website/assets/social-preview.svg
```

Rendered and visually inspected with librsvg 2.61.3 on 2026-10-24. The committed
PNG is used by social crawlers, so the website build does not require this tool
or the render-only fonts. Reinspect the PNG after any change to the source.

The docs favicon is already linked to the marketing favicon through
`site/theme/favicon.svg` → `website/favicon.svg`; mdBook copies and fingerprints
that theme asset. Keep the link so both surfaces use the same mark.

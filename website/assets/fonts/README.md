# Website fonts

Official Google Fonts Latin WOFF2 subsets, downloaded 2026-10-24. Font data is
unchanged; filenames are descriptive local names. Serve these files from
openxet.com with `font-display: swap` and system fallbacks. No visitor request
to Google's font service is needed.

| File | CSS family | Style | CSS weight | Bytes |
|---|---|---|---|---:|
| `manrope-latin-variable.woff2` | `Manrope` | normal | `400 800` | 24,576 |
| `ibm-plex-sans-latin-variable.woff2` | `IBM Plex Sans` | normal | `400 600` | 40,240 |
| `ibm-plex-mono-latin-400.woff2` | `IBM Plex Mono` | normal | `400` | 10,052 |

Total font payload: **74,868 bytes (73.1 KiB)**. Weight ranges come from the
official response to this [Google Fonts CSS request](https://fonts.googleapis.com/css2?family=IBM+Plex+Mono:wght@400&family=IBM+Plex+Sans:wght@400..600&family=Manrope:wght@400..800&display=swap),
using a Chrome 130 user agent. Plex Mono has one regular face; use weight 400
for code rather than synthesizing a heavier face.

Original files:

- [Manrope Latin WOFF2](https://fonts.gstatic.com/s/manrope/v20/xn7gYHE41ni1AdIRggexSvfedN4.woff2)
- [IBM Plex Sans Latin WOFF2](https://fonts.gstatic.com/s/ibmplexsans/v23/zYXzKVElMYYaJe8bpLHnCwDKr932-G7dytD-Dmu1syxeKYbSB4Zh.woff2)
- [IBM Plex Mono Latin WOFF2](https://fonts.gstatic.com/s/ibmplexmono/v20/-F63fjptAgt5VM-kVkqdyU8n1i8q131nj-o.woff2)

All three use the SIL Open Font License 1.1. The accompanying license files
are copied from their respective `ofl/manrope`, `ofl/ibmplexsans`, and
`ofl/ibmplexmono` directories in
[`google/fonts` at e7db6ca603fd4862010383ff17ffd6cc4ffac1c0](https://github.com/google/fonts/tree/e7db6ca603fd4862010383ff17ffd6cc4ffac1c0/ofl).

SHA-256 checksums:

```text
e310b55a7fd9677f5e3555e6c6c4d064fa1f1d24393f0ddbe217cea12a8c432f  manrope-latin-variable.woff2
056e4e2459f57a0033c8c9c844ff19d6e42ac8602027803d4345823bcc939818  ibm-plex-sans-latin-variable.woff2
c36f509c0a8f9f85f29cb44bc8701d8a9e0b14c499e77a884f789ead7093a7ac  ibm-plex-mono-latin-400.woff2
```

These are Latin subsets, not complete multilingual font distributions. Use
system fallbacks for missing glyphs; add official language subsets only when
the site needs them. Google Fonts declares the same Unicode range for these
three files:

```text
U+0000-00FF, U+0131, U+0152-0153, U+02BB-02BC, U+02C6, U+02DA,
U+02DC, U+0304, U+0308, U+0329, U+2000-206F, U+20AC, U+2122,
U+2191, U+2193, U+2212, U+2215, U+FEFF, U+FFFD
```

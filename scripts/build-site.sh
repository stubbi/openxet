#!/usr/bin/env bash
# Builds https://openxet.com into target/site/public, as pages.yml deploys it:
# the landing page from website/, the docs from docs/ rendered by mdBook
# (site/book.toml) under /docs/ with each page's plain Markdown next to its
# HTML, and /llms.txt and /sitemap.xml generated from docs/SUMMARY.md.
# Fails on a page missing from the navigation, a broken repository link, or
# any mdBook warning. Needs mdbook 0.5.4 and python3.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/target/site"
public="$out/public"

python3 -I "$root/scripts/check-docs-nav.py"

rm -rf "$out"
log="$(mktemp)"
trap 'rm -f "$log"' EXIT
mdbook build "$root/site" 2>&1 | tee "$log"
if grep -qE '(WARN|ERROR)' "$log"; then
  echo "mdbook reported the warnings above; fix them, the site build treats them as errors" >&2
  exit 1
fi

mkdir -p "$public"
cp -R "$root/website/." "$public/"
cp -R "$out/book/html" "$public/docs"
# The Markdown renderer's output is each page after preprocessing: includes
# expanded and repository links resolved, as the HTML has them.
cp -R "$out/book/markdown/." "$public/docs/"

python3 -I "$root/scripts/generate-site-index.py" "$public"
echo "site written to $public"

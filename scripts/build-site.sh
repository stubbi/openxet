#!/usr/bin/env bash
# Builds https://openxet.com into target/site/public, as pages.yml deploys it:
# the landing page from website/, the docs from docs/ rendered by mdBook
# (site/book.toml) under /docs/ with each page's plain Markdown next to its
# HTML. Page metadata and /llms.txt come from docs/SUMMARY.md and the rendered
# Markdown; /sitemap.xml includes every indexable page's canonical URL.
# Fails on a page missing from the navigation, a broken repository link, any
# mdBook warning, or an llms.txt entry without a description. Needs mdbook
# 0.5.4, python3 and Node.js 22 or later.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/target/site"
public="$out/public"

want="mdbook v0.5.4"
have="$(mdbook --version 2>/dev/null || true)"
if [ "$have" != "$want" ]; then
  echo "the site build needs $want, found ${have:-no mdbook on PATH}; install it with: cargo install mdbook --version 0.5.4 --locked" >&2
  exit 1
fi

python3 -I "$root/scripts/check-docs-nav.py"
# The link preprocessor must refuse includes from outside the repository.
python3 -I "$root/site/test_repo_links.py" -q
python3 -I "$root/site/test_site_output.py" -q
node --test "$root/site/"test_*.mjs

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
# Every docs entry in llms.txt carries a note: agents choose pages by it.
if bare="$(grep -E '^- \[[^]]+\]\(https://openxet\.com/docs/[^)]+\)$' "$public/llms.txt")"; then
  echo "these llms.txt entries have no description; add a <!-- description: ... --> line under the page's title, or start the page with a prose paragraph:" >&2
  echo "$bare" >&2
  exit 1
fi
echo "site written to $public"

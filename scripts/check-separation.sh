#!/bin/sh
#
# Keeps the Linux app and the macOS app from growing hidden dependencies on
# each other. Both may read spec/; neither may reach into the other.
#
#   scripts/check-separation.sh
#
# Run from CI on both sides. See docs/macos-port-spec.md section 4.3.
set -eu

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
failed=0

fail() {
    echo "separation: $1" >&2
    failed=1
}

# Tracked files, plus untracked ones that are not ignored, so the check also
# sees a file before it is committed.
if git rev-parse --git-dir > /dev/null 2>&1; then
    files="$(git ls-files --cached --others --exclude-standard)"
else
    files="$(find . -type f -not -path './.git/*' -not -path './target/*' | sed 's|^\./||')"
fi

# macos/ must not point at the Linux app. A text search is blunt on purpose.
macos_files="$(printf '%s\n' "$files" | grep '^macos/' || true)"
for f in $macos_files; do
    [ -f "$f" ] || continue
    if grep -InE '\.\./(src|tests|resources|packaging)([/"[:space:]]|$)' "$f" > /dev/null 2>&1; then
        fail "$f refers to the Linux app:"
        grep -InE '\.\./(src|tests|resources|packaging)([/"[:space:]]|$)' "$f" >&2 || true
    fi
done

# Xcode files may reach outside macos/ only into spec/.
for f in $(printf '%s\n' "$macos_files" | grep -E '\.(pbxproj|xcscheme|xcconfig|xctestplan)$' || true); do
    [ -f "$f" ] || continue
    if grep -In '\.\./' "$f" | grep -v '\.\./spec' > /dev/null 2>&1; then
        fail "$f refers outside macos/ other than ../spec:"
        grep -In '\.\./' "$f" | grep -v '\.\./spec' >&2 || true
    fi
done

# Nothing outside macos/ may refer to macos/. The exceptions are the files that
# have to name it: package exclusion, the Linux workflow's path filters, this
# script, the docs, the Mac workflow, and the spec tooling and its README.
others="$(printf '%s\n' "$files" | grep -v -e '^macos/' -e '^docs/' -e '^\.gitea/workflows/macos\.yml$' \
    -e '^scripts/check-separation\.sh$' -e '^Cargo\.toml$' -e '^\.gitea/workflows/deb\.yml$' \
    -e '^\.gitignore$' -e '^spec/README\.md$' -e '^spec/tools/' || true)"
for f in $others; do
    [ -f "$f" ] || continue
    if grep -Iq 'macos/' "$f" 2> /dev/null; then
        fail "$f refers to macos/:"
        grep -In 'macos/' "$f" >&2 || true
    fi
done

# Each language stays on its own side.
if printf '%s\n' "$macos_files" | grep -q '\.rs$'; then
    fail "macos/ contains Rust files:"
    printf '%s\n' "$macos_files" | grep '\.rs$' >&2
fi
if printf '%s\n' "$files" | grep -v '^macos/' | grep -q '\.swift$'; then
    fail "Swift files outside macos/:"
    printf '%s\n' "$files" | grep -v '^macos/' | grep '\.swift$' >&2
fi

if [ "$failed" -eq 0 ]; then
    echo "separation ok"
fi
exit "$failed"

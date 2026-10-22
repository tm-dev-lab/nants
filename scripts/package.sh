#!/bin/sh
# Builds and packages a Nants release locally. Used by .github/workflows/release.yml.
#
# Usage:
#   scripts/package.sh [--tag <tag>] [--base-url <url>] [--notes <file>] [--skip-tests]
#
#   --tag <tag>       vX.Y.Z (final release, the default) or vX.Y.Z-rc.N (test pre-release);
#                     X.Y.Z must be the Cargo.toml version. The tag sets the label of the
#                     packages and of latest.json, the default base URL and the notes
#   --base-url <url>  base of the download URLs written to latest.json
#                     (default: https://github.com/tm-dev-lab/nants/releases/download/<tag>)
#   --notes <file>    write the "## [X.Y.Z]" section of CHANGELOG.md (without its title and its
#                     link definitions) to <file>; for an rc, with a first line saying it is a
#                     test pre-release
#   --skip-tests      do not run the tests (only when another job already ran them)
#
# The version comes from the [package] section of Cargo.toml. The script refuses to run with
# uncommitted changes or without the "## [X.Y.Z]" section in CHANGELOG.md.
#
# Steps: tests (cargo test and the web tests through scripts/test.sh, then the web build, which
# must leave the tree clean), portable builds with cargo zigbuild (x86_64 and aarch64, glibc
# 2.31), reproducible archives and checksums.
#
# Output in dist-release/<tag>/ (LABEL is the tag without the leading v):
#   nants-LABEL-<target>.tar.gz, nants-LABEL-<target>.tar.gz.sha256 and latest.json
#
# It only builds, packages and writes files: it uploads nothing and creates no tag.
set -eu

# Portable targets: cargo zigbuild triple (glibc 2.31, Ubuntu 20.04 and later) and manifest name.
TARGETS="x86_64-unknown-linux-gnu.2.31:x86_64-linux aarch64-unknown-linux-gnu.2.31:aarch64-linux"
DEFAULT_BASE="https://github.com/tm-dev-lab/nants/releases/download"

say() { printf 'package: %s\n' "$*"; }
die() {
  printf 'package: ERROR: %s\n' "$*" >&2
  exit 1
}

# Version from the [package] section of Cargo.toml (single source).
cargo_version() {
  awk '
    /^\[/ { in_pkg = ($0 == "[package]") ; next }
    in_pkg && /^version[ \t]*=/ {
      sub(/^version[ \t]*=[ \t]*"/, ""); sub(/".*$/, ""); print; exit
    }
  ' "$ROOT/Cargo.toml"
}

# The version heading in CHANGELOG.md (Keep a Changelog): `## [X.Y.Z]`.
changelog_has() {
  [ -f "$ROOT/CHANGELOG.md" ] && grep -Eq "^## \[$(printf '%s' "$1" | sed 's/\./\\./g')\]" "$ROOT/CHANGELOG.md"
}

# Body of the `## [X.Y.Z]` section, without its heading, up to the next `## [`; link definitions
# (`[x]: url`) and leading and trailing blank lines are dropped.
changelog_section() {
  awk -v want="## [$1]" '
    index($0, "## [") == 1 {
      if (inside) exit
      if (index($0, want) == 1) { inside = 1; next }
    }
    inside && $0 !~ /^\[[^]]+\]: / { lines[++n] = $0 }
    END {
      first = 1
      while (first <= n && lines[first] ~ /^[ \t\r]*$/) first++
      last = n
      while (last >= first && lines[last] ~ /^[ \t\r]*$/) last--
      for (i = first; i <= last; i++) print lines[i]
    }
  ' "$ROOT/CHANGELOG.md"
}

tree_clean() {
  [ -z "$(git -C "$ROOT" status --porcelain --untracked-files=normal)" ]
}

# Refusal for a dirty tree: lists the pending paths first (names only, up to 30 lines), so the
# log shows what is dirty (a CI runner cannot be inspected afterwards).
die_dirty() {
  pending=$(git -C "$ROOT" status --porcelain --untracked-files=normal)
  total=$(printf '%s\n' "$pending" | wc -l | tr -d ' ')
  printf 'package: pending paths (git status --porcelain, %s in total):\n' "$total" >&2
  printf '%s\n' "$pending" | head -n 30 | sed 's/^/package:   /' >&2
  [ "$total" -le 30 ] || printf 'package:   ... and %s more\n' "$((total - 30))" >&2
  die "$1"
}

refusals() {
  tree_clean || die_dirty "the repository has uncommitted changes; commit or discard them first (git status)"
  changelog_has "$VERSION" || die "CHANGELOG.md has no ## [$VERSION] section; write the release notes first"
}

run_tests() {
  # Always through scripts/test.sh: each run gets its own temporary folder, removed at the end.
  t="$ROOT/scripts/test.sh"
  say "tests: cargo test (through scripts/test.sh)"
  (cd "$ROOT" && "$t" cargo test --quiet) || die "cargo test failed; fix it before packaging"
  say "tests: interface (web: npm test through scripts/test.sh, then npm run build)"
  (cd "$ROOT/web" && "$t" npm test --silent && npm run build --silent) || die "the interface tests or build failed"
  # The interface build is embedded in the executable: it must be committed exactly as built.
  tree_clean || die_dirty "the interface build (web/dist) changed or a test left files behind; commit the result and run again"
}

# Build tools: cargo-zigbuild, zig and the rustup targets. Without a system `cc`, host build
# scripts use `zig cc` through a temporary `cc` on the PATH (only for this process).
build_tools() {
  command -v cargo-zigbuild >/dev/null 2>&1 || die "cargo-zigbuild not found; install it with: cargo install cargo-zigbuild"
  if ! command -v zig >/dev/null 2>&1 && [ -x "$HOME/.local/zig/zig" ]; then
    PATH="$HOME/.local/zig:$PATH"
    export PATH
  fi
  command -v zig >/dev/null 2>&1 || die "zig not found on the PATH (cargo zigbuild needs it)"
  if ! command -v cc >/dev/null 2>&1; then
    shim="$WORK/cc-shim"
    mkdir -p "$shim"
    cat >"$shim/cc" <<'SHIM'
#!/bin/sh
# Temporary cc from package.sh: zig cc without the rustc --target (the host is zig's native target).
for a in "$@"; do
  shift
  case "$a" in --target=*) ;; *) set -- "$@" "$a" ;; esac
done
exec zig cc "$@"
SHIM
    chmod +x "$shim/cc"
    PATH="$shim:$PATH"
    export PATH
  fi
  if command -v rustup >/dev/null 2>&1; then
    for pair in $TARGETS; do
      rt=$(rust_triple "${pair%%:*}")
      rustup target add "$rt" >/dev/null 2>&1 || die "could not install the rustup target $rt"
    done
  fi
}

# rustup triple from the zigbuild triple: drops the whole glibc suffix (".2.31").
rust_triple() {
  printf '%s' "${1%%.*}"
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

# Builds and packages each target; writes "name url sha" lines to $WORK/assets.
build_and_package() {
  mtime=$(git -C "$ROOT" log -1 --format=%ct)
  : >"$WORK/assets"
  for pair in $TARGETS; do
    triple=${pair%%:*}
    name=${pair#*:}
    say "building $name ($triple)"
    (cd "$ROOT" && cargo zigbuild --release --locked --target "$triple") || die "the build for $triple failed"
    bin="${CARGO_TARGET_DIR:-$ROOT/target}/$(rust_triple "$triple")/release/nants"
    [ -x "$bin" ] || die "executable not found at $bin"
    stage="$WORK/stage-$name"
    rm -rf "$stage"
    mkdir -p "$stage"
    cp "$bin" "$stage/nants"
    chmod 755 "$stage/nants"
    file="nants-$LABEL-$name.tar.gz"
    tar --owner=0 --group=0 --numeric-owner --mtime="@$mtime" -C "$stage" -cf - nants | gzip -n -9 >"$OUT/$file"
    sum=$(sha256_of "$OUT/$file")
    printf '%s  %s\n' "$sum" "$file" >"$OUT/$file.sha256"
    printf '%s %s %s\n' "$name" "$BASE_URL/$file" "$sum" >>"$WORK/assets"
    say "package $file sha256 $sum"
  done
}

write_manifest() {
  date=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  {
    printf '{\n  "version": "%s",\n  "date": "%s",\n  "assets": {\n' "$LABEL" "$date"
    n=$(wc -l <"$WORK/assets")
    i=0
    while read -r name url sum; do
      i=$((i + 1))
      sep=","
      [ "$i" -eq "$n" ] && sep=""
      printf '    "%s": { "url": "%s", "sha256": "%s" }%s\n' "$name" "$url" "$sum" "$sep"
    done <"$WORK/assets"
    printf '  }\n}\n'
  } >"$OUT/latest.json"
  say "manifest $OUT/latest.json"
}

# Release notes: the CHANGELOG section; for an rc, a first line marks the test pre-release.
write_notes() {
  {
    if [ "$LABEL" != "$VERSION" ]; then
      printf '**Test pre-release %s.** It is not the final %s release.\n\n' "$LABEL" "$VERSION"
    fi
    changelog_section "$VERSION"
  } >"$1"
}

# Checks the tag format and that its X.Y.Z is the Cargo.toml version; sets LABEL.
check_tag() {
  printf '%s\n' "$1" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+(-rc\.[0-9]+)?$' &&
    [ "$(printf '%s\n' "$1" | wc -l | tr -d ' ')" = 1 ] ||
    die "invalid tag: $1 (use vX.Y.Z or vX.Y.Z-rc.N)"
  label=${1#v}
  [ "${label%%-*}" = "$VERSION" ] ||
    die "the tag $1 does not match the Cargo.toml version $VERSION (use v$VERSION or v$VERSION-rc.N)"
  LABEL=$label
}

main() {
  TAG=""
  BASE_ARG=""
  NOTES=""
  SKIP_TESTS=0
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --tag)
        [ "$#" -ge 2 ] && [ -n "$2" ] || die "--tag needs a tag"
        TAG=$2
        shift 2
        ;;
      --base-url)
        [ "$#" -ge 2 ] && [ -n "$2" ] || die "--base-url needs a URL"
        BASE_ARG=$2
        shift 2
        ;;
      --notes)
        [ "$#" -ge 2 ] && [ -n "$2" ] || die "--notes needs a file"
        NOTES=$2
        shift 2
        ;;
      --skip-tests)
        SKIP_TESTS=1
        shift
        ;;
      -h | --help)
        sed -n '2,28p' "$0" | sed 's/^# \{0,1\}//'
        exit 0
        ;;
      *) die "unknown option: $1 (see --help)" ;;
    esac
  done

  ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
  VERSION=$(cargo_version)
  [ -n "$VERSION" ] || die "could not read the version from Cargo.toml"
  [ -n "$TAG" ] || TAG="v$VERSION"
  check_tag "$TAG"
  if [ -n "$BASE_ARG" ]; then
    BASE_URL=${BASE_ARG%/}
  else
    BASE_URL="$DEFAULT_BASE/$TAG"
  fi
  say "version $VERSION (Cargo.toml), tag $TAG$([ "$SKIP_TESTS" -eq 1 ] && printf ', tests skipped (--skip-tests)')"

  refusals

  WORK=$(mktemp -d)
  trap 'rm -rf "$WORK"' EXIT INT TERM

  if [ "$SKIP_TESTS" -eq 0 ]; then
    run_tests
  fi
  OUT="$ROOT/dist-release/$TAG"
  rm -rf "$OUT"
  mkdir -p "$OUT"
  build_tools
  build_and_package
  write_manifest
  if [ -n "$NOTES" ]; then
    write_notes "$NOTES" || die "could not write the release notes to $NOTES"
    say "release notes $NOTES"
  fi
  say "done: packages and manifest in $OUT (nothing was uploaded and no tag was created)"
}

main "$@"

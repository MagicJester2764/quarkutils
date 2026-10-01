#!/bin/sh
# Keep rust-std-patches/ equal to what the std fork carries.
#
#     ./tools/std-patches.sh check [path/to/fork]   fail if the two differ
#     ./tools/std-patches.sh sync  [path/to/fork]   rewrite the mirror from the fork
#
# The fork — a branch of rust-lang/rust, `../rust` by default — is the truth:
# it is what `hello` and `httpget` are compiled against. `rust-std-patches/` is
# a copy of exactly what that branch adds to upstream, kept here because the
# platform layer it holds is written against `quark-rt` and has to change with
# it, and because a checkout of the whole of rust-lang/rust is a lot to ask of
# somebody who wants to read three hundred lines of `thread.rs`.
#
# A copy with nothing checking it against the original goes stale, and this one
# did: it was the seed the fork was made from, and ten of its sixteen files had
# since diverged while its notes still described a plan. So it is generated,
# never edited, and `make` runs `check` whenever the fork is on disk.
#
# The mirror is three things:
#
#   BASE              the upstream commit the fork left from
#   upstream.patch    what the fork changes in files upstream already has
#   library/...       the files the fork adds, as they are there
#
# `check` also holds the compiler to BASE. The fork's `library/` only compiles
# with the rustc built from the commit it is based on, so the toolchain pin has
# to be that commit — and that used to be a thing to remember.
set -e
cd "$(dirname "$0")/.."

MODE=${1:?usage: std-patches.sh check|sync [fork]}
FORK=${2:-../rust}
DIR=rust-std-patches

if [ ! -d "$FORK/.git" ] && [ ! -f "$FORK/.git" ]; then
    echo "std-patches: no fork at $FORK" >&2
    exit 1
fi

# Where the fork left upstream. Its own `main` follows upstream's, so the
# merge base with that is the answer; a fresh clone has it only as a remote
# branch.
base_of_fork() {
    for ref in main origin/main upstream/main upstream/master; do
        if b=$(git -C "$FORK" merge-base HEAD "$ref" 2>/dev/null); then
            echo "$b"
            return 0
        fi
    done
    return 1
}

# Write what the fork carries on top of $1 into the directory $2.
generate() {
    base=$1
    out=$2
    # Added and modified are all a mirror made of files and one patch can say.
    other=$(git -C "$FORK" diff --name-status "$base" HEAD | awk '$1 != "A" && $1 != "M"')
    if [ -n "$other" ]; then
        echo "std-patches: the fork deletes or renames upstream files, which this mirror cannot say:" >&2
        echo "$other" | sed 's/^/  /' >&2
        return 1
    fi
    mkdir -p "$out"
    echo "$base" > "$out/BASE"
    # --full-index, or the abbreviated object names in the patch change length
    # with the size of whichever clone wrote it.
    git -C "$FORK" diff --no-color --full-index --diff-filter=M "$base" HEAD > "$out/upstream.patch"
    git -C "$FORK" diff --name-only --diff-filter=A "$base" HEAD | while read -r f; do
        mkdir -p "$out/$(dirname "$f")"
        git -C "$FORK" show "HEAD:$f" > "$out/$f"
    done
}

case $MODE in
sync)
    base=$(base_of_fork) || { echo "std-patches: cannot tell where $FORK left upstream" >&2; exit 1; }
    T=$(mktemp -d)
    trap 'rm -rf "$T"' EXIT
    generate "$base" "$T/new"
    # Everything but the README, which is written by hand and says what the
    # rest is.
    rm -rf "$DIR/library" "$DIR/upstream.patch" "$DIR/BASE"
    mkdir -p "$DIR"
    cp -r "$T/new/." "$DIR/"
    echo "std-patches: $DIR is the fork at $(git -C "$FORK" rev-parse --short HEAD), on $(echo "$base" | cut -c1-9)"
    echo "             $(find "$DIR/library" -type f | wc -l) files added, $(grep -c '^diff --git' "$DIR/upstream.patch") changed"
    ;;
check)
    fail=0
    if [ ! -f "$DIR/BASE" ]; then
        echo "std-patches: $DIR/BASE is missing — run tools/std-patches.sh sync" >&2
        exit 1
    fi
    base=$(cat "$DIR/BASE")
    if ! git -C "$FORK" merge-base --is-ancestor "$base" HEAD 2>/dev/null; then
        echo "std-patches: the fork is not based on $base, which is what $DIR/BASE says" >&2
        echo "             (rebased? run tools/std-patches.sh sync, and move the toolchain pin with it)" >&2
        exit 1
    fi
    T=$(mktemp -d)
    trap 'rm -rf "$T"' EXIT
    generate "$base" "$T/new"
    if d=$(diff -r -q -x README.md "$T/new" "$DIR" 2>&1); then
        echo "std-patches: the mirror is the fork ($(find "$DIR/library" -type f | wc -l) files added, $(grep -c '^diff --git' "$DIR/upstream.patch") changed, on $(echo "$base" | cut -c1-9))"
    else
        echo "std-patches: $DIR is not what the fork at $FORK carries:" >&2
        echo "$d" | sed -e "s|$T/new|the fork|g" -e 's/^/  /' >&2
        echo "             run tools/std-patches.sh sync and commit the result" >&2
        fail=1
    fi

    # The compiler has to be the one built from that commit.
    if v=$(rustc --version 2>/dev/null); then
        have=$(echo "$v" | sed -nE 's/.*\(([0-9a-f]+) .*/\1/p')
        case $base in
        "$have"*)
            echo "std-patches: rustc is built from the fork's base ($have)"
            ;;
        *)
            echo "std-patches: rustc is built from ${have:-an unknown commit}, and the fork is based on $(echo "$base" | cut -c1-9)" >&2
            echo "             the pin in rust-toolchain.toml has to be the fork's base: std will not compile otherwise" >&2
            fail=1
            ;;
        esac
    else
        echo "std-patches: no rustc here — the toolchain pin was not checked"
    fi
    exit $fail
    ;;
*)
    echo "usage: std-patches.sh check|sync [fork]" >&2
    exit 2
    ;;
esac

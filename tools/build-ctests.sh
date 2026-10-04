#!/bin/sh
# Build the C library's own tests: small programs, each checking a piece of
# the platform a C program leans on.
#
#     tools/build-ctests.sh <outdir>
#
# They exist because every program ported so far has found a lie in the C
# library that nothing else had noticed — fcntl answering 0 to everything,
# close doing nothing, thread-locals landing outside their block, a pid that
# was somebody else's a moment later — and each was caught only because a real
# program tripped on it. A test per lie keeps it caught.
#
# Needs the musl compilers on PATH: `x86_64-quark-musl-gcc` and, for the one
# C++ test, `-g++`. <outdir> gets the programs and the lists `runtests` reads
# (`runtests /etc/libc.tests`), flat; a distribution that wants them in an
# image puts the programs in /usr/bin and the lists in /etc.
#
# A test that needs a library of the C library's own says so on its first
# line, `@OUT@` standing for <outdir>:
#     // LINK: -lutil
#
# A file named lib*.c is a shared library, built first: lib*.so in <outdir>,
# for a test linked with -dynamic to be linked to or to open. A
# distribution puts them where libraries are, /usr/lib.
set -e
HERE=$(cd "$(dirname "$0")/.." && pwd)
OUT=${1:?usage: build-ctests.sh <outdir>}
mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
for src in "$HERE"/ctests/lib*.c; do
    [ -f "$src" ] || continue
    name=$(basename "$src" .c)
    echo "==> $name.so"
    x86_64-quark-musl-gcc -O2 -fPIC -shared -Wl,--strip-debug -o "$OUT/$name.so" "$src"
done
for src in "$HERE"/ctests/*.c "$HERE"/ctests/*.cpp; do
    [ -f "$src" ] || continue
    case $src in
    */lib*.c) continue ;;
    *.cpp) name=$(basename "$src" .cpp); cc=x86_64-quark-musl-g++ ;;
    *)     name=$(basename "$src" .c);   cc=x86_64-quark-musl-gcc ;;
    esac
    flags=$(sed -n '1s|^// LINK: ||p' "$src" | sed "s|@OUT@|$OUT|g")
    echo "==> $name"
    # --strip-debug and not -s: the symbol table is what turns a faulting rip
    # into a function name.
    # shellcheck disable=SC2086
    $cc -O2 -Wl,--strip-debug -o "$OUT/$name" "$src" $flags
done
cp "$HERE"/ctests/*.tests "$OUT"/
echo "built into $OUT"

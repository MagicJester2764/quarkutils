#!/bin/sh
# The system call numbers this tree was written against, checked.
#
#     ./tools/check-abi.sh [path/to/installed/quark/abi.h]
#
# The kernel is another repository, and this one carries its own copy of the
# numbers — twice, in fact: `quark-rt/src/syscall.rs` for Rust and
# `libc/include/quark/syscall.h` for C. A mismatch is silent and catastrophic:
# a program calls one number and the kernel runs another.
#
# Two halves. The first needs nothing but this tree: the two copies agree with
# each other, and no number is used twice. The second needs the header the
# kernel's `make install` writes, and is the one that matters across the two
# repositories: the runtime's table is the kernel's, call for call. With no
# header to compare against it is skipped, and says so, so that this tree
# builds on a machine with no kernel on it — unless REQUIRE_ABI is set, which
# is how a build that has both makes sure the comparison was not skipped.
set -e
cd "$(dirname "$0")/.."

INSTALLED=$1
RT=quark-rt/src/syscall.rs
H=libc/include/quark/syscall.h

T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT

# "number name", sorted by number then name.
grep -E '^pub const SYS_[A-Z_0-9]+: u64 = [0-9]+;' "$RT" \
  | sed -E 's/.*(SYS_[A-Z_0-9]+): u64 = ([0-9]+);/\2 \1/' \
  | sort -k1,1n -k2,2 > "$T/rt"
grep -E '^#define SYS_[A-Z_0-9]+[[:space:]]+[0-9]+' "$H" \
  | sed -E 's/#define[[:space:]]+(SYS_[A-Z_0-9]+)[[:space:]]+([0-9]+).*/\2 \1/' \
  | sort -k1,1n -k2,2 > "$T/h"

fail=0

# Two names on one number: one of them is not the call it thinks it is.
dup=$(awk '{print $1}' "$T/rt" | uniq -d)
if [ -n "$dup" ]; then
    echo "abi: two calls share a number in $RT:" >&2
    for n in $dup; do
        echo "  $n: $(awk -v n="$n" '$1 == n {printf "%s ", $2}' "$T/rt")" >&2
    done
    fail=1
else
    echo "abi: quark-rt uses every number once ($(wc -l < "$T/rt") calls)"
fi

# The C header declares a subset — what C programs and the Linux layer call —
# so it is checked for disagreement rather than for completeness.
wrong=$(awk 'NR == FNR { rt[$2] = $1; next }
             !($2 in rt)   { print "  " $2 ": in the C header and not in quark-rt"; next }
             rt[$2] != $1  { print "  " $2 ": quark-rt " rt[$2] ", C header " $1 }' "$T/rt" "$T/h")
if [ -n "$wrong" ]; then
    echo "abi: $H disagrees with quark-rt:" >&2
    echo "$wrong" >&2
    fail=1
else
    echo "abi: the C header agrees ($(wc -l < "$T/h") numbers)"
fi

# And the kernel's own table, if the kernel has been installed somewhere.
if [ -z "$INSTALLED" ]; then
    if [ -n "$REQUIRE_ABI" ]; then
        echo "abi: no installed kernel ABI to compare against, and one is required" >&2
        echo "     (install the kernel into the same DESTDIR first, or pass QUARK_ABI=)" >&2
        fail=1
    else
        echo "abi: no installed kernel ABI given — not compared against a kernel"
    fi
elif [ ! -f "$INSTALLED" ]; then
    echo "abi: $INSTALLED is not there" >&2
    fail=1
else
    grep -E '^#define SYS_[A-Z_0-9]+[[:space:]]+[0-9]+' "$INSTALLED" \
      | sed -E 's/#define[[:space:]]+(SYS_[A-Z_0-9]+)[[:space:]]+([0-9]+).*/\2 \1/' \
      | sort -k1,1n -k2,2 > "$T/k"
    if d=$(diff "$T/k" "$T/rt"); then
        echo "abi: quark-rt and the installed kernel agree ($(wc -l < "$T/k") calls)"
    else
        echo "abi: MISMATCH between quark-rt and the kernel's installed ABI ($INSTALLED):" >&2
        echo "$d" | sed -n 's/^< \(.*\)/  the kernel has, quark-rt has not: \1/p; s/^> \(.*\)/  quark-rt has, the kernel has not: \1/p' >&2
        fail=1
    fi
fi

exit $fail

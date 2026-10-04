# The network's protocol

The contract between the network stack and its clients. `net` is the server
(`net/src/sockets.rs` has its copy of the numbers below); `quark-rt`'s
`socket` module and the C library are clients, each with a copy of its own.
This document is the one they are checked against.

A client finds the server by looking up `net` with the nameserver, which
also gives it the right to call it — which it needs to make a socket, and
for nothing else: once a socket is a descriptor, the descriptor is the
permission, as it is for a pipe or a file. The stack is started again if it
fails (`docs/services.md`), so a client that remembers which task it is
asks again when a call to it fails; its sockets went with the old one.

## A socket is a descriptor

A socket is a *served descriptor* (the kernel's `docs/abi.md`): the stack
makes it with `SYS_FD_SERVE` and the flag that says it will say when it is
ready, and names it by a cookie of its own. So:

- **Reading and writing it** are `SYS_FD_READ` and `SYS_FD_WRITE`, and their
  forms that do not wait, as for any descriptor. The kernel calls the stack
  for the task (`TAG_FD_READ`, `TAG_FD_WRITE`) and lends it the task's
  buffer — up to what the kernel lends at once — and says whether the task
  may wait. A stream reads what has come, or the end; a datagram socket the
  next datagram from the one it is connected to, cut to the room there is.
- **A poll** answers what the stack last said: readable, writable, hung up
  (`SYS_FD_READY`). A listener is readable while a connection waits to be
  accepted; a stream being connected is nothing until it is connected or
  has failed, and then writable (and readable and hung up, if it failed).
- **It is shared** as any descriptor is: `dup`, `fork`, passing it over a
  local socket. The socket goes when the last descriptor for it closes; the
  kernel tells the stack, which collects it with `SYS_FD_REAP`. A stream
  that had nothing unread says goodbye first, and one that had is reset, as
  Linux does.
- **Everything else** is a request, `TAG_SOCKET` (20), naming the socket by
  its cookie — which a client learns with `SYS_FD_SERVED` — and believed
  only of a task whose program holds a descriptor for it (`SYS_FD_HOLDS`).

## Requests

`data[0]` is the operation, with flags above it: `op | flags << 8`. Flag 1:
the caller is not to wait — what would wait is refused with `EAGAIN`, or
for a connection `EINPROGRESS`. Flag 2: a receive only looks, and leaves
what it saw to be received.

An address is three words: `family << 16 | port`, then the sixteen bytes of
the address as they are on the wire — IPv4's four first, and nought after —
read as two little-endian words. A socket of IPv6's family (10) reaches
IPv4 too, as `::ffff:a.b.c.d`, and says IPv4's addresses that way; one of
IPv4's (2) is given IPv4's only. An address of the other family is refused
with `EAFNOSUPPORT` — but for IPv4's sent to, or connected to, by a datagram
socket of IPv6's that has not asked for IPv6 alone, which is IPv4 in IPv6's
clothes, as Linux has it.

A reply's tag is `0`, or `u64::MAX` with Linux's errno in `data[0]`.

| Op | Request | Reply |
|---|---|---|
| 0 create | `[0, family, type, protocol]`: 2 or 10; 1 a stream, 2 datagrams (the low four bits; the rest is the C library's); 0, 6 or 17 | `[descriptor]` |
| 1 bind | `[1, cookie, address…]`; port 0 for one the stack chooses | — |
| 2 listen | `[2, cookie, backlog]`, 1 to 16; again, to change it | — |
| 3 connect | `[3, cookie, address…]`; waits until connected, refused or 60 s unanswered. A datagram socket's correspondent; family 0 for none | — |
| 4 accept | `[4, cookie]`; waits for a connection | `[descriptor, address…]` of the other end |
| 5 send to | `[5, cookie, address…, length]`, the data lent; family 0 for the correspondent. A stream's is its write | `[sent]` |
| 6 receive from | `[6, cookie, room]`, room lent; waits for something | `[received, address…, whole length]` — a datagram's length, which may be more than was received |
| 7 shut down | `[7, cookie, how]`: 0 reading, 1 writing (the other end is told), 2 both | — |
| 8 name | `[8, cookie, which]`: 0 where it is, 1 who is at the other end | `[address…]` |
| 9 option | `[9, cookie, option, value, set]`: set 1 to change it | `[value]` |

Options: 1 the error not yet said, which asking takes (`SO_ERROR`); 2 keep
alive; 3 send small writes at once (`TCP_NODELAY`); 4 listening; 5 the
type; 6 IPv6 alone (`IPV6_V6ONLY`, before it is bound); 7 and 8 the
receive and send buffers, which are what they are and say so; 10, to be
asked, what a read would find: a stream's bytes, or the next datagram's
length (`FIONREAD`).

A request that waits is held, with its reply, and answered after the turn
of the stack's loop that brings what it waits for. A task in a call is in
one call: one that asks something else, or dies, is waiting for nothing.

## What goes wrong, and how it is said

- A connection refused is `ECONNREFUSED`; one nobody answered in a minute,
  `ETIMEDOUT`. Either is the connect's answer, or — for one that did not
  wait — what option 1 says after a poll has said it is done.
- A connection the other end reset, or that went unanswered with data in
  flight, is `ECONNRESET` once: to the next read when there is nothing left
  to read, and then the end. A read through the descriptor cannot carry an
  errno, so there it fails, and the error stays for option 1 to say which —
  which is how a C library sets `errno` for it.
- A write after the end — the other end gone, or this end shut down — is
  `EPIPE`.
- A datagram larger than the interface it leaves by can carry is
  `EMSGSIZE`: nothing is cut into fragments.
- A port somebody has is `EADDRINUSE`; an address this machine does not
  have, `EADDRNOTAVAIL`.

## Limits

256 sockets in the machine and 128 a program; a stream's buffers 64 KiB
each way, a datagram socket's 32 datagrams or 64 KiB; up to 16 connections
waiting to be accepted on one listener; a minute for a connection to be
answered, or for data to be acknowledged. Ports the stack chooses are
32768 to 49151.

## The resolver

DNS on 127.0.0.1:53 and [::1]:53, for this machine only: what musl asks
when `/etc/resolv.conf` names nobody, which is always. A question is asked
of the servers DHCP and routers' advertisements named, one after another,
two seconds each and six in all, and the answer is handed back as it came,
with the asker's id — a failure, `SERVFAIL`. An answer is kept for the
least of its TTLs; one that a name is not there, by its zone's SOA, and
without one not at all (RFC 2308); and handed out again with its TTLs what
is left of them: 256 answers, 64 questions waiting. An answer cut short
is handed back cut short and not kept: nothing is asked again over TCP.

`TAG_RESOLVER` (21) asks what it has done: `[asked, asked of a server,
answered from what was kept, failed]`. The old protocol's lookup
(`TAG_DNS_RESOLVE`) is a question of type A put to it.

## What it is doing, and what is let in

`TAG_STATUS` (22) writes what the stack is doing, as text, into a buffer
the caller lends for writing (`[its length]`): the card, its addresses,
the ways out and the DNS servers; `lo`'s addresses; the resolver's
counts; a line for each socket — what it is, where it is, where it goes
and what state it is in; and the filter. The answer is `[written, how long
it all was]`. Anybody may ask; `netctl` prints it.

The **filter** is a list of rules about what comes in, the first that
matches a packet deciding and a packet none matches let in. Every packet
the card brings and every packet `lo` carries is asked about before the
protocols see it, so a rule about a port is about a connection to it from
anywhere, this machine included. `TAG_FILTER` (23) changes it, for a
caller that offers the right to with the call (`SYS_CALL_OFFER` of a
`NetAdmin` capability, type 15) — taken to be looked at, and let go of
again; anybody else is refused (`EPERM`):

| Op | Request | |
|---|---|---|
| 0 add | `[0, rule, first << 16 \| last port, address…]` | at the end |
| 1 remove | `[1, n]` | rule `n`, counting from one |
| 2 clear | `[2]` | every rule |

A rule's word: 1, drop it (or else let it in); 2, from an address — the
two words after the ports, as an address is said above, IPv6's if 4 is
set too; the protocol in bits 8 to 15 (0 any, 1 ICMP of either family, 6
TCP, 17 UDP); and the bits of the address that must match in bits 16 to
23. Ports are this machine's, the ones a packet is for: all of them, 0 to
65535, for every port. The answer is `[how many rules there are]`; 64 at
most (`ENOSPC`), and a rule that is not one is `EINVAL`.

## Before sockets were descriptors

Programs built before this spoke to the stack by tags of their own: a
datagram sent or received (1, 2), the card's address (4), an echo (5), a
name (7), a stream connected, listened for, sent to, received from and
closed by a handle the task owned (10, 11, 13, 14, 15), and a descriptor
the kernel read and wrote forty bytes a call through (`FdKind::Socket`, 16
and 17). They still work, on the same stack — sixty-four streams in the
machine, thirty-two a task — and nothing written now should use them.

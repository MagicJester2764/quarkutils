# The network's protocol

The contract between the network stack and its clients. `net` is the server
(`net/src/sockets.rs` has its copy of the numbers below); `quark-rt`'s
`socket` module and the C library are clients, each with a copy of its own.
This document is the one they are checked against.

A client finds the server by looking up `net` with the nameserver, which
also gives it the right to call it — which it needs to make a socket, and
for nothing else: once a socket is a descriptor, the descriptor is the
permission, as it is for a pipe or a file.

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

## Before sockets were descriptors

Programs built before this spoke to the stack by tags of their own: a
datagram sent or received (1, 2), the card's address (4), an echo (5), a
name (7), a stream connected, listened for, sent to, received from and
closed by a handle the task owned (10, 11, 13, 14, 15), and a descriptor
the kernel read and wrote forty bytes a call through (`FdKind::Socket`, 16
and 17). They still work, on the same stack — sixty-four streams in the
machine, thirty-two a task — and nothing written now should use them.

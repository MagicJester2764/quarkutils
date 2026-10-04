# Services

`init` is the service manager. It starts the boot image's programs in the
order the machine needs them — the nameserver, the framebuffer, the console,
the device manager, the rest of the boot image, the file server — and then
whatever `/etc/init.conf` names, and from then on it keeps them: it knows
what each needs, says what each is doing, and answers `svc`
(`init/src/services.rs`; the protocol is `quark_rt::services`).

## A service

Each has a name, a program, the services it needs, the name it registers if
it is up only once it has registered, and what is done when it ends. Its
state is one of:

| State | |
|---|---|
| `up` | running, and registered if it registers |
| `starting` | running, and not yet registered under the name it is waited for by |
| `waiting` | not started: something it needs is not up |
| `restarting` | ended, and to be started again in a moment |
| `failed` | ended with a status that is not 0, and not started again |
| `done` | ended by itself with status 0, and not started again |

A service that registers is up once the nameserver says its first task has
the name — asked without being granted anything (`TAG_LOOKUP_TID`), every
twentieth of a second while it is starting. One that has not registered
after thirty seconds is said to be slow on the console, and is still waited
for. A service that registers from a thread other than the one it began as
is never seen to: it stays `starting`.

## When one ends

What is done is its policy: `always` started again, `on-failure` started
again if it ended with a status that is not 0 — a fault, a signal, a
failure it said — and `never`. It is started again a second after it
ended, and twice as long after each time it ends within a minute of
starting, up to a minute; the fifth such end in a row leaves it `failed`,
which `svc status` says. One that ran for a minute or more starts the count
again. Started again, it waits as anything not started does for what it
needs.

A client finds a service again by asking for it again. `quark_rt` looks a
service up each time it makes something of it; the C library keeps the
network stack's task and asks again when a call to it fails. What a client
had of the old one — a socket, a stream — went with it.

## The boot image's

| Service | Up once it has registered | Started again |
|---|---|---|
| `nameserver` | (at once) | never |
| `fb` | `fb` | never |
| `console` | `console` | never |
| `devmgr` | `devices` | never |
| `keyboard` | `keyboard` | never |
| `auth` | `auth` | on failure |
| `net` | `net` | on failure |
| `sound` | `sound` | on failure |
| `ramdisk` | (at once) | never |
| `input` | `input` | never |
| `vfs` | `vfs` | never |

What is never started again is what other programs hold a part of that a
new one would not have: the nameserver's capabilities, the console's pipe,
the devices, every session's standard input, every open file. `auth`, `net`
and `sound` are looked up by whoever wants them, each time; init keeps a
copy of their programs before it frees the boot image, since a root need
not carry one. init stays in the drivers' band to start them: a spawner can
give no better band than it is in, and `net` asks for the drivers'.

## `/etc/init.conf`

```text
service NAME [needs=A,B] [restart=always|on-failure|never] [register=X] PATH [ARGUMENT...]
start PATH [ARGUMENT...]
run PATH [ARGUMENT...]
session PATH
```

- `service` adds a service. It is started once every service it `needs` is
  up — a boot service or another line's, in any order — and waits for ever
  for a name nothing has, which `svc` says. `register=` is the name it is
  up once it has registered; without it, it is up when it is started.
  `restart=` is `on-failure` unless it says.
- `start` is a service named after its file, never started again and
  waited for by nothing: what the line always meant.
- `run` lines are run one after another, each to its end, before the
  session.
- `session` is what everybody logs in through: `getty`, for a system whose
  users are on terminals, which is started again if it fails. Without one
  it is `login` from `/usr/bin`, or the shell, which is not: on a console
  with no terminal, the end of the login is the end of the session. It is started once the `run` lines have run and every service that
  is starting has registered — or five seconds have passed: a line a service
  prints after the login prompt pushes the prompt off its line, and a
  network that never comes does not keep anybody from logging in for long.

A line that is none of these is a comment, a blank, or something from the
future. A service's arguments are at most six.

## `svc`

```text
svc                     every service: what it is doing, its process, how
                        many times it has been started, for how long it has run
svc status NAME         one, said in full
```

Anybody may ask (`quark_rt::services::table`, `state`, `describe`).

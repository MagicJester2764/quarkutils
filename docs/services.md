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
| `failed` | ended with a status that is not 0, and not started again |
| `done` | ended by itself with status 0, and not started again |

A service that registers is up once the nameserver says its first task has
the name — asked without being granted anything (`TAG_LOOKUP_TID`), every
twentieth of a second while it is starting. One that has not registered
after thirty seconds is said to be slow on the console, and is still waited
for. A service that registers from a thread other than the one it began as
is never seen to: it stays `starting`.

## The boot image's

| Service | Up once it has registered |
|---|---|
| `nameserver` | (at once) |
| `fb` | `fb` |
| `console` | `console` |
| `devmgr` | `devices` |
| `keyboard` | `keyboard` |
| `auth` | `auth` |
| `net` | `net` |
| `sound` | `sound` |
| `ramdisk` | (at once) |
| `input` | `input` |
| `vfs` | `vfs` |

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
  users are on terminals. Without one it is `login` from `/usr/bin`, or the
  shell. It is started once the `run` lines have run and every service that
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

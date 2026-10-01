# Wayland on Quark

`wm` is a Wayland compositor: the actual protocol and the actual wire format,
so that a client built for Linux and never modified runs here. Upstream
libwayland, weston's `weston-simple-shm` and `weston-terminal`, and GTK 4 all
do, unpatched.

This says what the compositor implements, the rules it keeps, and what it does
not do. The code is `wm/src`, one module per part; the wire format both halves
agree on is `quark-rt/src/wl`.

## Why the real thing

A protocol shaped like Wayland but carried over Quark's own IPC was designed in
full first, and rejected. It would have been smaller and it would have worked.
What it could not buy at any price is a client somebody else wrote — and
without that, every program that ever draws on this system has to be written
for this system.

Writing that draft was still worth it, because it exposed what the objection to
real Wayland actually was. Wayland assumes four things:

1. a bidirectional, ordered byte stream between two processes;
2. descriptor passing, so a message can carry a handle to an object;
3. memory addressable as a descriptor, which both sides map;
4. `poll`, so one task can wait on several descriptors.

Quark had none of them, and the draft's whole transport — a shared-memory ring,
a futex wake, an endpoint granted sideways so that a server could push — was
machinery invented to work around exactly that absence. So the kernel grew the
four instead: `SYS_SOCKETPAIR`, `SYS_FD_SEND` and `SYS_FD_RECV`,
`SYS_MEMFD_CREATE`, and `SYS_POLL` with poll sets. They are what every
Unix-shaped system has, and they were worth having whether or not anything was
ever drawn.

## Running it

```
wm "<program> [args]" ...
```

`wm` takes the display from whoever has it, starts up to four programs, and
gives the display back when the last of them has gone or when Escape is
pressed. It is a program a user runs, not something the system is built
around: the machine boots into the text console, and `wm` is a client of the
framebuffer device like the console is.

Each program is given:

- one end of a socketpair as descriptor 3, and `WAYLAND_SOCKET=3` in its
  environment — the first thing `wl_display_connect` looks at, which is the
  whole reason libwayland needs no patch and Quark needs no socket files;
- `WM_SESSION=n`, saying which of the session's programs it is;
- the rest of the compositor's own environment;
- the compositor's standard output and error, so that what it prints goes to
  the console underneath. Not standard input: a program in a session takes its
  keys from the compositor.

A program is granted what its own manifest asks for, out of what the
compositor holds.

## What the compositor is

Six things have to happen for a window to be on a screen:

1. Own the screen — mode, pixel format, and the memory that is scanned out.
2. Give each application somewhere to draw.
3. Decide where each buffer goes, and in what stacking order.
4. Composite the visible parts into the screen.
5. Route input to whichever client it belongs to.
6. Furniture and policy: title bars, focus, dragging, closing.

**Job 1 is not Wayland's and is not the compositor's.** `fb` owns the
framebuffer the way `/dev/fb0` does and lends the display by capability;
clients never see it, and the compositor is one of its claimants.

X11 split jobs 3 and 6 into a window-manager process separate from the server
doing 1, 4 and 5, and the two disagreed in the gap between them. Wayland
collapsed them, so "compositor" and "window manager" name one program.

## Interfaces

Versions are what the compositor advertises; a client binds no higher than the
minimum of what it supports and what it is offered. **A version is advertised
only when every event of it is sent.**

| Global | Version | Why that one |
|---|---|---|
| `wl_compositor` | 4 | The buffer transform, the buffer scale and `damage_buffer` are read and checked. weston's toytoolkit binds 3 with no negotiation, so a compositor offering less is one every weston client dies against. |
| `wl_shm` | 1 | `ARGB8888` and `XRGB8888`. |
| `wl_output` | 2 | One output; `geometry`, `mode`, `scale` and `done`. |
| `wl_seat` | 5 | A keyboard and a pointer. 5 is `wl_pointer.frame` and the axis events that go with it. |
| `xdg_wm_base` | 1 | Toplevels. Popups and positioners are refused. |
| `zxdg_decoration_manager_v1` | 1 | Always answers server-side. |
| `wl_data_device_manager` | 1 | The clipboard. 2 and 3 are drag and drop. |
| `zwp_primary_selection_device_manager_v1` | 1 | What the middle button pastes. |

Objects made from those: `wl_registry`, `wl_callback`, `wl_surface`,
`wl_region`, `wl_shm_pool`, `wl_buffer`, `wl_keyboard`, `wl_pointer`,
`xdg_surface`, `xdg_toplevel`, the toplevel decoration, and the data device,
source and offer of each selection. An object made from another inherits its
version, which is how a client that bound `wl_seat` at 4 gets a `wl_pointer`
with no `frame`.

`wl_region` exists so that clients may name it; its requests do nothing,
because this compositor composites and routes the same either way.
`wl_pointer.set_cursor` is read and checked and then let go: the compositor
draws the pointer itself.

**Not implemented, and each for a reason:**

- `xdg_popup`, `xdg_positioner` — menus. `weston-terminal`'s are the one thing
  of its it cannot show.
- drag and drop — `wl_data_device_manager` 2 and 3. It needs a pointer grab
  that follows a surface a client supplies.
- `wl_touch` — no touch hardware, and nothing to test against.
- `wl_subcompositor`, `wl_subsurface` — a client-side optimisation for
  compositing video or GL under a widget tree.
- `wl_shell`, `wl_shell_surface` — deprecated in favour of `xdg-shell`.
- output transforms, fractional scaling, `wl_drm`, `linux-dmabuf` — one output,
  one scale, no rotation, no GPU.
- `wl_shm_pool.resize` — see "What is missing".

## Rules the compositor keeps

These are the parts of Wayland a compositor gets wrong quietly, where the
client is correct and the picture is not.

**A request is read inside the request.** Every argument comes through a cursor
bounded by the size in the message's own header. Anything that cannot be
honoured — an opcode the interface does not have, an object that is not there
or is not what the request needs, a string that does not end in a NUL, a `bind`
above the version advertised — is a `wl_display.error` naming the object and
the reason, and then the connection ends. The compositor also prints why,
because libwayland hands the reason to the program and most programs exit
without repeating it.

**A stream is not a message.** A read delivers whatever was in the buffer,
which may be half of one request or three and a bit. Each connection keeps the
bytes that have arrived and do not yet make a message.

**A client cannot name another client's objects.** There is one id table per
client, and the id a client sends is only ever looked up in its own.

**A surface has no meaning until it is given a role.** A bare `wl_surface` is
never displayed; `xdg_surface.get_toplevel` makes it a window. Giving a surface
a second role is a protocol error.

**Surface state is double-buffered and applied atomically.** `attach`,
`damage`, `frame` and the rest change nothing a viewer could see — they
accumulate pending state, and `commit` applies all of it at once. A half-drawn
frame is not unlikely here; it is unrepresentable.

**A buffer is lent, and must be released.** `wl_buffer.release` says the
compositor has finished reading. A client with two buffers draws into one while
the compositor reads the other; a client with one waits for the release. A
buffer destroyed while it is being shown becomes a zombie, and its pool stays
mapped until nothing shows it.

**Every buffer is checked against its pool before anything reads a pixel.** The
numbers in `create_buffer` are the client's arithmetic, and the compositor is
the thing that would fault.

**A slot is not freed while an object still names it.** `xdg_toplevel.destroy`
takes the role away and leaves the surface, because the client's `wl_surface`
still names it. A surface slot freed under a live name is a slot the next
client's surface takes, with the first client still able to attach to it.

**Frame callbacks are the throttle.** A callback asked for with a commit is
answered on the compositor's next pass rather than at once, so a client that
draws only when its callback fires never draws faster than the compositor
comes round. A callback is never dropped: one that is never sent is a client
that never draws again.

**A size is agreed, not imposed.** The compositor never resizes a window
itself. It sends `xdg_toplevel.configure` with a size and the states, then
`xdg_surface.configure` with a serial, and the window follows whatever buffer
the client attaches. The two go together — one without the other leaves a
client waiting for a serial that never comes — and a surface accepts any serial
from the oldest unanswered one up to the newest sent, because a resize sends
one per tick and answering one supersedes the older ones.

**Keyboard focus and pointer focus are separate.** Keyboard focus is one
surface, changed by policy. Pointer focus is whatever is under the cursor.
Moving across an unfocused window still sends it `enter` and `motion`, which is
how hover works without stealing focus. A change of keyboard focus is `leave`
on the old surface *then* `enter` on the new, so no client can believe it holds
focus twice.

**The implicit grab.** While a button is held, pointer events keep going to the
surface where the press happened, even after the cursor leaves it.

**Between a press on the compositor's own furniture and the release that ends
it, the pointer is the compositor's.** A press on the title bar moves the
window, one near an edge or corner resizes it, one on the close box asks the
client to go, and two on the bar within half a second fill the screen. No
client hears a motion while that goes on. `xdg_toplevel.move` and `.resize`
start the same grabs for a client that draws its own decorations, and are
refused unless a button is actually down.

**Decorations are server-side.** The title bar, its drag region and its close
box belong to the compositor, and `xdg-decoration` says so to a client that
asks. Wayland's default is the opposite; this inverts it because a client here
may be a hundred and forty lines and should not have to reimplement a title bar
to have one.

**The selection follows keyboard focus, and the compositor never sees the
data.** A client offering a selection hands over a source and the MIME types it
can produce; a client taking it hands back a pipe, and the compositor passes
that pipe to the source. A client that never has focus can never read the
clipboard.

**The keymap is said once.** `wl_keyboard.keymap` carries an `XKB_V1` US
layout in a memory descriptor, generated with `xkbcomp` rather than written by
hand, and `repeat_info` says 25 a second after 400 ms. Repeating is the
client's to do.

**The wheel is version 5's.** A detent arrives as `axis_source` (wheel),
`axis_discrete` (the click count), `axis` (ten units per detent, as Weston
sends) and a `frame`.

**Compositor policy, and nowhere else.** Escape ends the session and Tab cycles
focus; neither reaches a client, and nor does the release of either. A click
focuses and raises the window under it.

**What the compositor has, each client has a share of.** Four clients; each may
hold a quarter of the surfaces (16), pools (16) and buffers (64), and 64
objects of its own. A client asking for them in a loop is a client, not a
compositor.

**No client can hold up the others.** Writes to a client are non-blocking. An
event that will not fit in the four kilobytes waiting for a client that has
stopped reading is not sent, and the compositor goes on to the next client.

## How it waits

The compositor's work comes from three places — IPC from the framebuffer
device and the input server, streams from Wayland clients, and the clock — and
the kernel has no single wait that covers IPC and descriptors together. So the
loop receives with a one-tick timeout and, each time round, asks every
connection whether it has anything (`SYS_POLL` with no wait) and reads the
keyboard and pointer from `input`. That is a hundred passes a second on an
idle screen, and it is the first thing to change if a wait that covers both
ever exists.

## The older protocol

Before any of this, a window was seven IPC requests: create, commit, move,
focus, the screen's size, destroy, and a poll for events. `quark_rt::wm` is
the client half, and `wmdemo` and `wmtype` use it. It still works beside
Wayland — the compositor composites both kinds of window — and nothing new
should be written against it.

## What is missing

- **`wl_shm_pool.resize` is refused.** A pool may only grow, and growing means
  new memory, which means a descriptor the request does not carry; a client
  that drew past the old end would fault the compositor. A client that needs a
  bigger pool makes a new one. Toolkits do call `resize`, so this is a real
  gap rather than a preference.
- **Serials are not remembered.** `xdg_toplevel.move`, `.resize` and
  `set_selection` cannot check that the serial they are given was a recent
  press; what `move` and `resize` check instead is that a button is down.
- **Popups.** A menu is a surface with a role this compositor refuses.
- **Fullscreen and minimise** are read and ignored. Maximise works.
- **Focus has little policy.** Tab cycles, a new window takes it, and a click
  raises the window under the pointer. There is no follow-mouse and no focus
  stealing prevention.
- **A client's cursor is not drawn.** The pointer is always the compositor's
  own arrow.
- **A client's damage is a yes or a no.** A commit that damaged anything
  repaints its window's part of the screen, whichever part of the buffer the
  client said had changed.

## Testing it

`dtest` checks the wire format against bytes libwayland actually sent. The
rest is done from outside, by clients built with the cross toolchain whose
sources are in ExplOSion's `toolchain/`: `wlprobe` binds every global and
prints what it was told, `wlfuzz` sends malformed requests and expects to be
disconnected with a reason each time, and `wlclip` and `wlscroll` check the
selections and the wheel.

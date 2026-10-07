#![no_std]
#![no_main]

use quark_rt::ipc::Message;
use quark_rt::nameserver;
use quark_rt::spawn::{self, Scratch, Spawned};
use quark_rt::{args, print, println, syscall, vfs};
use quark_rt::stdio::{read_line_event, Line};

mod words;

use quark_rt::manifest::CapReq;

// No physical range: the shell only maps frames it allocated itself to stage a
// child, and frame ownership authorises those. It runs arbitrary user code, so
// it is exactly the task that should not hold one. The ports are ACPI poweroff.
quark_rt::manifest!([
    CapReq::task_mgmt(0),
    CapReq::phys_alloc(64),
    CapReq::ioport(0x604, 0x604),
    CapReq::ioport(0xB004, 0xB004),
    // The reset control register: `shutdown -r`.
    CapReq::ioport(0xCF9, 0xCF9),
    // The right to say who a task is, where the session holds it — root's
    // does — for the programs root runs that say it.
    CapReq::set_uid(),
    // And the right to set the clock, for `date -s`.
    CapReq::clock(),
    // And to turn the machine off, for `shutdown`.
    CapReq::power(),
    // And to say how the network treats what comes in, for `netctl`.
    CapReq::net_admin(),
    // And to run a thread in a real-time class, for a program that asks to.
    CapReq::realtime(),
]);

const TAG_SET_FOREGROUND: u64 = 2;

// Shell temp address ranges (non-overlapping with init's 0x82-0x88)
const FILE_BUF_BASE: usize = 0x90_0000_0000;
// Staging areas for quark_rt::spawn, in this task's own address space.
const ELF_TEMP: usize = 0x91_0000_0000;
const STACK_TEMP: usize = 0x92_0000_0000;
const ARGS_TEMP_PAGE: usize = 0x93_0000_0000;

/// Staging areas quark_rt::spawn maps through while building a child.
const SPAWN_SCRATCH: Scratch = Scratch {
    elf: ELF_TEMP,
    stack: STACK_TEMP,
    args: ARGS_TEMP_PAGE,
};

// ---------------------------------------------------------------------------
// ELF loader (mirrors init's load_elf using shell temp addresses)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Program arguments
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Capability granting for child tasks
// ---------------------------------------------------------------------------

fn eq_ignore_case(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    for i in 0..a.len() {
        let ca = if a[i] >= b'a' && a[i] <= b'z' { a[i] - 32 } else { a[i] };
        let cb = if b[i] >= b'a' && b[i] <= b'z' { b[i] - 32 } else { b[i] };
        if ca != cb {
            return false;
        }
    }
    true
}

/// Grant a child what its manifest asks for, from what the shell itself holds.
///
/// This was a name match too — `cat`, `disktest`, `httpget`, `shutdown` — so a
/// program the shell had not heard of got nothing, whatever it needed. The
/// shell now reads the request out of the image and mints what it can.
///
/// It deliberately holds no PhysRange, so a child asking for one is refused:
/// the shell runs arbitrary user code and cannot hand out authority it was
/// never given. That refusal is the kernel's, not a check here.
/// What every program the shell starts is told about the world.
///
/// Fixed, because there is no `export` and nothing sets a variable at run time.
/// It exists because software ported here expects these to be answerable — and
/// because a Wayland client is handed its connection through this, in
/// `WAYLAND_SOCKET`.
const BASE_ENV: [&[u8]; 4] = [
    b"HOME=/home/root",
    b"PATH=/usr/bin",
    b"TERM=quark",
    b"USER=root",
];

fn grant_caps_from_manifest(image: &[u8], tid: usize) {
    // Every child inherits the shell's IPC reach, so it can find and call the
    // services. Delegated rather than minted: the shell cannot read back the
    // 64-bit destination set to re-mint it.
    let _ = syscall::sys_cap_grant(tid, syscall::SLOT_ENDPOINT, syscall::SLOT_ENDPOINT);

    quark_rt::manifest::grant_image(tid, image, 12);
}

fn build_path(cmd: &[u8], path_buf: &mut [u8; 64]) -> usize {
    let has_slash = cmd.iter().any(|&b| b == b'/');

    if has_slash {
        // Absolute/relative path — copy directly
        let len = cmd.len().min(64);
        path_buf[..len].copy_from_slice(&cmd[..len]);
        len
    } else {
        // Bare command — prepend /usr/bin/, lowercase name (ext2 format)
        let prefix = b"/usr/bin/";
        let cmd_len = cmd.len().min(64 - prefix.len());
        path_buf[..prefix.len()].copy_from_slice(prefix);
        let mut pos = prefix.len();
        for i in 0..cmd_len {
            path_buf[pos] = if cmd[i] >= b'A' && cmd[i] <= b'Z' {
                cmd[i] + 32
            } else {
                cmd[i]
            };
            pos += 1;
        }
        pos
    }
}

fn ends_with_elf(path: &[u8]) -> bool {
    path.len() >= 4 && eq_ignore_case(&path[path.len() - 4..], b".elf")
}

// ---------------------------------------------------------------------------
// Command execution
// ---------------------------------------------------------------------------

fn set_foreground(input_tid: usize, child_tid: usize) {
    let msg = Message {
        sender: 0,
        tag: TAG_SET_FOREGROUND,
        data: [child_tid as u64, 0, 0, 0, 0, 0],
    };
    let mut reply = Message::empty();
    let _ = syscall::sys_call(input_tid, &msg, &mut reply);
}

/// Load a command and prepare it to run, without starting it.
///
/// Separated from running so a pipeline can create every stage, wire the pipes
/// between them, and only then start them — a stage started before its reader
/// exists would write into a pipe with no reader.
/// Find which spelling of a command exists: the path as given, then — for a
/// bare name — the uppercase `.ELF` a FAT32 root uses, or — for a path — the
/// same path with `.ELF` added. Returns the length written to `out`.
fn resolve_program(cmd: &[u8], vfs_tid: usize, out: &mut [u8; 64]) -> Option<usize> {
    let exists = |p: &[u8]| match vfs::open(vfs_tid, p) {
        Ok((h, _, _)) => {
            let _ = vfs::close(vfs_tid, h);
            true
        }
        Err(_) => false,
    };

    let pos = build_path(cmd, out);
    if exists(&out[..pos]) {
        return Some(pos);
    }

    let has_slash = cmd.iter().any(|&b| b == b'/');
    if !has_slash {
        // Tried lowercase (ext2); now uppercase with .ELF (FAT32).
        let prefix = b"/usr/bin/";
        let suffix = b".ELF";
        let cmd_len = cmd.len().min(64 - prefix.len() - suffix.len());
        let mut fat = [0u8; 64];
        fat[..prefix.len()].copy_from_slice(prefix);
        let mut p = prefix.len();
        for &c in &cmd[..cmd_len] {
            fat[p] = c.to_ascii_uppercase();
            p += 1;
        }
        fat[p..p + suffix.len()].copy_from_slice(suffix);
        p += suffix.len();
        if exists(&fat[..p]) {
            *out = fat;
            return Some(p);
        }
    } else if !ends_with_elf(&out[..pos]) && pos + 4 <= 64 {
        out[pos..pos + 4].copy_from_slice(b".ELF");
        if exists(&out[..pos + 4]) {
            return Some(pos + 4);
        }
    }
    None
}

fn cmd_spawn(
    argv: &[&[u8]],
    vfs_tid: usize,
    inherit_stdin: bool,
    inherit_stdout: bool,
) -> Option<Spawned> {
    let cmd = argv[0];
    let mut path = [0u8; 64];
    let Some(len) = resolve_program(cmd, vfs_tid, &mut path) else {
        if let Ok(s) = core::str::from_utf8(cmd) {
            println!("{}: not found", s);
        }
        return None;
    };

    // Read, load, grant from the manifest, and give the staging memory back.
    let info = match spawn::load_path(
        vfs_tid,
        &path[..len],
        FILE_BUF_BASE,
        &SPAWN_SCRATCH,
        grant_caps_from_manifest,
    ) {
        Ok(i) => i,
        Err(()) => {
            println!("shell: failed to load ELF");
            return None;
        }
    };

    let tid = info.tid;

    // The child starts where the shell is.
    let _ = vfs::give_cwd(vfs_tid, tid);

    // Wire file descriptors — duplicate the shell's own fds to the child.
    //
    // A pipeline stage skips whichever end is about to be replaced by a pipe:
    // there is no point handing it the console only for the pipe to close it.
    // stderr is never redirected, so it always comes from the shell.
    if inherit_stdin {
        let _ = syscall::sys_fd_dup(tid, 0, 0);
    }
    if inherit_stdout {
        let _ = syscall::sys_fd_dup(tid, 1, 1);
    }
    let _ = syscall::sys_fd_dup(tid, 2, 2);

    let _ = spawn::set_args_env(&info, argv, &BASE_ENV, &SPAWN_SCRATCH);

    Some(info)
}

/// Longest pipeline accepted. Each stage beyond the first needs a pipe, and a
/// task may hold MAX_PIPES_PER_TASK (8) of them, so this is well inside the
/// kernel's limit while covering anything typed by hand.
const MAX_STAGES: usize = 4;

/// Builtins run inside the shell process, so they have no fds of their own to
/// redirect and cannot be a pipeline stage.
fn is_builtin(cmd: &[u8]) -> bool {
    cmd == b"exit" || cmd == b"cd" || cmd == b"pwd" || cmd == b"kill" || cmd == b"status"
}

/// Split a stage into its command word and the rest.
/// The status of a command the shell could not run at all — not found, or not
/// loadable, or not startable. POSIX's "command not found" number.
///
/// Not -1, which is what this was: a negative status now means a task the
/// kernel killed, and the shell reporting its own failure to launch as one
/// printed "killed (1)" underneath "not found". The reason has always already
/// been printed by the time this is returned, so nothing prints it again.
const NOT_RUN: i32 = 127;

/// Run one command to completion. Returns its exit status.
/// The one word a redirection names, unquoted.
fn file_named<'a>(typed: &[u8], store: &'a mut [u8; words::STORE]) -> Option<&'a [u8]> {
    let mut one: [&[u8]; words::MAX_WORDS] = [b""; words::MAX_WORDS];
    match words::split(typed, store, &mut one) {
        Ok(1) => Some(one[0]),
        _ => None,
    }
}

/// Open the file a redirection names and put it where the child reads or
/// writes. The descriptor is the child's alone: the shell's copy is closed.
fn redirect_to(vfs_tid: usize, child: usize, fd: usize, typed: &[u8], flags: u64) -> bool {
    let mut store = [0u8; words::STORE];
    let Some(path) = file_named(typed, &mut store) else {
        println!("qsh: a redirection wants one file to go to");
        return false;
    };
    match vfs::open_fd(vfs_tid, path, flags, 0o644) {
        Ok(opened) => {
            let given = syscall::sys_fd_dup(child, fd, opened).is_ok();
            let _ = syscall::sys_fd_close(opened);
            if !given {
                println!("qsh: {}: could not be handed over", core::str::from_utf8(path).unwrap_or("?"));
            }
            given
        }
        Err(code) => {
            println!("qsh: {}: {}", core::str::from_utf8(path).unwrap_or("?"), vfs::why(code));
            false
        }
    }
}

fn cmd_exec(argv: &[&[u8]], vfs_tid: usize, input_tid: usize, redirect: &words::Redirects) -> i32 {
    let info = match cmd_spawn(argv, vfs_tid, redirect.input.is_none(), redirect.output.is_none()) {
        Some(i) => i,
        None => return NOT_RUN,
    };
    // `< FILE` and `> FILE`: opened by the shell, as whoever is typing, and
    // given to the program in place of the terminal.
    let wired = redirect.input.is_none_or(|file| redirect_to(vfs_tid, info.tid, 0, file, vfs::OPEN_READ))
        && redirect.output.is_none_or(|file| {
            let how = if redirect.append { vfs::OPEN_APPEND } else { vfs::OPEN_TRUNCATE };
            redirect_to(vfs_tid, info.tid, 1, file, vfs::OPEN_WRITE | vfs::OPEN_CREATE | how)
        });
    if !wired {
        info.discard();
        return NOT_RUN;
    }
    if info.start().is_err() {
        println!("shell: failed to start task");
        info.discard();
        return NOT_RUN;
    }

    if input_tid != 0 {
        set_foreground(input_tid, info.tid);
    }
    let status = match syscall::sys_wait() {
        Ok((_, code)) => code,
        Err(()) => {
            println!("shell: lost track of the task it started");
            NOT_RUN
        }
    };
    if input_tid != 0 {
        set_foreground(input_tid, 0);
    }
    status
}

/// Run a pipeline: stage i's stdout becomes stage i+1's stdin.
///
/// Every stage is created first and started only once all the pipes are wired,
/// so no stage can run against an endpoint that does not exist yet. The shell
/// installs each end on a child and holds neither itself — a write end left in
/// the shell would keep the writer count above zero, and the reader would wait
/// for an EOF that never came.
///
/// Returns the last stage's status, as a POSIX shell does.
fn cmd_pipeline(stages: &[&[u8]], vfs_tid: usize, input_tid: usize) -> i32 {
    let n = stages.len();
    if n > MAX_STAGES {
        println!("shell: pipeline too long (max {} stages)", MAX_STAGES);
        return NOT_RUN;
    }

    let mut pipes = [0usize; MAX_STAGES - 1];
    let mut npipes = 0;
    for i in 0..n - 1 {
        match syscall::sys_pipe_create() {
            Ok(h) => { pipes[i] = h; npipes += 1; }
            Err(()) => {
                println!("shell: out of pipes");
                return NOT_RUN;
            }
        }
    }
    let _ = npipes;

    let mut infos = [Spawned::EMPTY; MAX_STAGES];
    let mut spawned = 0;

    for i in 0..n {
        // The first stage may be given a file to read and the last a file
        // to write; anywhere else the pipe is what is read and written.
        let mut rest = [0u8; words::STORE];
        let (rest_len, redirect) = match words::redirects(stages[i], &mut rest) {
            Ok(found) => found,
            Err(why) => {
                println!("qsh: {}", why);
                break;
            }
        };
        if (redirect.input.is_some() && i > 0) || (redirect.output.is_some() && i + 1 < n) {
            println!("qsh: only the first of a pipeline reads a file, and only the last writes one");
            break;
        }
        let mut store = [0u8; words::STORE];
        let mut argv: [&[u8]; words::MAX_WORDS] = [b""; words::MAX_WORDS];
        let argc = match words::split(&rest[..rest_len], &mut store, &mut argv) {
            Ok(argc) => argc,
            Err(why) => {
                println!("qsh: {}", why);
                break;
            }
        };
        if argc == 0 {
            println!("shell: empty pipeline stage");
            break;
        }
        let cmd = argv[0];
        if is_builtin(cmd) {
            if let Ok(c) = core::str::from_utf8(cmd) {
                println!("shell: {}: builtin cannot be used in a pipeline", c);
            }
            break;
        }

        let from_shell = i == 0 && redirect.input.is_none();
        let to_shell = i + 1 == n && redirect.output.is_none();
        let info = match cmd_spawn(&argv[..argc], vfs_tid, from_shell, to_shell) {
            Some(v) => v,
            None => break,
        };
        let wired = redirect.input.is_none_or(|file| redirect_to(vfs_tid, info.tid, 0, file, vfs::OPEN_READ))
            && redirect.output.is_none_or(|file| {
                let how = if redirect.append { vfs::OPEN_APPEND } else { vfs::OPEN_TRUNCATE };
                redirect_to(vfs_tid, info.tid, 1, file, vfs::OPEN_WRITE | vfs::OPEN_CREATE | how)
            });
        if !wired {
            info.discard();
            break;
        }

        // Reading end from the previous stage, writing end to the next. The
        // ends replace the stdin/stdout cmd_spawn duplicated from the shell.
        if i > 0 {
            let _ = syscall::sys_pipe_fd_set(info.tid, 0, pipes[i - 1], false);
        }
        if i + 1 < n {
            let _ = syscall::sys_pipe_fd_set(info.tid, 1, pipes[i], true);
        }

        infos[i] = info;
        spawned += 1;
    }

    // A stage that never loaded leaves the pipeline unrunnable; tear down the
    // tasks already created rather than leaking their slots.
    if spawned != n {
        for info in infos.into_iter().take(spawned) {
            info.discard();
        }
        return NOT_RUN;
    }

    for i in 0..n {
        if infos[i].start().is_err() {
            println!("shell: failed to start pipeline stage");
            for j in 0..n {
                let _ = syscall::sys_task_kill(infos[j].tid);
            }
            return NOT_RUN;
        }
    }

    let last_tid = infos[n - 1].tid;
    if input_tid != 0 {
        set_foreground(input_tid, last_tid);
    }

    // Reap every stage; the pipeline's status is the last stage's.
    let mut status = NOT_RUN;
    for _ in 0..n {
        match syscall::sys_wait() {
            Ok((tid, code)) => {
                if tid == last_tid {
                    status = code;
                }
            }
            Err(()) => break,
        }
    }

    if input_tid != 0 {
        set_foreground(input_tid, 0);
    }
    status
}

// ---------------------------------------------------------------------------
// Current working directory
// ---------------------------------------------------------------------------

/// Status of the last command run, reported by the `status` builtin.
static mut LAST_STATUS: i32 = 0;

/// Record a command's exit status and report a failure.
///
/// The kernel has carried exit codes through sys_wait since the syscall
/// boundary audit, but every caller discarded them, so a program had no way to
/// report failure. Non-zero is printed as it happens; `status` reads back the
/// last one either way.
fn set_status(name: &[u8], code: i32) {
    unsafe { LAST_STATUS = code; }
    if code == 0 || code == NOT_RUN {
        return;
    }
    let Ok(s) = core::str::from_utf8(name) else { return };
    // A negative status is a task the kernel killed, and the magnitude is the
    // signal Linux would have sent. Naming it is the difference between "it
    // failed" and "it executed an instruction it was not allowed to".
    match code {
        // Interrupted from the terminal, which showed `^C` and nothing after
        // it: the line is ended, and that says what happened.
        -2 => println!(),
        -3 => println!("{}: quit", s),
        // Its reader went away, which is how a pipeline ends early.
        -13 => {}
        -15 => println!("{}: terminated", s),
        -4 => println!("{}: illegal instruction", s),
        -5 => println!("{}: trace trap", s),
        -7 => println!("{}: bus error", s),
        -8 => println!("{}: floating-point exception", s),
        -9 => println!("{}: killed", s),
        -11 => println!("{}: segmentation fault", s),
        c if c < 0 => println!("{}: killed ({})", s, -c),
        c => println!("{}: exit {}", s, c),
    }
}

static mut HOME: [u8; 64] = [0; 64];
static mut HOME_LEN: usize = 0;

/// Remember the home directory: argv[1], which login passes, or — for a
/// shell started to run one command, or started in `.` — wherever it is.
fn home_init() {
    let home = match args::argv(1) {
        Some(h) if h.first() == Some(&b'/') && h.len() <= 64 => h,
        Some(b"-c") | Some(b".") => b"." as &[u8],
        _ => b"/home/root" as &[u8],
    };
    unsafe {
        HOME[..home.len()].copy_from_slice(home);
        HOME_LEN = home.len();
    }
}

fn home_get() -> &'static [u8] {
    unsafe { &HOME[..HOME_LEN] }
}

/// Why a directory could not be entered, in words.
fn dir_error(code: u64) -> &'static str {
    match code {
        vfs::ERR_NOT_FOUND => "no such directory",
        vfs::ERR_NOT_DIR => "not a directory",
        vfs::ERR_PERMISSION => "permission denied",
        vfs::ERR_NAME_TOO_LONG => "name too long",
        vfs::ERR_LOOP => "too many symbolic links",
        _ => "cannot enter it",
    }
}

/// The prompt: where the shell is, with the home directory as `~`.
fn print_prompt(vfs_tid: usize) {
    let mut buf = [0u8; vfs::MAX_PATH + 1];
    let Ok(len) = vfs::getcwd(vfs_tid, &mut buf) else {
        // The directory was removed from under the shell.
        print!("?$ ");
        return;
    };
    let cwd = &buf[..len];
    let home = home_get();
    let (tilde, rest) = if cwd == home {
        (true, &b""[..])
    } else if cwd.starts_with(home) && cwd.get(home.len()) == Some(&b'/') {
        (true, &cwd[home.len()..])
    } else {
        (false, cwd)
    };
    if tilde {
        print!("~");
    }
    if let Ok(s) = core::str::from_utf8(rest) {
        print!("{}", s);
        if !s.is_empty() && !s.ends_with('/') {
            print!("/");
        }
    }
    print!("$ ");
}

fn cmd_cd(arg: Option<&[u8]>, vfs_tid: usize) {
    let target = arg.unwrap_or(home_get());
    if let Err(code) = vfs::chdir(vfs_tid, target) {
        let shown = core::str::from_utf8(target).unwrap_or("?");
        println!("cd: {}: {}", shown, dir_error(code));
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    home_init();

    // Discover services
    let vfs_tid = match nameserver::lookup_retry(b"vfs", 50) {
        Some(tid) => tid,
        None => {
            println!("shell: vfs not found");
            syscall::sys_exit();
        }
    };
    // Start at home; if it is not there, wherever the shell was put. A
    // shell told to stay where it is has nowhere to go — and is not told
    // that it may not go there, which is what `su USER -c ...` typed in a
    // directory USER may not enter used to begin by saying.
    if home_get() == b"." {
        let mut here = [0u8; 64];
        if let Ok(len) = vfs::getcwd(vfs_tid, &mut here) {
            unsafe {
                HOME[..len].copy_from_slice(&here[..len]);
                HOME_LEN = len;
            }
        }
    } else if let Err(code) = vfs::chdir(vfs_tid, home_get()) {
        let shown = core::str::from_utf8(home_get()).unwrap_or("?");
        println!("shell: {}: {}", shown, dir_error(code));
    }

    let input_tid = nameserver::lookup(b"input").unwrap_or(0);

    // `qsh -c LINE`: one line, run as if it had been typed, and this ends
    // with what it ended with. What `su -c` hands a shell.
    if args::argv(1) == Some(&b"-c"[..]) {
        match args::argv(2) {
            Some(line) => {
                run_line(line, vfs_tid, input_tid);
                syscall::sys_exit_code(unsafe { LAST_STATUS });
            }
            None => {
                println!("usage: qsh -c LINE");
                syscall::sys_exit_code(2);
            }
        }
    }
    // On a terminal, a read of nothing is the end: Ctrl-D was typed at the
    // prompt, or the terminal has gone. From the input server it is a line
    // that was interrupted, and the next read is the next line.
    let on_terminal = syscall::sys_pty_number(0).is_ok();
    if on_terminal {
        // A terminal's Ctrl-C is raised for every program that has it open,
        // and this is one. What it is for is whatever is running in the
        // foreground, which has said nothing about it and is ended. Handled
        // here rather than ignored, so that at the prompt it ends the wait
        // for a line: the line is abandoned and there is a fresh prompt.
        let _ = syscall::sys_sig_action(syscall::SIGINT, syscall::SIG_HANDLE);
        let _ = syscall::sys_sig_action(syscall::SIGQUIT, syscall::SIG_IGNORE);
    }

    // Main loop
    let mut line_buf = [0u8; 256];
    loop {
        if on_terminal {
            // Whatever was raised while a command ran was the command's.
            let _ = syscall::sys_sig_take(None);
        }
        print_prompt(vfs_tid);

        let n = match read_line_event(&mut line_buf) {
            Line::Read(n) => n,
            Line::Nothing => 0,
            Line::Interrupted => {
                println!();
                continue;
            }
            // No descriptor on stdin. Nothing is ever going to be typed here,
            // so asking again is a spin — which is what running a shell under
            // a compositor used to be, a prompt printed as fast as the machine
            // could manage into a terminal that was not on the screen.
            Line::Closed => {
                println!("shell: no input available; exiting");
                syscall::sys_exit_code(0);
            }
        };
        if n == 0 {
            if on_terminal {
                // Asking again would be answered the same way, for ever: a
                // shell whose terminal emulator had gone sat printing prompts
                // at nothing, as fast as the machine would run it.
                println!();
                syscall::sys_exit_code(0);
            }
            continue;
        }

        let line = &line_buf[..n];

        // Trim trailing newline/whitespace
        let mut end = line.len();
        while end > 0 && (line[end - 1] == b'\n' || line[end - 1] == b'\r' || line[end - 1] == b' ') {
            end -= 1;
        }
        if end == 0 {
            continue;
        }
        let line = &line[..end];

        // Trim leading whitespace
        let mut start = 0;
        while start < line.len() && line[start] == b' ' {
            start += 1;
        }
        if start >= line.len() {
            continue;
        }
        let line = &line[start..];

        run_line(line, vfs_tid, input_tid);
    }
}

/// Run one line: a command, or a pipeline of them.
fn run_line(line: &[u8], vfs_tid: usize, input_tid: usize) {
    // Pipeline: split on '|' before anything else, since the first word of
    // `a | b` is a stage command rather than a builtin. A quoted bar is
    // not a pipe.
    let mut stages: [&[u8]; MAX_STAGES + 1] = [b""; MAX_STAGES + 1];
    let Some(nstages) = words::stages(line, &mut stages).filter(|&n| n <= MAX_STAGES) else {
        println!("shell: pipeline too long (max {} stages)", MAX_STAGES);
        return;
    };
    if nstages > 1 {
        let code = cmd_pipeline(&stages[..nstages], vfs_tid, input_tid);
        set_status(b"pipeline", code);
        return;
    }

    // Where its input comes from and its output goes, taken out of the
    // line before it is split into words.
    let mut rest = [0u8; words::STORE];
    let (rest_len, redirect) = match words::redirects(line, &mut rest) {
        Ok(found) => found,
        Err(why) => {
            println!("qsh: {}", why);
            return;
        }
    };
    let line = &rest[..rest_len];

    let mut store = [0u8; words::STORE];
    let mut argv: [&[u8]; words::MAX_WORDS] = [b""; words::MAX_WORDS];
    let argc = match words::split(line, &mut store, &mut argv) {
        Ok(n) => n,
        Err(why) => {
            println!("qsh: {}", why);
            return;
        }
    };
    if argc == 0 {
        return;
    }
    let argv = &argv[..argc];
    let cmd = argv[0];
    let args = &argv[1..];
    if is_builtin(cmd) && (redirect.input.is_some() || redirect.output.is_some()) {
        println!("qsh: {}: what the shell does itself has no output to send anywhere", core::str::from_utf8(cmd).unwrap_or("?"));
        return;
    }

    // Builtin: exit
    if cmd == b"exit" {
        syscall::sys_exit();
    }

    // Builtin: cd
    if cmd == b"cd" {
        cmd_cd(args.first().copied(), vfs_tid);
        return;
    }

    // Builtin: pwd
    if cmd == b"pwd" {
        let mut buf = [0u8; vfs::MAX_PATH + 1];
        match vfs::getcwd(vfs_tid, &mut buf) {
            Ok(len) => println!("{}", core::str::from_utf8(&buf[..len]).unwrap_or("?")),
            Err(_) => println!("pwd: the directory has been removed"),
        }
        return;
    }

    // Builtin: status
    if cmd == b"status" {
        println!("{}", unsafe { LAST_STATUS });
        return;
    }

    // Builtin: kill [-9] <tid>
    if cmd == b"kill" {
        let (sig, tid_arg) = match args {
            [b"-9", tid] => (syscall::SIG_KILL, *tid),
            [tid] => (syscall::SIG_TERM, *tid),
            _ => {
                println!("usage: kill [-9] <tid>");
                return;
            }
        };
        match parse_usize(tid_arg) {
            Some(tid) => {
                if syscall::sys_signal(tid, sig).is_err() {
                    // Refused is not the same as not there.
                    match syscall::sys_task_info(tid) {
                        Ok((_, _, owner)) => println!("kill: task {} is user {}'s, and not this account's to end", tid, owner),
                        Err(()) => println!("kill: there is no task {}", tid),
                    }
                }
            }
            None => println!("usage: kill [-9] <tid>"),
        }
        return;
    }

    // External command. Relative paths in its arguments are its own
    // business: it starts in the shell's directory.
    let code = cmd_exec(argv, vfs_tid, input_tid, &redirect);
    set_status(cmd, code);
}

fn parse_usize(s: &[u8]) -> Option<usize> {
    if s.is_empty() { return None; }
    let mut val: usize = 0;
    for &b in s {
        if b < b'0' || b > b'9' { return None; }
        val = val.checked_mul(10)?.checked_add((b - b'0') as usize)?;
    }
    Some(val)
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("shell: PANIC: {}", info);
    loop {
        core::hint::spin_loop();
    }
}

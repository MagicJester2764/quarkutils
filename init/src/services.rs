//! What init does once the machine is up: keeps the services.
//!
//! A service is a program init starts and keeps an eye on. Each has a name,
//! the services it needs, the name it registers if it is up only once it
//! has registered, and what is done when it ends. The boot image's are added
//! as the passes start them; the root's come from `/etc/init.conf`
//! ([`Manager::configure`]). A service is started once everything it needs
//! is up, and one that needs a name nothing has waits for it, which `svc`
//! says.
//!
//! After that init is this loop ([`Manager::serve`]): it watches every
//! child, collects one that ends, starts what has become startable, runs the
//! `run` lines one after another and then the session, answers `svc`
//! (`quark_rt::services`), and sleeps until the next thing is due.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use quark_rt::ipc::{death_notice, Message, TID_ANY};
use quark_rt::services::{self as proto, State};
use quark_rt::{nameserver, println, spawn, syscall};

use crate::{grant_caps_from_manifest, SPAWN_SCRATCH, VFS_IMAGE_BASE};

/// How long a service may take to register before init says so. It is still
/// waited for.
const SLOW_NS: u64 = 30_000_000_000;
/// How often a service that is starting is asked after.
const LOOK_NS: u64 = 20_000_000;
/// How long the session waits for services that are still starting. A line
/// one of them prints after the login prompt pushes the prompt off the line
/// it is read from; a machine whose network never comes is not kept from
/// logging in for long.
const SESSION_WAIT_NS: u64 = 5_000_000_000;
/// The arguments a configured program may be given.
const MAX_ARGS: usize = 6;

/// Where a service's program comes from.
pub enum Program {
    /// A copy of a boot program, kept to start it again.
    Image(Vec<u8>),
    /// A boot program that is never started again: nothing is kept.
    Boot,
    /// A program on the root.
    Path(Vec<u8>),
}

/// What is done when a service ends.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    Never,
    /// When it ends with a status that is not 0: a fault, a signal, a failure.
    OnFailure,
    Always,
}

impl Policy {
    fn word(self) -> &'static str {
        match self {
            Policy::Never => "never",
            Policy::OnFailure => "on-failure",
            Policy::Always => "always",
        }
    }
}

pub struct Service {
    pub name: Vec<u8>,
    program: Program,
    /// What it is started as: its name first.
    argv: Vec<Vec<u8>>,
    needs: Vec<Vec<u8>>,
    policy: Policy,
    /// The name it is waited for under, if it registers one.
    register: Option<Vec<u8>>,
    /// Whether its standard input is the console's keyboard.
    stdin: bool,
    /// The session: started once the `run` lines have run.
    session: bool,
    state: State,
    tid: usize,
    pid: u64,
    starts: u64,
    /// When it was last started, in nanoseconds since boot.
    started: u64,
    ended: Option<i32>,
    said_slow: bool,
    /// Why it is failed, or anything else worth saying about it.
    note: Option<&'static str>,
}

impl Service {
    fn new(name: &[u8], program: Program, argv: Vec<Vec<u8>>, policy: Policy) -> Service {
        Service {
            name: name.to_vec(),
            program,
            argv,
            needs: Vec::new(),
            policy,
            register: None,
            stdin: false,
            session: false,
            state: State::Waiting,
            tid: 0,
            pid: 0,
            starts: 0,
            started: 0,
            ended: None,
            said_slow: false,
            note: None,
        }
    }

    fn show(&self) -> &str {
        core::str::from_utf8(&self.name).unwrap_or("a service")
    }
}

pub struct Manager {
    services: Vec<Service>,
    console_pipe: usize,
    input_tid: usize,
    vfs_tid: usize,
    /// `run` lines still to run, each its path and then its arguments.
    runs: Vec<Vec<Vec<u8>>>,
    /// The one running, and what it is.
    running: usize,
    running_name: Vec<u8>,
    /// When the session could first have been started.
    session_ready: u64,
}

fn now() -> u64 {
    syscall::sys_clock()
}

fn text(bytes: &[u8]) -> &str {
    core::str::from_utf8(bytes).unwrap_or("?")
}

/// A name a service may have: letters, digits and `-_.`, at most 24.
fn good_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name.len() <= proto::NAME_MAX
        && name.iter().all(|&b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

/// The name a task has registered first, as the nameserver says without
/// granting anything.
fn registered_as(tid: usize) -> Option<([u8; 24], usize)> {
    const TAG_LOOKUP_TID: u64 = 3;
    let msg = Message { sender: 0, tag: TAG_LOOKUP_TID, data: [tid as u64, 0, 0, 0, 0, 0] };
    let mut reply = Message::empty();
    if syscall::sys_call(nameserver::NAMESERVER_TID, &msg, &mut reply).is_err() || reply.tag != 0 {
        return None;
    }
    let mut name = [0u8; 24];
    for i in 0..3 {
        name[i * 8..i * 8 + 8].copy_from_slice(&reply.data[i].to_le_bytes());
    }
    Some((name, (reply.data[3] as usize).min(24)))
}

impl Manager {
    pub fn new() -> Manager {
        Manager {
            services: Vec::new(),
            console_pipe: 0,
            input_tid: 0,
            vfs_tid: 0,
            runs: Vec::new(),
            running: 0,
            running_name: Vec::new(),
            session_ready: 0,
        }
    }

    fn find(&self, name: &[u8]) -> Option<usize> {
        self.services.iter().position(|s| s.name == name)
    }

    /// A program a pass of the boot has started, as `tid`: up once it has
    /// registered `register`, if it registers.
    pub fn boot(&mut self, name: &[u8], tid: usize, program: Program, policy: Policy, register: Option<&[u8]>) {
        let mut s = Service::new(name, program, Vec::new(), policy);
        s.register = register.map(<[u8]>::to_vec);
        self.running_now(&mut s, tid);
        self.services.push(s);
    }

    /// What a boot program started as `tid` was called: what it is started
    /// as again.
    pub fn boot_argv(&mut self, name: &[u8], argv: &[&[u8]], stdin: bool) {
        if let Some(i) = self.find(name) {
            self.services[i].argv = argv.iter().map(|a| a.to_vec()).collect();
            self.services[i].stdin = stdin;
        }
    }

    fn running_now(&self, s: &mut Service, tid: usize) {
        let _ = syscall::sys_task_watch(tid);
        s.tid = tid;
        s.pid = syscall::sys_pid(tid).unwrap_or(0);
        s.starts += 1;
        s.started = now();
        s.said_slow = false;
        s.state = if s.register.is_some() { State::Starting } else { State::Up };
    }

    /// What the console is, for whatever is started from now on.
    pub fn wire(&mut self, console_pipe: usize, input_tid: usize) {
        self.console_pipe = console_pipe;
        self.input_tid = input_tid;
    }

    /// `/etc/init.conf`, once there are files: the services it names, the
    /// programs to run, and the session.
    ///
    /// ```text
    /// service NAME [needs=A,B] [restart=always|on-failure|never] [register=X] PATH [ARGUMENT...]
    /// start PATH [ARGUMENT...]    a service named after its file, never started again
    /// run PATH [ARGUMENT...]      run to its end, in order, before the session
    /// session PATH                what the session is
    /// ```
    pub fn configure(&mut self, vfs_tid: usize, config: &[u8]) -> bool {
        self.vfs_tid = vfs_tid;
        let mut session = false;
        for line in config.split(|&b| b == b'\n') {
            let mut words = line.split(|&b| b == b' ' || b == b'\t' || b == b'\r').filter(|w| !w.is_empty());
            match words.next() {
                Some(b"service") => self.service_line(&mut words),
                Some(b"start") => self.start_line(&mut words),
                Some(b"run") => {
                    let run: Vec<Vec<u8>> = words.take(1 + MAX_ARGS).map(<[u8]>::to_vec).collect();
                    if run.first().is_some_and(|p| p.starts_with(b"/")) {
                        self.runs.push(run);
                    }
                }
                Some(b"session") if !session => {
                    if let Some(path) = words.next().filter(|p| p.starts_with(b"/")) {
                        session = self.session(path);
                    }
                }
                _ => {}
            }
        }
        session
    }

    fn service_line<'a>(&mut self, words: &mut impl Iterator<Item = &'a [u8]>) {
        let Some(name) = words.next() else { return };
        if !good_name(name) {
            println!("[init] /etc/init.conf: '{}' is not a name a service can have", text(name));
            return;
        }
        if self.find(name).is_some() {
            println!("[init] /etc/init.conf: there is a service called {} already", text(name));
            return;
        }
        let mut needs = Vec::new();
        let mut policy = Policy::OnFailure;
        let mut register = None;
        let mut path = None;
        for word in words.by_ref() {
            if let Some(list) = word.strip_prefix(b"needs=") {
                needs.extend(list.split(|&b| b == b',').filter(|n| !n.is_empty()).map(<[u8]>::to_vec));
            } else if let Some(p) = word.strip_prefix(b"restart=") {
                policy = match p {
                    b"always" => Policy::Always,
                    b"on-failure" => Policy::OnFailure,
                    b"never" => Policy::Never,
                    _ => {
                        println!("[init] /etc/init.conf: {} has a restart= that is not one", text(name));
                        return;
                    }
                };
            } else if let Some(r) = word.strip_prefix(b"register=") {
                register = Some(r.to_vec());
            } else {
                path = Some(word);
                break;
            }
        }
        let Some(path) = path.filter(|p| p.starts_with(b"/") && p.len() <= 64) else {
            println!("[init] /etc/init.conf: {} names no program to run", text(name));
            return;
        };
        let mut argv = Vec::new();
        argv.push(path.rsplit(|&b| b == b'/').next().unwrap_or(path).to_vec());
        argv.extend(words.take(MAX_ARGS).map(<[u8]>::to_vec));
        let mut s = Service::new(name, Program::Path(path.to_vec()), argv, policy);
        s.needs = needs;
        s.register = register;
        self.services.push(s);
    }

    fn start_line<'a>(&mut self, words: &mut impl Iterator<Item = &'a [u8]>) {
        let Some(path) = words.next().filter(|p| p.starts_with(b"/") && p.len() <= 64) else { return };
        let file = path.rsplit(|&b| b == b'/').next().unwrap_or(path);
        // Named after its file, and the second of a name after that.
        let mut name = file.to_vec();
        let mut n = 2;
        while self.find(&name).is_some() || !good_name(&name) {
            name = file.iter().take(proto::NAME_MAX - 3).copied().collect();
            if !good_name(&name) {
                name = b"start".to_vec();
            }
            let _ = write!(Bytes(&mut name), "-{}", n);
            n += 1;
        }
        let mut argv = Vec::new();
        argv.push(file.to_vec());
        argv.extend(words.take(MAX_ARGS).map(<[u8]>::to_vec));
        self.services.push(Service::new(&name, Program::Path(path.to_vec()), argv, Policy::Never));
    }

    /// The session: what everybody logs in through. Started again if it
    /// fails; a login that ends by itself, on a console with no `getty`, has
    /// ended the session.
    fn session(&mut self, path: &[u8]) -> bool {
        if path.len() > 64 {
            return false;
        }
        let file = path.rsplit(|&b| b == b'/').next().unwrap_or(path);
        let mut s = Service::new(b"session", Program::Path(path.to_vec()), alloc::vec![file.to_vec()], Policy::OnFailure);
        s.stdin = true;
        s.session = true;
        self.services.push(s);
        true
    }

    /// No session was named: `login`, or the shell, from `/usr/bin`, as the
    /// file is called there.
    pub fn default_session(&mut self, path: &[u8]) {
        if self.session(path) {
            if let Some(s) = self.services.last_mut() {
                s.policy = Policy::Never;
            }
        }
    }

    /// The loop init is for the rest of the machine's life.
    pub fn serve(mut self) -> ! {
        if nameserver::register(proto::NAME).is_err() {
            println!("[init] Could not register as {}: nothing can ask about the services.", text(proto::NAME));
        }
        loop {
            self.collect();
            self.settle();
            let mut msg = Message::empty();
            let heard = match self.next_wait() {
                Some(ns) => syscall::sys_recv_timeout(TID_ANY, &mut msg, syscall::ns(ns.max(1))).is_ok(),
                None => syscall::sys_recv(TID_ANY, &mut msg).is_ok(),
            };
            if !heard {
                continue;
            }
            if let Some(dead) = death_notice(&msg) {
                self.died(dead);
                continue;
            }
            if msg.sender == 0 {
                continue;
            }
            self.request(&msg);
        }
    }

    /// A child the kernel has said is gone: collected as soon as it can be.
    fn died(&mut self, tid: usize) {
        let ours = tid == self.running || self.services.iter().any(|s| s.tid == tid);
        if ours {
            if let Ok((tid, status)) = syscall::sys_wait_for(tid) {
                self.ended(tid, status);
            }
        }
    }

    /// Every child that has ended, collected.
    fn collect(&mut self) {
        while let Ok(Some((tid, status))) = syscall::sys_wait_nowait(0) {
            self.ended(tid, status);
        }
    }

    fn ended(&mut self, tid: usize, status: i32) {
        if tid == self.running && tid != 0 {
            self.running = 0;
            if status != 0 {
                println!("[init] {} ended with status {}", text(&self.running_name), status);
            }
            return;
        }
        let Some(i) = self.services.iter().position(|s| s.tid == tid && tid != 0) else { return };
        let s = &mut self.services[i];
        s.tid = 0;
        s.pid = 0;
        s.ended = Some(status);
        s.state = if status == 0 { State::Done } else { State::Failed };
        if status != 0 {
            println!("[init] {} ended with status {}", s.show(), status);
        }
    }

    fn up(&self, name: &[u8]) -> bool {
        self.find(name).is_some_and(|i| self.services[i].state == State::Up)
    }

    /// Whatever has changed since: what has registered is up, what needed it
    /// is started, and the `run` lines and the session take their turns.
    fn settle(&mut self) {
        let t = now();
        for s in self.services.iter_mut().filter(|s| s.state == State::Starting) {
            let want = s.register.as_deref().unwrap_or(b"");
            if registered_as(s.tid).is_some_and(|(name, n)| &name[..n] == want) {
                s.state = State::Up;
            } else if !s.said_slow && t.saturating_sub(s.started) > SLOW_NS {
                s.said_slow = true;
                println!("[init] {} has not registered as {} after {} seconds", s.show(), text(want), SLOW_NS / 1_000_000_000);
            }
        }
        for i in 0..self.services.len() {
            let s = &self.services[i];
            if s.state != State::Waiting || s.session {
                continue;
            }
            if s.needs.iter().all(|n| self.up(n)) {
                self.launch(i);
            }
        }
        if self.running == 0 && !self.runs.is_empty() {
            let run = self.runs.remove(0);
            self.run(run);
        }
        if self.running == 0 && self.runs.is_empty() {
            if self.session_ready == 0 {
                self.session_ready = t;
            }
            let starting = self.services.iter().any(|s| s.state == State::Starting);
            if !starting || t.saturating_sub(self.session_ready) >= SESSION_WAIT_NS {
                if let Some(i) = self.services.iter().position(|s| s.session && s.state == State::Waiting) {
                    self.launch(i);
                }
            }
        }
    }

    /// How long until something is due: `None` when nothing is.
    fn next_wait(&self) -> Option<u64> {
        let t = now();
        let mut wait: Option<u64> = None;
        let mut sooner = |ns: u64| wait = Some(wait.map_or(ns, |w| w.min(ns)));
        if self.services.iter().any(|s| s.state == State::Starting) {
            sooner(LOOK_NS);
        }
        if self.running == 0 && self.runs.is_empty() && self.services.iter().any(|s| s.session && s.state == State::Waiting) {
            sooner((self.session_ready + SESSION_WAIT_NS).saturating_sub(t).max(1));
        }
        wait
    }

    /// Load a service's program, give it what it needs, and start it.
    fn launch(&mut self, i: usize) -> bool {
        let (console, input, vfs) = (self.console_pipe, self.input_tid, self.vfs_tid);
        let s = &self.services[i];
        let grant = |image: &[u8], tid: usize| grant_caps_from_manifest(image, tid);
        let loaded = match &s.program {
            Program::Image(image) => spawn::load(image, &SPAWN_SCRATCH).ok().inspect(|info| grant(image, info.tid)),
            Program::Path(path) if vfs != 0 => spawn::load_path(vfs, path, VFS_IMAGE_BASE, &SPAWN_SCRATCH, grant).ok(),
            _ => None,
        };
        let Some(info) = loaded else {
            let s = &mut self.services[i];
            s.state = State::Failed;
            s.note = Some("its program would not load");
            println!("[init] {} will not load", s.show());
            return false;
        };
        let tid = info.tid;
        if console != 0 {
            let _ = syscall::sys_pipe_fd_set(tid, 1, console, true);
            let _ = syscall::sys_pipe_fd_set(tid, 2, console, true);
        }
        if s.stdin && input != 0 {
            let _ = syscall::sys_fd_set(tid, 0, input, 1);
        }
        let argv: Vec<&[u8]> = s.argv.iter().map(Vec::as_slice).collect();
        let _ = spawn::set_args(&info, &argv, &SPAWN_SCRATCH);
        let mut s = core::mem::replace(&mut self.services[i], Service::new(b"", Program::Boot, Vec::new(), Policy::Never));
        self.running_now(&mut s, tid);
        let started = info.start().is_ok();
        println!("[init] Started {} (TID {})", s.show(), tid);
        self.services[i] = s;
        started
    }

    /// One `run` line: started, and the next is started when it ends.
    fn run(&mut self, run: Vec<Vec<u8>>) {
        let Some(path) = run.first() else { return };
        let file = path.rsplit(|&b| b == b'/').next().unwrap_or(path);
        let grant = |image: &[u8], tid: usize| grant_caps_from_manifest(image, tid);
        let Ok(info) = spawn::load_path(self.vfs_tid, path, VFS_IMAGE_BASE, &SPAWN_SCRATCH, grant) else {
            println!("[init] /etc/init.conf asks for a program to be run that will not load.");
            return;
        };
        if self.console_pipe != 0 {
            let _ = syscall::sys_pipe_fd_set(info.tid, 1, self.console_pipe, true);
            let _ = syscall::sys_pipe_fd_set(info.tid, 2, self.console_pipe, true);
        }
        let mut argv: Vec<&[u8]> = Vec::new();
        argv.push(file);
        argv.extend(run[1..].iter().map(Vec::as_slice));
        let _ = spawn::set_args(&info, &argv, &SPAWN_SCRATCH);
        let _ = syscall::sys_task_watch(info.tid);
        self.running = info.tid;
        self.running_name = file.to_vec();
        if info.start().is_err() {
            self.running = 0;
        }
    }

    // ------------------------------------------------------------------
    // What `svc` asks.
    // ------------------------------------------------------------------

    fn request(&mut self, msg: &Message) {
        match msg.tag {
            proto::TAG_TABLE => {
                let out = self.table();
                self.give_text(msg, &out);
            }
            proto::TAG_STATE => match self.named(msg) {
                Some(i) => {
                    let s = &self.services[i];
                    reply(
                        msg.sender,
                        proto::TAG_OK,
                        [
                            s.state.number(),
                            s.tid as u64,
                            s.pid,
                            s.starts,
                            s.ended.unwrap_or(0) as i64 as u64,
                            s.ended.is_some() as u64,
                        ],
                    );
                }
                None => refuse(msg.sender, proto::NO_SUCH),
            },
            proto::TAG_DESCRIBE => match self.named(msg) {
                Some(i) => {
                    let out = self.describe(i);
                    self.give_text(msg, &out);
                }
                None => refuse(msg.sender, proto::NO_SUCH),
            },
            _ => refuse(msg.sender, proto::INVALID),
        }
    }

    /// The service a request names, by the name in its first four words.
    fn named(&self, msg: &Message) -> Option<usize> {
        let (name, n) = proto::unpack(&msg.data[..4]);
        self.find(&name[..n])
    }

    /// Text into the buffer a request lent: `[written, how long it was]`.
    fn give_text(&self, msg: &Message, out: &str) {
        let room = msg.data[4] as usize;
        let n = out.len().min(room);
        if n > 0 && syscall::sys_lent_write(msg.sender, 0, &out.as_bytes()[..n]).is_err() {
            return refuse(msg.sender, proto::INVALID);
        }
        reply(msg.sender, proto::TAG_OK, [n as u64, out.len() as u64, 0, 0, 0, 0]);
    }

    /// What a service is doing, in a few words.
    fn doing(&self, s: &Service) -> String {
        let mut out = String::new();
        match s.state {
            State::Waiting if s.session => out.push_str("waiting for the boot to finish"),
            State::Waiting => {
                out.push_str("waiting for ");
                let mut first = true;
                for n in s.needs.iter().filter(|n| !self.up(n)) {
                    let _ = write!(out, "{}{}", if first { "" } else { ", " }, text(n));
                    if self.find(n).is_none() {
                        out.push_str(" (no such service)");
                    }
                    first = false;
                }
            }
            State::Starting => {
                let _ = write!(out, "starting, not yet {}", text(s.register.as_deref().unwrap_or(b"")));
            }
            State::Failed | State::Done => {
                out.push_str(s.state.word());
                if let Some(status) = s.ended {
                    let _ = write!(out, ", status {}", status);
                }
            }
            other => out.push_str(other.word()),
        }
        out
    }

    fn table(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "{:<14} {:<34} {:>5} {:>6}  {}", "SERVICE", "STATE", "PID", "STARTS", "UP FOR");
        let t = now();
        for s in &self.services {
            let _ = write!(out, "{:<14} {:<34} ", s.show(), self.doing(s));
            if s.tid != 0 {
                let secs = t.saturating_sub(s.started) / 1_000_000_000;
                let _ = writeln!(out, "{:>5} {:>6}  {}:{:02}:{:02}", s.pid, s.starts, secs / 3600, secs / 60 % 60, secs % 60);
            } else {
                let _ = writeln!(out, "{:>5} {:>6}", "-", s.starts);
            }
        }
        out
    }

    fn describe(&self, i: usize) -> String {
        let s = &self.services[i];
        let mut out = String::new();
        let _ = writeln!(out, "{}: {}", s.show(), self.doing(s));
        if s.tid != 0 {
            let secs = now().saturating_sub(s.started) / 1_000_000_000;
            let _ = writeln!(out, "  process {} (task {}), up for {}:{:02}:{:02}", s.pid, s.tid, secs / 3600, secs / 60 % 60, secs % 60);
        }
        match &s.program {
            Program::Path(path) => {
                let _ = write!(out, "  runs {}", text(path));
                for a in s.argv.iter().skip(1) {
                    let _ = write!(out, " {}", text(a));
                }
                out.push('\n');
            }
            Program::Image(_) => {
                let _ = writeln!(out, "  runs {} from the boot image, which init keeps", text(s.argv.first().map_or(&s.name[..], |a| &a[..])));
            }
            Program::Boot => {
                let _ = writeln!(out, "  runs {} from the boot image", text(s.argv.first().map_or(&s.name[..], |a| &a[..])));
            }
        }
        let _ = write!(out, "  needs ");
        if s.needs.is_empty() {
            out.push_str("nothing");
        }
        for (k, n) in s.needs.iter().enumerate() {
            let _ = write!(out, "{}{}", if k == 0 { "" } else { ", " }, text(n));
        }
        out.push('\n');
        if let Some(r) = &s.register {
            let _ = writeln!(out, "  up once it has registered {}", text(r));
        }
        let _ = writeln!(out, "  started {} time{}; started again: {}", s.starts, if s.starts == 1 { "" } else { "s" }, s.policy.word());
        if let Some(status) = s.ended {
            let _ = writeln!(out, "  last ended with status {}", status);
        }
        if let Some(note) = s.note {
            let _ = writeln!(out, "  {}", note);
        }
        out
    }
}

fn reply(tid: usize, tag: u64, data: [u64; 6]) {
    let _ = syscall::sys_reply(tid, &Message { sender: 0, tag, data });
}

fn refuse(tid: usize, why: u64) {
    reply(tid, proto::TAG_ERROR, [why, 0, 0, 0, 0, 0]);
}

/// `write!` into a `Vec<u8>`.
struct Bytes<'a>(&'a mut Vec<u8>);

impl core::fmt::Write for Bytes<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

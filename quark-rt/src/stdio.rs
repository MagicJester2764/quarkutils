use core::fmt;
use crate::syscall;

const BUF_SIZE: usize = 256;

/// What a `print!` is gathered in on its way to a descriptor, so that a
/// formatted line is one write and not one per piece.
struct BufWriter {
    buf: [u8; BUF_SIZE],
    pos: usize,
    fd: usize,
}

impl BufWriter {
    const fn new(fd: usize) -> Self {
        BufWriter { buf: [0; BUF_SIZE], pos: 0, fd }
    }

    fn flush(&mut self) {
        if self.pos == 0 {
            return;
        }
        let data = &self.buf[..self.pos];
        let ret = syscall::sys_fd_write(self.fd, data);
        if ret == u64::MAX {
            syscall::sys_write(data);
        }
        self.pos = 0;
    }
}

impl fmt::Write for BufWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            if self.pos == BUF_SIZE {
                // Full: what has been gathered goes, and the rest follows.
                // It used to be dropped, on the grounds that nothing prints
                // that much at once — and `cat` printed a file a page at a
                // time, so it showed the first 256 bytes of every 4096.
                self.flush();
            }
            self.buf[self.pos] = b;
            self.pos += 1;
        }
        Ok(())
    }
}

pub fn _print(args: fmt::Arguments) {
    use fmt::Write;
    let mut w = BufWriter::new(1);
    let _ = w.write_fmt(args);
    w.flush();
}

/// Write bytes to standard output as they are: what `print!` is for text,
/// for a program that is passing on bytes it did not write and has no
/// business deciding the meaning of.
pub fn print_bytes(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    if syscall::sys_fd_write(1, bytes) == u64::MAX {
        syscall::sys_write(bytes);
    }
}

pub fn _eprint(args: fmt::Arguments) {
    use fmt::Write;
    let mut w = BufWriter::new(2);
    let _ = w.write_fmt(args);
    w.flush();
}

/// Read a line from stdin (fd 0) into `buf`. Returns the number of bytes read.
/// Blocks until a line is available. Returns 0 if stdin is not connected.
pub fn read_line(buf: &mut [u8]) -> usize {
    read_line_result(buf).unwrap_or(0)
}

/// The bit of a terminal's local flags that has it show what is typed.
const ECHO: u32 = 0o10;
/// The console's line reader is asked for a line with this, a length, and
/// then a word that says whether to show the line as it is typed.
const TAG_INPUT_READ: u64 = 1;

/// Read a line that is not shown as it is typed: a password.
///
/// A terminal has its echo turned off for the line and put back. The
/// console's own line reader — what standard input is where nothing has put
/// a terminal there — is asked for the line with a word that says not to
/// show it. Anything else, a pipe or a file, is read as it is: nothing was
/// going to show it.
pub fn read_secret(buf: &mut [u8]) -> usize {
    if let Ok(was) = syscall::sys_pty_get_termios(0) {
        let mut quiet = was;
        quiet.c_lflag &= !ECHO;
        let _ = syscall::sys_pty_set_termios(0, &quiet);
        let n = read_line(buf);
        let _ = syscall::sys_pty_set_termios(0, &was);
        // The newline that ended it was not shown either.
        print_bytes(b"\n");
        return n;
    }
    let console = matches!(syscall::sys_fd_kind(0), Some((syscall::FD_KIND_ENDPOINT, _)));
    let Some(input) = crate::nameserver::lookup(b"input").filter(|_| console) else {
        return read_line(buf);
    };
    let mut got = 0;
    while got < buf.len() {
        let want = (buf.len() - got).min(40);
        let msg = crate::ipc::Message { sender: 0, tag: TAG_INPUT_READ, data: [want as u64, 1, 0, 0, 0, 0] };
        let mut reply = crate::ipc::Message::empty();
        if syscall::sys_call(input, &msg, &mut reply).is_err() {
            break;
        }
        let n = (reply.data[0] as usize).min(want);
        if n == 0 {
            break;
        }
        for i in 0..n {
            buf[got + i] = (reply.data[1 + i / 8] >> (8 * (i % 8))) as u8;
        }
        got += n;
        if buf[got - 1] == b'\n' {
            break;
        }
    }
    got
}

/// What reading a line came to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Line {
    /// This many bytes, the newline among them if there was room.
    Read(usize),
    /// A read of nothing. From a terminal or a pipe that is the end; from the
    /// input server it is a line that was interrupted.
    Nothing,
    /// There is no descriptor to read from.
    Closed,
    /// A signal this program handles arrived instead.
    Interrupted,
}

/// Read a line and say which of four things happened.
pub fn read_line_event(buf: &mut [u8]) -> Line {
    let mut got = 0;
    while got < buf.len() {
        let ret = syscall::sys_fd_read(0, &mut buf[got..]);
        if ret == syscall::INTERRUPTED {
            return Line::Interrupted;
        }
        if ret == u64::MAX {
            return if got == 0 { Line::Closed } else { Line::Read(got) };
        }
        let n = ret as usize;
        if n == 0 {
            break;
        }
        got += n;
        if buf[got - 1] == b'\n' {
            break;
        }
    }
    if got == 0 { Line::Nothing } else { Line::Read(got) }
}

/// Read a line, distinguishing "nothing was typed" from "there is nowhere to
/// read from".
///
/// [`read_line`] answers 0 to both, which is fine for a program that will ask
/// again and wrong for one that will ask again *immediately*: a shell with no
/// descriptor on stdin spins printing prompts at a pipe nobody is reading.
/// `Err` means the descriptor is not connected, which is not a pause — it is
/// the end.
///
/// A line arrives in as many reads as it takes: a read over IPC carries forty
/// bytes, and the input server keeps the rest of a longer line for the next.
/// This reads until the newline, the end of `buf`, or a read that returns
/// nothing after something was read.
pub fn read_line_result(buf: &mut [u8]) -> Result<usize, ()> {
    match read_line_event(buf) {
        Line::Read(n) => Ok(n),
        Line::Nothing | Line::Interrupted => Ok(0),
        Line::Closed => Err(()),
    }
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::stdio::_print(format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($fmt:expr $(, $($arg:tt)*)?) => {
        $crate::stdio::_print(format_args!(concat!($fmt, "\n") $(, $($arg)*)?))
    };
}

#[macro_export]
macro_rules! eprint {
    ($($arg:tt)*) => {
        $crate::stdio::_eprint(format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! eprintln {
    () => { $crate::eprint!("\n") };
    ($fmt:expr $(, $($arg:tt)*)?) => {
        $crate::stdio::_eprint(format_args!(concat!($fmt, "\n") $(, $($arg)*)?))
    };
}

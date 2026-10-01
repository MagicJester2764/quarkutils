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

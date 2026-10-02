use crate::time::Duration;

// The kernel's clock counts nanoseconds: since boot for a time that only
// goes forward, and since 1970 for the date, which moves when somebody sets
// it. How fine it really is depends on the machine — well under a
// microsecond where the processor has a counter the kernel can keep time by,
// ten milliseconds where it has not.

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Instant(u64); // nanoseconds since boot

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct SystemTime(u64); // nanoseconds since 1970

pub const UNIX_EPOCH: SystemTime = SystemTime(0);

fn nanos(time: &Duration) -> Option<u64> {
    u64::try_from(time.as_nanos()).ok()
}

impl Instant {
    pub fn now() -> Instant {
        Instant(quark_rt::rt::now_ns())
    }

    pub fn checked_sub_instant(&self, other: &Instant) -> Option<Duration> {
        self.0.checked_sub(other.0).map(Duration::from_nanos)
    }

    pub fn checked_add_duration(&self, other: &Duration) -> Option<Instant> {
        self.0.checked_add(nanos(other)?).map(Instant)
    }

    pub fn checked_sub_duration(&self, other: &Duration) -> Option<Instant> {
        self.0.checked_sub(nanos(other)?).map(Instant)
    }
}

impl SystemTime {
    pub const MAX: SystemTime = SystemTime(u64::MAX);
    pub const MIN: SystemTime = SystemTime(0);

    pub fn now() -> SystemTime {
        SystemTime(quark_rt::rt::unix_ns())
    }

    pub fn sub_time(&self, other: &SystemTime) -> Result<Duration, Duration> {
        if self.0 >= other.0 {
            Ok(Duration::from_nanos(self.0 - other.0))
        } else {
            Err(Duration::from_nanos(other.0 - self.0))
        }
    }

    pub fn checked_add_duration(&self, other: &Duration) -> Option<SystemTime> {
        self.0.checked_add(nanos(other)?).map(SystemTime)
    }

    pub fn checked_sub_duration(&self, other: &Duration) -> Option<SystemTime> {
        self.0.checked_sub(nanos(other)?).map(SystemTime)
    }
}

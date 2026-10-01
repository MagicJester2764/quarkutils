pub fn fill_bytes(bytes: &mut [u8]) {
    // The kernel's generator. Failing loudly beats handing out zeroes, which
    // is what the RDRAND loop this replaced did when the instruction failed.
    quark_rt::random::fill(bytes).expect("the kernel has no random numbers")
}

//! Ordering contract for blocking map loads.

/// Submit and expose the loading presentation before any map payload read.
///
/// Keeping this tiny sequence in the host-tested core prevents a caller from
/// accidentally moving the synchronous CD read ahead of the only frame the
/// player can see while the drive is busy.
#[optimize(size)]
pub fn present_before_payload<T>(present: impl FnOnce(), read_payload: impl FnOnce() -> T) -> T {
    present();
    read_payload()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;

    #[optimize(size)]
    #[test]
    fn a_frame_is_submitted_before_the_map_payload_is_read() {
        let sequence = Cell::new(0u8);
        let value = present_before_payload(
            || {
                assert_eq!(sequence.get(), 0);
                sequence.set(1);
            },
            || {
                assert_eq!(sequence.get(), 1, "payload read preceded presentation");
                sequence.set(2);
                42
            },
        );
        assert_eq!(value, 42);
        assert_eq!(sequence.get(), 2);
    }
}

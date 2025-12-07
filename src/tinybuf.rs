#[derive(Debug)]
pub struct Buffer<const N: usize> {
    buf: [u8; N],
    len: usize,
}

#[derive(Debug, PartialEq)]
pub enum BufferError {
    Overflow,
}

impl<const N: usize> Buffer<N> {
    pub fn new() -> Self {
        Self {
            buf: [0_u8; N],
            len: 0_usize,
        }
    }
    pub fn push(&mut self, data: &[u8]) -> Result<(), BufferError> {
        if data.len() > N {
            Err(BufferError::Overflow)
        } else {
            if data.len() > self.available() {
                // Shift buffer left by min(data.len,N/2)
                // (avoids frequent small shifts)
                self.advance(data.len().max(N / 2));
            }
            self.buf[self.len..(self.len + data.len())].copy_from_slice(data);
            self.len += data.len();
            Ok(())
        }
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn available(&self) -> usize {
        N - self.len
    }
    pub fn clear(&mut self) {
        self.buf.fill(0);
        self.len = 0;
    }
    pub fn advance(&mut self, n: usize) {
        // Dont advance more than self.len
        let n = n.min(self.len);
        // Shift buffer left
        self.buf.copy_within(n.., 0);
        // Reset self.len
        self.len -= n;
    }
    pub fn find(&self, needle: &[u8]) -> Option<usize> {
        if needle.is_empty() {
            return Some(0);
        }
        self.buf[..self.len]
            .windows(needle.len())
            .position(|w| w == needle)
    }
    pub fn copy_to(&self, dst: &mut [u8]) -> usize {
        let n = self.len.min(dst.len());
        dst[..n].copy_from_slice(&self.buf[..n]);
        n
    }
    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_is_empty() {
        let buf: Buffer<8> = Buffer::new();
        assert_eq!(buf.len(), 0);
        assert_eq!(buf.available(), 8);
    }

    #[test]
    fn test_push_fits_exactly() {
        let mut buf: Buffer<4> = Buffer::new();
        let data = [1, 2, 3, 4];
        assert!(buf.push(&data).is_ok());
        assert_eq!(buf.len(), 4);
        assert_eq!(buf.available(), 0);
        assert_eq!(&buf.buf[..4], &data);
    }

    #[test]
    fn test_push_too_large_fails() {
        let mut buf: Buffer<3> = Buffer::new();
        let data = [1, 2, 3, 4]; // len = 4 > N = 3
        assert_eq!(buf.push(&data), Err(BufferError::Overflow));
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn test_push_multiple_chunks() {
        let mut buf: Buffer<8> = Buffer::new();
        assert!(buf.push(&[1, 2]).is_ok());
        assert_eq!(buf.len(), 2);
        assert!(buf.push(&[3, 4]).is_ok());
        assert_eq!(buf.len(), 4);
        assert_eq!(&buf.buf[..4], &[1, 2, 3, 4]);
    }

    #[test]
    fn test_push_when_full_triggers_shift() {
        let mut buf: Buffer<4> = Buffer::new();
        // Fill buffer
        assert!(buf.push(&[1, 2, 3, 4]).is_ok());
        // Push 2 more bytes: should shift by max(2, 4/2) = 2
        assert!(buf.push(&[5, 6]).is_ok());
        // After shift: [3, 4] -> shifted to front, then [5, 6] appended
        assert_eq!(buf.len(), 4);
        assert_eq!(&buf.buf[..4], &[3, 4, 5, 6]);
    }

    #[test]
    fn test_push_when_nearly_full() {
        let mut buf: Buffer<5> = Buffer::new();
        assert!(buf.push(&[1, 2, 3]).is_ok()); // len=3, avail=2
        assert!(buf.push(&[4, 5]).is_ok()); // fits exactly, no shift
        assert_eq!(buf.len(), 5);
        assert_eq!(&buf.buf[..5], &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_push_requires_shift_smaller_than_data_len() {
        let mut buf: Buffer<6> = Buffer::new();
        assert!(buf.push(&[1, 2, 3, 4]).is_ok()); // len=4, avail=2
                                                  // Push 3 bytes → need 3, have 2 → must shift
                                                  // shift amount = max(3, 6/2) = max(3,3) = 3
        assert!(buf.push(&[5, 6, 7]).is_ok());
        // After shifting 3: original [1,2,3,4] → keep [4], then append [5,6,7]
        // Result: [4,5,6,7] → len=4
        assert_eq!(buf.len(), 4);
        assert_eq!(&buf.buf[..4], &[4, 5, 6, 7]);
    }

    #[test]
    fn test_clear() {
        let mut buf: Buffer<4> = Buffer::new();
        assert!(buf.push(&[1, 2, 3, 4]).is_ok());
        buf.clear();
        assert_eq!(buf.len(), 0);
        assert_eq!(buf.available(), 4);
        // Ensure buffer is zeroed (as per clear() impl)
        assert_eq!(buf.buf, [0; 4]);
    }

    #[test]
    fn test_advance_zero() {
        let mut buf: Buffer<4> = Buffer::new();
        assert!(buf.push(&[1, 2]).is_ok());
        buf.advance(0);
        assert_eq!(buf.len(), 2);
        assert_eq!(&buf.buf[..2], &[1, 2]);
    }

    #[test]
    fn test_advance_full() {
        let mut buf: Buffer<4> = Buffer::new();
        assert!(buf.push(&[1, 2, 3, 4]).is_ok());
        buf.advance(4); // or more
        assert_eq!(buf.len(), 0);
        // Remaining buffer content doesn't matter (as len=0), but should not panic
    }

    #[test]
    fn test_advance_partial() {
        let mut buf: Buffer<5> = Buffer::new();
        assert!(buf.push(&[1, 2, 3, 4, 5]).is_ok());
        buf.advance(2);
        assert_eq!(buf.len(), 3);
        assert_eq!(&buf.buf[..3], &[3, 4, 5]);
    }

    #[test]
    fn test_advance_more_than_len() {
        let mut buf: Buffer<4> = Buffer::new();
        assert!(buf.push(&[1, 2]).is_ok());
        buf.advance(10); // should clamp to len=2
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn test_find_found_at_start() {
        let mut buf: Buffer<8> = Buffer::new();
        assert!(buf.push(&[1, 2, 3, 4]).is_ok());
        assert_eq!(buf.find(&[1, 2]), Some(0));
    }

    #[test]
    fn test_find_found_in_middle() {
        let mut buf: Buffer<8> = Buffer::new();
        assert!(buf.push(&[1, 2, 3, 4]).is_ok());
        assert_eq!(buf.find(&[2, 3]), Some(1));
    }

    #[test]
    fn test_find_found_at_end() {
        let mut buf: Buffer<8> = Buffer::new();
        assert!(buf.push(&[1, 2, 3, 4]).is_ok());
        assert_eq!(buf.find(&[3, 4]), Some(2));
    }

    #[test]
    fn test_find_not_found() {
        let mut buf: Buffer<8> = Buffer::new();
        assert!(buf.push(&[1, 2, 3, 4]).is_ok());
        assert_eq!(buf.find(&[5]), None);
    }

    #[test]
    fn test_find_empty_needle() {
        let buf: Buffer<4> = Buffer::new();
        // windows(0) produces one match at position 0 by convention
        assert_eq!(buf.find(&[]), Some(0));
    }

    #[test]
    fn test_find_needle_larger_than_buffer() {
        let buf: Buffer<3> = Buffer::new();
        assert_eq!(buf.find(&[1, 2, 3, 4]), None);
    }

    #[test]
    fn test_copy_to_larger_dst() {
        let mut buf: Buffer<4> = Buffer::new();
        assert!(buf.push(&[10, 20, 30]).is_ok());
        let mut dst = [0u8; 8];
        let copied = buf.copy_to(&mut dst);
        assert_eq!(copied, 3);
        assert_eq!(&dst[..3], &[10, 20, 30]);
    }

    #[test]
    fn test_copy_to_smaller_dst() {
        let mut buf: Buffer<4> = Buffer::new();
        assert!(buf.push(&[10, 20, 30, 40]).is_ok());
        let mut dst = [0u8; 2];
        let copied = buf.copy_to(&mut dst); // n=5, but dst.len=2
        assert_eq!(copied, 2);
        assert_eq!(&dst, &[10, 20]);
    }

    #[test]
    fn test_copy_to_limited_by_n() {
        let mut buf: Buffer<4> = Buffer::new();
        assert!(buf.push(&[10, 20, 30, 40]).is_ok());
        let mut dst = [0u8; 8];
        let copied = buf.copy_to(&mut dst[..2]);
        assert_eq!(copied, 2);
        assert_eq!(&dst[..2], &[10, 20]);
    }

    #[test]
    fn test_copy_to_empty_buffer() {
        let buf: Buffer<4> = Buffer::new();
        let mut dst = [0u8; 4];
        let copied = buf.copy_to(&mut dst);
        assert_eq!(copied, 0);
        assert_eq!(dst, [0; 4]);
    }

    #[test]
    fn test_as_slice() {
        let mut buf: Buffer<8> = Buffer::new();
        let data = [1, 2, 3, 4];
        assert!(buf.push(&data).is_ok());
        assert_eq!(buf.as_slice(), &[1, 2, 3, 4]);
        assert!(buf.push(&data).is_ok());
        assert_eq!(buf.as_slice(), &[1, 2, 3, 4, 1, 2, 3, 4]);
    }
}

//! Filter-bank history (QMF and polyphase synthesis buffers): the latest
//! values, newest first, with room for new ones at the front each step.

/// A window of up to `max` values that moves back through a buffer of
/// twice that, so the kept values are copied once every `max / step`
/// steps instead of on every step.
pub struct History {
    buf: Vec<f32>,
    off: usize,
}

impl History {
    /// Zeros, for windows of up to `max` values.
    pub fn new(max: usize) -> History {
        History {
            buf: vec![0.0; 2 * max],
            off: max,
        }
    }

    /// The next window of `len` values (at most `max`): `step` new ones at
    /// the front, for the caller to fill, then the previous window's first
    /// `len - step`.
    #[inline]
    pub fn push(&mut self, len: usize, step: usize) -> &mut [f32] {
        assert!(step <= len && 2 * len <= self.buf.len());
        if self.off < step {
            let kept = len - step;
            let end = self.buf.len();
            self.buf.copy_within(self.off..self.off + kept, end - kept);
            self.off = end - kept;
        }
        self.off -= step;
        &mut self.buf[self.off..self.off + len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_latest_values_newest_first() {
        let (len, step) = (10, 3);
        let mut h = History::new(len);
        let mut reference = vec![0f32; len];
        for n in 0..50 {
            let w = h.push(len, step);
            for (i, v) in w[..step].iter_mut().enumerate() {
                *v = (n * step + i) as f32;
            }
            reference.rotate_right(step);
            for i in 0..step {
                reference[i] = (n * step + i) as f32;
            }
            assert_eq!(w, &reference[..], "step {n}");
        }
    }
}

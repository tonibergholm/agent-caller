//! A log-linear histogram of microsecond values (about 6% relative error).
const SUB: usize = 16;
const EXP: usize = 40;

#[derive(Clone)]
pub struct Hist {
    counts: Vec<u64>,
    n: u64,
    max: u64,
    sum: u128,
}

impl Default for Hist {
    fn default() -> Self {
        Hist { counts: vec![0; EXP * SUB], n: 0, max: 0, sum: 0 }
    }
}

fn bucket(v: u64) -> usize {
    if v < SUB as u64 {
        return v as usize;
    }
    let exp = 63 - v.leading_zeros() as usize; // >= 4
    let sub = ((v >> (exp - 4)) & (SUB as u64 - 1)) as usize;
    ((exp - 3) * SUB + sub).min(EXP * SUB - 1)
}

fn lower(b: usize) -> u64 {
    if b < SUB {
        return b as u64;
    }
    let exp = b / SUB + 3;
    let sub = (b % SUB) as u64;
    (1u64 << exp) | (sub << (exp - 4))
}

impl Hist {
    pub fn record(&mut self, us: u64) {
        self.counts[bucket(us)] += 1;
        self.n += 1;
        self.sum += us as u128;
        self.max = self.max.max(us);
    }
    pub fn merge(&mut self, o: &Hist) {
        for (a, b) in self.counts.iter_mut().zip(&o.counts) {
            *a += b;
        }
        self.n += o.n;
        self.sum += o.sum;
        self.max = self.max.max(o.max);
    }
    pub fn count(&self) -> u64 {
        self.n
    }
    pub fn max(&self) -> u64 {
        self.max
    }
    /// The value at quantile `q` in `[0, 1]`, as the bucket's lower bound.
    pub fn quantile(&self, q: f64) -> u64 {
        if self.n == 0 {
            return 0;
        }
        let want = ((self.n as f64 * q).ceil() as u64).clamp(1, self.n);
        let mut seen = 0;
        for (b, c) in self.counts.iter().enumerate() {
            seen += c;
            if seen >= want {
                return lower(b).min(self.max);
            }
        }
        self.max
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quantiles_are_close() {
        let mut h = Hist::default();
        for v in 1..=10_000u64 {
            h.record(v * 10);
        }
        let p50 = h.quantile(0.5) as f64;
        let p99 = h.quantile(0.99) as f64;
        assert!((p50 - 50_000.0).abs() / 50_000.0 < 0.07, "{p50}");
        assert!((p99 - 99_000.0).abs() / 99_000.0 < 0.07, "{p99}");
        assert_eq!(h.max(), 100_000);
    }
}

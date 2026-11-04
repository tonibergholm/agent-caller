//! `--key value` arguments, no dependencies.
use std::collections::HashMap;

pub struct Args(HashMap<String, String>);

impl Args {
    pub fn parse(items: &[String]) -> Args {
        let mut m = HashMap::new();
        let mut it = items.iter();
        while let Some(k) = it.next() {
            if let Some(k) = k.strip_prefix("--") {
                let v = it.next().cloned().unwrap_or_default();
                m.insert(k.to_owned(), v);
            }
        }
        Args(m)
    }
    pub fn str(&self, k: &str, d: &str) -> String {
        self.0.get(k).cloned().unwrap_or_else(|| d.to_owned())
    }
    pub fn opt(&self, k: &str) -> Option<String> {
        self.0.get(k).cloned()
    }
    pub fn f64(&self, k: &str, d: f64) -> f64 {
        self.0.get(k).and_then(|v| v.parse().ok()).unwrap_or(d)
    }
    pub fn usize(&self, k: &str, d: usize) -> usize {
        self.0.get(k).and_then(|v| v.parse().ok()).unwrap_or(d)
    }
}

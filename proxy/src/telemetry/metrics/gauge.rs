use std::fmt;

use super::FmtMetric;
use super::labels::FmtLabels;

/// An instaneous metric value.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Gauge(u64);

impl FmtMetric for Gauge {
    fn fmt_metric<L>(&self, f: &mut fmt::Formatter, name: &str, labels: &L) -> fmt::Result
    where
        L: FmtLabels,
    {
        f.write_str(name)?;
        if !labels.is_empty() {
            f.write_str("{")?;
            labels.fmt(f)?;
            f.write_str("}")?;
        }
        writeln!(f, " {}", self.0)
    }
}

impl Gauge {
    /// Increment the gauge by one.
    pub fn incr(&mut self) {
        if let Some(new_value) = self.0.checked_add(1) {
            (*self).0 = new_value;
        } else {
            warn!("Gauge overflow");
        }
    }

    /// Decrement the gauge by one.
    pub fn decr(&mut self) {
        if let Some(new_value) = self.0.checked_sub(1) {
            (*self).0 = new_value;
        } else {
            warn!("Gauge underflow");
        }
    }
}

impl From<u64> for Gauge {
    fn from(n: u64) -> Self {
        Gauge(n)
    }
}

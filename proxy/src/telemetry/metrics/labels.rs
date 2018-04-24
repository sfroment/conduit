use std::collections::HashMap;
use std::fmt::{self, Write};
use std::hash;
use std::sync::Arc;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DstLabels {
    formatted: Arc<str>,
    original: Arc<HashMap<String, String>>,
}

#[derive(Debug)]
pub struct AppendLabels<'a, A: FmtLabels + 'a, B: FmtLabels + 'a>(&'a A, &'a B);

pub trait FmtLabels {
    fn is_empty(&self) -> bool;

    fn fmt_labels(&self, f: &mut fmt::Formatter) -> fmt::Result;

    fn append<'a, B: FmtLabels>(&'a self, b: &'a B) -> AppendLabels<'a, Self, B>
    where
        Self: ::std::marker::Sized,
    {
        AppendLabels(self, b)
    }
}

// ===== impl DstLabels ====

impl DstLabels {
    pub fn new<I, S>(labels: I) -> Option<Self>
    where
        I: IntoIterator<Item=(S, S)>,
        S: fmt::Display,
    {
        let mut labels = labels.into_iter();

        if let Some((k, v)) = labels.next() {
            let mut original = HashMap::new();

            // Format the first label pair without a leading comma, since we
            // don't know where it is in the output labels at this point.
            let mut s = format!("dst_{}=\"{}\"", k, v);
            original.insert(format!("{}", k), format!("{}", v));

            // Format subsequent label pairs with leading commas, since
            // we know that we already formatted the first label pair.
            for (k, v) in labels {
                write!(s, ",dst_{}=\"{}\"", k, v)
                    .expect("writing to string should not fail");
                original.insert(format!("{}", k), format!("{}", v));
            }

            Some(DstLabels {
                formatted: Arc::from(s),
                original: Arc::new(original),
            })
        } else {
            // The iterator is empty; return None
            None
        }
    }

    pub fn as_map(&self) -> &HashMap<String, String> {
        &self.original
    }

    pub fn as_str(&self) -> &str {
        &self.formatted
    }
}

// Simply hash the formatted string and no other fields on `DstLabels`.
impl hash::Hash for DstLabels {
    fn hash<H: hash::Hasher>(&self, state: &mut H) {
        self.formatted.hash(state)
    }
}

impl FmtLabels for DstLabels {
    fn is_empty(&self) -> bool {
        self.formatted.is_empty()
    }

    fn fmt_labels(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(self.formatted.as_ref(), f)
    }
}

impl<L: FmtLabels> FmtLabels for Option<L> {
    fn is_empty(&self) -> bool {
        self.as_ref().map(|d| d.is_empty()).unwrap_or(true)
    }

    fn fmt_labels(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if let Some(ref dst) = self.as_ref() {
            dst.fmt_labels(f)?;
        }

        Ok(())
    }
}

impl<'a> FmtLabels for &'a str {
    fn is_empty(&self) -> bool {
        str::is_empty(self)
    }

    fn fmt_labels(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl<F> FmtLabels for F
where
    F: Fn(&mut fmt::Formatter) -> fmt::Result
{
    fn is_empty(&self) -> bool {
        false
    }

    fn fmt_labels(&self, f: &mut fmt::Formatter) -> fmt::Result {
        (self)(f)
    }
}

impl FmtLabels for () {
    fn is_empty(&self) -> bool {
        true
    }

    fn fmt_labels(&self, _: &mut fmt::Formatter) -> fmt::Result {
        Ok(())
    }
}

impl<'a, A: FmtLabels + 'a, B: FmtLabels + 'a> FmtLabels for AppendLabels<'a, A, B> {
    fn is_empty(&self) -> bool {
        self.0.is_empty() && self.1.is_empty()
    }

    fn fmt_labels(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match (self.0.is_empty(), self.1.is_empty()) {
            (true, true) => Ok(()),
            (false, true) => self.0.fmt_labels(f),
            (true, false) => self.1.fmt_labels(f),
            (false, false) => {
                self.0.fmt_labels(f)?;
                write!(f, ",")?;
                self.1.fmt_labels(f)
            }
        }
    }
}

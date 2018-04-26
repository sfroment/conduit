//! Records and serves Prometheus metrics.
//!
//! # A note on label formatting
//!
//! Prometheus labels are represented as a comma-separated list of values
//! Since the Conduit proxy labels its metrics with a fixed set of labels
//! which we know in advance, we represent these labels using a number of
//! `struct`s, all of which implement `fmt::Display`. Some of the label
//! `struct`s contain other structs which represent a subset of the labels
//! which can be present on metrics in that scope. In this case, the
//! `fmt::Display` impls for those structs call the `fmt::Display` impls for
//! the structs that they own. This has the potential to complicate the
//! insertion of commas to separate label values.
//!
//! In order to ensure that commas are added correctly to separate labels,
//! we expect the `fmt::Display` implementations for label types to behave in
//! a consistent way: A label struct is *never* responsible for printing
//! leading or trailing commas before or after the label values it contains.
//! If it contains multiple labels, it *is* responsible for ensuring any
//! labels it owns are comma-separated. This way, the `fmt::Display` impl for
//! any struct that represents a subset of the labels are position-agnostic;
//! they don't need to know if there are other labels before or after them in
//! the formatted output. The owner is responsible for managing that.
//!
//! If this rule is followed consistently across all structs representing
//! labels, we can add new labels or modify the existing ones without having
//! to worry about missing commas, double commas, or trailing commas at the
//! end of the label set (all of which will make Prometheus angry).
use std::fmt;
use std::sync::{Arc, Mutex};
use std::io::Write;

use deflate::CompressionOptions;
use deflate::write::GzEncoder;
use futures::future::{self, FutureResult};
use hyper::{self, Body, StatusCode};
use hyper::header::{AcceptEncoding, ContentEncoding, ContentType, Encoding, QualityItem};
use hyper::server::{
    Response as HyperResponse,
    Request as HyperRequest,
    Service as HyperService,
};

use ctx;
use telemetry::event::Event;

mod counter;
mod gauge;
mod help;
mod labels;
mod latency;
mod tree;

pub use self::gauge::Gauge;
pub use self::labels::DstLabels;
use self::labels::FmtLabels;

/// Tracks Prometheus metrics
#[derive(Debug)]
pub struct Record {
    metrics: Arc<Mutex<tree::Root>>,
}

/// Serve Prometheues metrics.
#[derive(Clone, Debug)]
pub struct Serve {
    metrics: Arc<Mutex<tree::Root>>,
}

trait FmtMetrics {
    fn fmt_metrics<L>(&self, f: &mut fmt::Formatter, labels: &L) -> fmt::Result
    where
        L : FmtLabels;
}

trait FmtMetric {
    fn fmt_metric<L>(&self, f: &mut fmt::Formatter, name: &str, labels: &L) -> fmt::Result
    where
        L : FmtLabels;
}

/// Construct the Prometheus metrics.
///
/// Returns the `Record` and `Serve` sides. The `Serve` side
/// is a Hyper service which can be used to create the server for the
/// scrape endpoint, while the `Record` side can receive updates to the
/// metrics by calling `record`.
pub fn new(process: &Arc<ctx::Process>) -> (Record, Serve) {
    let metrics = Arc::new(Mutex::new(tree::Root::new(process)));
    let agg = Record { metrics: metrics.clone() };
    let srv = Serve { metrics };
    (agg, srv)
}

// ===== impl Record =====

impl Record {
    pub fn record(&mut self, event: &Event) {
        self.metrics.lock()
            .expect("metrics lock")
            .record(event);
    }
}

// ===== impl Serve =====

impl Serve {
    fn is_gzip(req: &HyperRequest) -> bool {
        if let Some(accept_encodings) = req
            .headers()
            .get::<AcceptEncoding>()
        {
            return accept_encodings
                .iter()
                .any(|&QualityItem { ref item, .. }| item == &Encoding::Gzip)
        }
        false
    }
}

impl HyperService for Serve {
    type Request = HyperRequest;
    type Response = HyperResponse;
    type Error = hyper::Error;
    type Future = FutureResult<Self::Response, Self::Error>;

    fn call(&self, req: Self::Request) -> Self::Future {
        if req.path() != "/metrics" {
            return future::ok(HyperResponse::new()
                .with_status(StatusCode::NotFound));
        }

        let metrics = self.metrics.lock().expect("metrics lock");

        let rsp = if Self::is_gzip(&req) {
            trace!("gzipping metrics");
            let mut writer = GzEncoder::new(Vec::<u8>::new(), CompressionOptions::fast());
            if let Err(e) = write!(&mut writer, "{}", *metrics) {
                return future::err(e.into());
            }
            let buf = match writer.finish() {
                Err(e) => return future::err(e.into()),
                Ok(buf) => buf,
            };

            HyperResponse::new()
                .with_header(ContentEncoding(vec![Encoding::Gzip]))
                .with_header(ContentType::plaintext())
                .with_body(Body::from(buf))
        } else {
            let mut buf = Vec::<u8>::new();
            if let Err(e) = write!(&mut buf, "{}", *metrics) {
                return future::err(e.into());
            }

            HyperResponse::new()
                .with_header(ContentType::plaintext())
                .with_body(Body::from(buf))
        };

        future::ok(rsp)
    }
}

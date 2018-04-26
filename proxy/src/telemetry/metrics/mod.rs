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
use std::io::{self, Write};
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

use deflate::CompressionOptions;
use deflate::write::GzEncoder;
use futures::future::{self, FutureResult};
use hyper::{self, Body, StatusCode};
use hyper::header::{AcceptEncoding, ContentEncoding, ContentType, Encoding, QualityItem};
use hyper::server::{
    Request as HyperRequest,
    Response as HyperResponse,
    Service as HyperService,
};

use ctx;
use telemetry::event::Event;

mod counter;
mod gauge;
mod labels;
mod latency;
mod tree;

use self::counter::Counter;
use self::latency::Histogram;
pub use self::gauge::Gauge;
pub use self::labels::DstLabels;
use self::labels::FmtLabels;

/// Describes a metric.
struct Metric<'a, K> {
    name: &'a str,
    help: &'a str,
    _p: PhantomData<K>,
}

const PROCESS_START_TIME: Metric<Gauge> = Metric {
    name: "process_start_time_seconds",
    help: "The time the process started, in seconds since the Unix epoch.",
    _p: PhantomData,
};

const HTTP_REQUEST_TOTAL: Metric<Counter> = Metric {
    name: "request_total",
    help: "Total number of HTTP requests the proxy has routed.",
    _p: PhantomData,
};
const HTTP_RESPONSE_TOTAL: Metric<Counter> = Metric {
    name: "response_total",
    help: "Total number of HTTP resonses the proxy has served.",
    _p: PhantomData,
};
const HTTP_RESPONSE_LATENCY: Metric<Histogram> = Metric {
    name: "response_latency_ms",
    help: "HTTP request latencies, in milliseconds.",
    _p: PhantomData,
};

const TCP_READ_BYTES: Metric<Counter> = Metric {
    name: "tcp_read_bytes_total",
    help: "Total number of bytes read from peers.",
    _p: PhantomData,
};
const TCP_WRITE_BYTES: Metric<Counter> = Metric {
    name: "tcp_write_bytes_total",
    help: "Total number of bytes written to peers.",
    _p: PhantomData,
};
const TCP_OPEN_CONNECTIONS: Metric<Gauge> = Metric {
    name: "tcp_open_connections",
    help: "Currently open connections.",
    _p: PhantomData,
};
const TCP_OPEN_TOTAL: Metric<Counter> = Metric {
    name: "tcp_open_total",
    help: "Total number of opened connections.",
    _p: PhantomData,
};
const TCP_CLOSE_TOTAL: Metric<Counter> = Metric {
    name: "tcp_close_total",
    help: "Total number of closed connections.",
    _p: PhantomData,
};
const TCP_CONNECTION_DURATION: Metric<Histogram> = Metric {
    name: "tcp_connection_duration_ms",
    help: "Connection lifetimes, in milliseconds",
    _p: PhantomData,
};


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

    fn write_help<W: Write>(buf: &mut W) -> io::Result<()> {
        write!(buf, "{}", PROCESS_START_TIME)?;

        write!(buf, "{}", HTTP_REQUEST_TOTAL)?;
        write!(buf, "{}", HTTP_RESPONSE_TOTAL)?;
        write!(buf, "{}", HTTP_RESPONSE_LATENCY)?;

        write!(buf, "{}", TCP_OPEN_TOTAL)?;
        write!(buf, "{}", TCP_CLOSE_TOTAL)?;
        write!(buf, "{}", TCP_OPEN_CONNECTIONS)?;
        write!(buf, "{}", TCP_CONNECTION_DURATION)?;
        write!(buf, "{}", TCP_READ_BYTES)?;
        write!(buf, "{}", TCP_WRITE_BYTES)?;

        Ok(())
    }

    fn write_metrics<W: Write>(&self, buf: &mut W) -> io::Result<()> {
        Self::write_help(buf)?;
        write!(buf, "{}", *self.metrics.lock().expect("metrics lock"))
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

        let rsp = if Self::is_gzip(&req) {
            trace!("gzipping metrics");
            let mut writer = GzEncoder::new(Vec::<u8>::new(), CompressionOptions::fast());
            if let Err(e) = self.write_metrics(&mut writer) {
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
            if let Err(e) = self.write_metrics(&mut buf) {
                return future::err(e.into());
            }

            HyperResponse::new()
                .with_header(ContentType::plaintext())
                .with_body(Body::from(buf))
        };

        future::ok(rsp)
    }
}


impl<'a, K> Metric<'a, K> {
    fn fmt_help(&self, f: &mut fmt::Formatter, kind: &str) -> fmt::Result {
        writeln!(f, "# HELP {} {}", self.name, self.help)?;
        writeln!(f, "# TYPE {} {}", self.name, kind)
    }
}

impl<'a> fmt::Display for Metric<'a, Counter> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.fmt_help(f, "counter")
    }
}

impl<'a> fmt::Display for Metric<'a, Gauge> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.fmt_help(f, "gauge")
    }
}

impl<'a> fmt::Display for Metric<'a, Histogram> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.fmt_help(f, "histogram")
    }
}

impl<'a, M: FmtMetric> Metric<'a, M> {
    fn fmt_metric<L: FmtLabels>(&self, f: &mut fmt::Formatter, metric: &M, labels: &L) -> fmt::Result {
        metric.fmt_metric(f, self.name, labels)
    }
}

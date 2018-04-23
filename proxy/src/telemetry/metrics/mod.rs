//! Aggregates and serves Prometheus metrics.
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
use std::{fmt, time};
use std::sync::{Arc, Mutex};

use futures::future::{self, FutureResult};
use http;
use hyper;
use hyper::header::{ContentLength, ContentType};
use hyper::StatusCode;
use hyper::server::{
    Service as HyperService,
    Request as HyperRequest,
    Response as HyperResponse
};
use indexmap::{IndexMap};

use ctx;
use telemetry::event::{self, Event};

mod counter;
mod labels;
mod latency;

use self::counter::Counter;
use self::latency::Histogram;
pub use self::labels::DstLabels;

#[derive(Clone, Debug)]
struct MetricsTree {
    inbound: ProxyTree,
    outbound: ProxyTree,
    start_time: u64,
}

#[derive(Clone, Debug, Default)]
struct ProxyTree {
    by_destination: IndexMap<DestinationClass, DestinationTree>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct DestinationClass {
    labels: Option<DstLabels>,
}

#[derive(Clone, Debug, Default)]
struct DestinationTree {
    transport: TransportMetrics,

    http: IndexMap<HttpClass, HttpTree>,
}

#[derive(Clone, Debug, Default)]
struct TransportMetrics {
    accept_open_total: Counter,
    accept_close_total: Counter,
    connect_open_total: Counter,
    connect_close_total: Counter,
    connection_duration: Histogram,
    received_bytes: Counter,
    sent_bytes: Counter,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct HttpClass {
    authority: String,
}

#[derive(Clone, Debug, Default)]
pub struct HttpTree {
    request: HttpRequestMetrics,
    responses: IndexMap<HttpResponseClass, HttpResponseMetrics>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct HttpResponseClass {
    status_code: u16,
    grpc_status_code: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct HttpRequestMetrics {
    total: Counter,
}

#[derive(Clone, Debug, Default)]
pub struct HttpResponseMetrics {
    total: Counter,
    latency: Histogram,
    lifetime: Histogram,
}

/// Describes a metric.
#[derive(Clone, Debug)]
struct Desc {
    kind: &'static str,
    name: &'static str,
    help: &'static str,
}

/// Tracks Prometheus metrics
#[derive(Debug)]
pub struct Aggregate {
    metrics: Arc<Mutex<MetricsTree>>,
}

/// Serve Prometheues metrics.
#[derive(Clone, Debug)]
pub struct Serve {
    metrics: Arc<Mutex<MetricsTree>>,
}

/// Construct the Prometheus metrics.
///
/// Returns the `Aggregate` and `Serve` sides. The `Serve` side
/// is a Hyper service which can be used to create the server for the
/// scrape endpoint, while the `Aggregate` side can receive updates to the
/// metrics by calling `record_event`.
pub fn new(process: &Arc<ctx::Process>) -> (Aggregate, Serve) {
    let metrics = Arc::new(Mutex::new(MetricsTree::new(process)));
    (Aggregate::new(&metrics), Serve::new(&metrics))
}

// ===== impl Metrics =====

impl MetricsTree {
    pub fn new(process: &Arc<ctx::Process>) -> Self {
        let start_time = process.start_time
            .duration_since(time::UNIX_EPOCH)
            .expect(
                "process start time should not be before the beginning \
                 of the Unix epoch"
            )
            .as_secs();

        Self {
            inbound: ProxyTree { by_destination: IndexMap::new() },
            outbound: ProxyTree { by_destination: IndexMap::new() },
            start_time,
        }
    }

    fn proxy_mut(&mut self, proxy: &ctx::Proxy) -> &mut ProxyTree {
        match *proxy {
            ctx::Proxy::Inbound(_) => &mut self.inbound,
            ctx::Proxy::Outbound(_) => &mut self.outbound,
        }
    }

    fn record(&mut self, event: &Event) {
        trace!("Metrics::record({:?})", event);
        match *event {
            Event::StreamRequestOpen(ref req) => {
                self.proxy_mut(req.proxy().as_ref())
                    .destination_mut(Some(req.client().as_ref()))
                    .http_request_mut(req.as_ref())
                    .open();
            },

            Event::StreamRequestFail(ref req, ref fail) => {
                self.proxy_mut(req.proxy().as_ref())
                    .destination_mut(Some(req.client().as_ref()))
                    .http_request_mut(req.as_ref())
                    .fail(fail);
            },

            Event::StreamRequestEnd(ref req, ref end) => {
                self.proxy_mut(req.proxy().as_ref())
                    .destination_mut(Some(req.client().as_ref()))
                    .http_request_mut(req.as_ref())
                    .end(end);
            },

            Event::StreamResponseOpen(ref res, ref open) => {
                self.proxy_mut(res.proxy().as_ref())
                    .destination_mut(Some(res.client().as_ref()))
                    .http_response_mut(res.as_ref())
                    .open(open);
            }

            Event::StreamResponseEnd(ref res, ref end) => {
                self.proxy_mut(res.proxy().as_ref())
                    .destination_mut(Some(res.client().as_ref()))
                    .http_response_mut(res.as_ref())
                    .end(end);
            },

            Event::StreamResponseFail(ref res, ref fail) => {
                self.proxy_mut(res.proxy().as_ref())
                    .destination_mut(Some(res.client().as_ref()))
                    .http_response_mut(res.as_ref())
                    .fail(fail);
            },

            Event::TransportOpen(ref ctx) => {
                let dst = match *ctx.as_ref() {
                    ctx::transport::Ctx::Client(dst) => Some(dst.as_ref()),
                    ctx::transport::Ctx::Server(_) => None,
                };
                self.proxy_mut(ctx.proxy().as_ref())
                    .destination_mut(dst)
                    .transport_mut()
                    .open();
            },

            Event::TransportClose(ref ctx, ref close) => {
                let dst = match *ctx.as_ref() {
                    ctx::transport::Ctx::Client(dst) => Some(dst.as_ref()),
                    ctx::transport::Ctx::Server(_) => None,
                };
                self.proxy_mut(ctx.proxy().as_ref())
                    .destination_mut(dst)
                    .transport_mut()
                    .close(close);
            },
        };
    }
}

impl ProxyTree {
    fn destination_mut(&mut self, ctx: Option<&ctx::transport::Client>) -> &mut DestinationTree {
        let labels = ctx
            .and_then(|c| c.dst_labels.as_ref())
            .and_then(|w| w.borrow().clone());

        self.by_destination
            .entry(DestinationClass { labels })
            .or_insert_with(Default::default)
    }
}

impl DestinationTree {
    fn transport_mut(&mut self) -> &mut TransportMetrics {
        &mut self.transport
    }

    fn http_request_mut(&mut self, req: &ctx::http::Request) -> &mut HttpTree {
        let authority = req.uri
            .authority_part()
            .map(http::uri::Authority::to_string)
            .unwrap_or_else(String::new);

        self.http
            .entry(HttpClass { authority })
            .or_insert_with(Default::default)
    }

    fn http_response_mut(&mut self, rsp: &ctx::http::Response) -> &mut HttpResponseMetrics {
        let status = rsp.status.as_u16();
        let mut req = self.http_request_mut(rsp.request.as_ref());
        unimplemented!()
    }
}

impl HttpTree {
    fn open(&mut self) {
        unimplemented!()
    }

    fn end(&mut self, _: &event::StreamRequestEnd) {}

    fn fail(&mut self, _fail: &event::StreamRequestFail) {
        unimplemented!()
    }
}

impl HttpResponseMetrics {
    fn open(&mut self, _open: &event::StreamResponseOpen) {
        unimplemented!()
    }

    fn end(&mut self, _end: &event::StreamResponseEnd) {
        unimplemented!()
    }

    fn fail(&mut self, _fail: &event::StreamResponseFail) {
        unimplemented!()
    }
}

impl TransportMetrics {
    fn open(&mut self) {
        unimplemented!()
    }

    fn close(&mut self, _end: &event::TransportClose) {
        unimplemented!()
    }
}

// ===== impl Metric =====

impl Desc {
    pub fn new(name: &'static str, kind: &'static str, help: &'static str) -> Self {
        Self { name, kind, help, }
    }
}

impl fmt::Display for Desc {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f,
            "# HELP {name} {help}\n# TYPE {name} counter\n",
            name = self.name,
            help = self.help,
        )
    }
}

// ===== impl Aggregate =====

impl Aggregate {
    fn new(metrics: &Arc<Mutex<MetricsTree>>) -> Self {
        Self {
            metrics: metrics.clone(),
        }
    }

    pub fn record_event(&mut self, event: &Event) {
        self.metrics.lock()
            .expect("metrics lock")
            .record(event);
    }
}

    /// Observe the given event.
    // pub fn record_event(&mut self, event: &Event) {
    //     trace!("Metrics::record({:?})", event);
    //     match *event {
    //         Event::StreamRequestOpen(_) => {},

    //         Event::StreamRequestFail(ref req, ref fail) => {
    //             let labels = Arc::new(RequestLabels::new(req.as_ref()));
    //             self.update(|metrics| {
    //                 *metrics.request_duration(&labels) += fail.since_request_open;
    //                 *metrics.request_total(&labels).incr();
    //             })
    //         },

    //         Event::StreamRequestEnd(ref req, ref end) => {
    //             let labels = Arc::new(RequestLabels::new(req.as_ref()));
    //             self.update(|metrics| {
    //                 *metrics.request_total(&labels).incr();
    //                 *metrics.request_duration(&labels) += end.since_request_open;
    //             })
    //         },

    //         Event::StreamResponseOpen(ref res, ref open) => {
    //             let labels = Arc::new(ResponseLabels {res, None));
    //             self.update(|metrics| {
    //                 *metrics.response_total(&labels).incr();
    //                 *metrics.response_latency(&labels) += open.since_request_open;
    //             });
    //         }

    //         Event::StreamResponseEnd(ref res, ref end) => {
    //             let labels = Arc::new(ResponseLabels {
    //                 res,
    //                 end.grpc_status,
    //             ));
    //             self.update(|metrics| {
    //                 *metrics.response_duration(&labels) +=  end.since_response_open;
    //                 *metrics.response_latency(&labels) += end.since_request_open;
    //             });
    //         },

    //         Event::StreamResponseFail(ref res, ref fail) => {
    //             // TODO: do we care about the failure's error code here?
    //             let labels = Arc::new(ResponseLabels::fail(res.as_ref()));
    //             self.update(|metrics| {
    //                 *metrics.response_total(&labels).incr();
    //                 *metrics.response_duration(&labels) += fail.since_response_open;
    //                 *metrics.response_latency(&labels) += fail.since_request_open;
    //             });
    //         },

    //         Event::TransportOpen(ref ctx) => {
    //             let labels = Arc::new(TransportLabels {ctx));
    //             self.update(|metrics| match ctx.as_ref() {
    //                 &ctx::transport::Ctx::Server(_) => {
    //                     *metrics.tcp_accept_open_total(&labels).incr();
    //                 },
    //                 &ctx::transport::Ctx::Client(_) => {
    //                     *metrics.tcp_connect_open_total(&labels).incr();
    //                 },
    //             })
    //         },

    //         Event::TransportClose(ref ctx, ref close) => {
    //             // TODO: use the `clean` field in `close` to record whether or not
    //             // there was an error.
    //             let labels = Arc::new(TransportLabels {ctx));
    //             self.update(|metrics| {
    //                 *metrics.tcp_connection_duration(&labels) += close.duration;
    //                 *metrics.sent_bytes(&labels) += close.tx_bytes as u64;
    //                 *metrics.received_bytes(&labels) += close.tx_bytes as u64;
    //                 match ctx.as_ref() {
    //                     &ctx::transport::Ctx::Server(_) => {
    //                         *metrics.tcp_accept_close_total(&labels).incr();
    //                     },
    //                     &ctx::transport::Ctx::Client(_) => {
    //                         *metrics.tcp_connect_close_total(&labels).incr();
    //                     },
    //                 };
    //             })
    //         },
    //     };
    // }

// ===== impl Serve =====

impl Serve {
    fn new(metrics: &Arc<Mutex<MetricsTree>>) -> Self {
        Serve { metrics: metrics.clone() }
    }

    fn prometheus_metrics(&self) -> String {
        //let _metrics = self.metrics.lock().expect("metrics lock");
        unimplemented!()
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

        let body = self.prometheus_metrics();
        future::ok(HyperResponse::new()
            .with_header(ContentLength(body.len() as u64))
            .with_header(ContentType::plaintext())
            .with_body(body))
    }
}

const REQUEST_TOTAL_DESC: Desc = Desc {
    kind: "counter",
    name: "request_total",
    help: "A counter of the number of requests the proxy has received.",
};

const REQUEST_DURATION_DESC: Desc = Desc {
    kind: "histogram",
    name: "request_duration_ms",
    help: "A histogram of the duration of a request. This is measured from \
        when the request headers are received to when the request \
        stream has completed.",
};

const RESPONSE_TOTAL_DESC: Desc = Desc {
    kind: "counter",
    name: "response_total",
    help: "A counter of the number of responses the proxy has received.",
};

const RESPONSE_DURATION_MS_DESC: Desc = Desc {
    kind: "histogram",
    name: "response_duration_ms",
    help: "A histogram of the duration of a response. This is measured from \
        when the response headers are received to when the response \
        stream has completed.",
};

const RESPONSE_LATENCY_MS_DESC: Desc = Desc {
    kind: "histogram",
    name: "response_latency_ms",
    help: "A histogram of the total latency of a response. This is measured \
    from when the request headers are received to when the response \
    stream has completed.",
};

const TCP_ACCEPT_OPEN_TOTAL_DESC: Desc = Desc {
    kind: "counter",
    name: "tcp_accept_open_total",
    help: "A counter of the total number of transport connections which \
        have been accepted by the proxy.",
};

const TCP_ACCEPT_CLOSE_TOTAL_DESC: Desc = Desc {
    kind: "counter",
    name: "tcp_accept_close_total",
    help: "A counter of the total number of transport connections accepted \
        by the proxy which have been closed.",
};

const TCP_CONNECT_OPEN_TOTAL_DESC: Desc = Desc {
    kind: "counter",
    name: "tcp_connect_open_total",
    help: "A counter of the total number of transport connections which \
        have been opened by the proxy.",
};

const TCP_CONNECT_CLOSE_TOTAL_DESC: Desc = Desc {
    kind: "counter",
    name: "tcp_connect_close_total",
    help: "A counter of the total number of transport connections opened \
        by the proxy which have been closed.",
};

const TCP_CONNECTION_DURATION_MS_DESC: Desc = Desc {
    kind: "histogram",
    name: "tcp_connection_duration_ms",
    help: "A histogram of the duration of the lifetime of a connection, in milliseconds",
};

const RECEIVED_BYTES_DESC: Desc = Desc {
    kind: "counter",
    name: "received_bytes",
    help: "A counter of the total number of recieved bytes."
};

const SENT_BYTES_DESC: Desc = Desc {
    kind: "counter",
    name: "sent_bytes",
    help: "A counter of the total number of sent bytes."
};

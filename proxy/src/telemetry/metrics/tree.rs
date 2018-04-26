use http;
use indexmap::IndexMap;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use ctx;
use telemetry::event::{self, Event};

use super::FmtMetrics;
use super::counter::Counter;
use super::gauge::Gauge;
use super::labels::{DstLabels, FmtLabels, FmtLabelsFn, NoLabels};
use super::latency::Histogram;

const SUCCESS_CLASS: &'static str = "classification=\"success\"";
const FAILURE_CLASS: &'static str = "classification=\"failure\"";

#[derive(Clone, Debug)]
pub struct Root {
    inbound: ProxyTree,
    outbound: ProxyTree,
    start_time: Gauge,
}

#[derive(Clone, Debug, Default)]
struct ProxyTree {
    by_dst: IndexMap<DstClass, DstTree>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct DstClass {
    labels: Option<DstLabels>,
}

#[derive(Clone, Debug, Default)]
struct DstTree {
    src_tcp_metrics: TransportTree,
    dst_tcp_metrics: TransportTree,

    by_http_request: IndexMap<HttpRequestClass, HttpRequestTree>,
}

#[derive(Clone, Debug, Default)]
struct TransportTree {
    open_total: Counter,
    open_active: Gauge,
    rx_bytes_total: Counter,
    tx_bytes_total: Counter,

    by_end: IndexMap<TransportEndClass, TransportEndMetrics>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum TransportEndClass {
    Success,
    Failure,
}

#[derive(Clone, Debug, Default)]
struct TransportEndMetrics {
    close_total: Counter,
    lifetime: Histogram,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct HttpRequestClass {
    authority: String,
}

#[derive(Clone, Debug, Default)]
pub struct HttpRequestTree {
    metrics: HttpRequestMetrics,
    by_response: IndexMap<HttpResponseClass, HttpResponseTree>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum HttpResponseClass {
    Response { status_code: u16 },
    Error { reason: &'static str },
}

#[derive(Clone, Debug, Default)]
pub struct HttpRequestMetrics {
    total: Counter,
}

#[derive(Clone, Debug, Default)]
pub struct HttpResponseTree {
    by_end: IndexMap<HttpEndClass, HttpEndMetrics>,
    // TODO track latency here?
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum HttpEndClass {
    Eos,
    Grpc { status_code: u32 },
    Error { reason: &'static str },
}

#[derive(Clone, Debug, Default)]
pub struct HttpEndMetrics {
    total: Counter,
    latency: Histogram,
}

// ===== impl Root =====

impl Root {
    pub fn new(process: &Arc<ctx::Process>) -> Self {
        let t0 = process
            .start_time
            .duration_since(UNIX_EPOCH)
            .expect("process start after the Unix epoch")
            .as_secs();

        Self {
            inbound: ProxyTree::default(),
            outbound: ProxyTree::default(),
            start_time: t0.into(),
        }
    }

    fn proxy_mut(&mut self, proxy: &ctx::Proxy) -> &mut ProxyTree {
        match *proxy {
            ctx::Proxy::Inbound(_) => &mut self.inbound,
            ctx::Proxy::Outbound(_) => &mut self.outbound,
        }
    }

    pub fn record(&mut self, event: &Event) {
        trace!("Metrics::record({:?})", event);
        match *event {
            Event::TransportOpen(ref ctx) => {
                let dst = match ctx.as_ref() {
                    &ctx::transport::Ctx::Client(ref dst) => Some(dst.as_ref()),
                    &ctx::transport::Ctx::Server(_) => None,
                };
                self.proxy_mut(ctx.proxy().as_ref())
                    .dst_mut(dst)
                    .transport_mut(ctx.as_ref())
                    .open();
            },

            Event::TransportClose(ref ctx, ref close) => {
                let dst = match ctx.as_ref() {
                    &ctx::transport::Ctx::Client(ref dst) => Some(dst.as_ref()),
                    &ctx::transport::Ctx::Server(_) => None,
                };
                self.proxy_mut(ctx.proxy().as_ref())
                    .dst_mut(dst)
                    .transport_mut(ctx.as_ref())
                    .close(close);
            },

            Event::StreamRequestOpen(ref req) => {
                self.proxy_mut(req.proxy().as_ref())
                    .dst_mut(Some(req.client().as_ref()))
                    .http_request_mut(req.as_ref())
                    .open();
            },

            Event::StreamRequestFail(ref req, ref fail) => {
                self.proxy_mut(req.proxy().as_ref())
                    .dst_mut(Some(req.client().as_ref()))
                    .http_request_mut(req.as_ref())
                    .fail(fail);
            },

            Event::StreamRequestEnd(ref req, ref end) => {
                self.proxy_mut(req.proxy().as_ref())
                    .dst_mut(Some(req.client().as_ref()))
                    .http_request_mut(req.as_ref())
                    .end(end);
            },

            Event::StreamResponseOpen(ref res, ref open) => {
                self.proxy_mut(res.proxy().as_ref())
                    .dst_mut(Some(res.client().as_ref()))
                    .http_response_mut(res.as_ref())
                    .open(open);
            },

            Event::StreamResponseEnd(ref res, ref end) => {
                self.proxy_mut(res.proxy().as_ref())
                    .dst_mut(Some(res.client().as_ref()))
                    .http_response_mut(res.as_ref())
                    .end(end);
            },

            Event::StreamResponseFail(ref res, ref fail) => {
                self.proxy_mut(res.proxy().as_ref())
                    .dst_mut(Some(res.client().as_ref()))
                    .http_response_mut(res.as_ref())
                    .fail(fail);
            },
        };
    }
}

impl fmt::Display for Root {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        super::PROCESS_START_TIME.fmt_metric(f, &self.start_time, &NoLabels)?;

        self.inbound.fmt_metrics(f, &"direction=\"inbound\"")?;
        self.outbound.fmt_metrics(f, &"direction=\"outbound\"")?;

        Ok(())
    }
}

impl ProxyTree {
    fn dst_mut(&mut self, ctx: Option<&ctx::transport::Client>) -> &mut DstTree {
        let labels = ctx.and_then(|c| c.dst_labels.as_ref())
            .and_then(|w| w.borrow().clone());

        self.by_dst
            .entry(DstClass { labels })
            .or_insert_with(Default::default)
    }
}

impl FmtMetrics for ProxyTree {
    fn fmt_metrics<L: FmtLabels>(&self, f: &mut fmt::Formatter, labels: &L) -> fmt::Result {
        for (ref class, ref tree) in &self.by_dst {
            match class.labels.as_ref() {
                Some(l) => tree.fmt_metrics(f, &labels.append(l)),
                None => tree.fmt_metrics(f, labels),
            }?;
        }

        Ok(())
    }
}

impl DstTree {
    fn transport_mut(&mut self, ctx: &ctx::transport::Ctx) -> &mut TransportTree {
        match *ctx {
            ctx::transport::Ctx::Client(_) => &mut self.dst_tcp_metrics,
            ctx::transport::Ctx::Server(_) => &mut self.src_tcp_metrics,
        }
    }

    fn http_request_mut(&mut self, req: &ctx::http::Request) -> &mut HttpRequestTree {
        let authority = req.uri
            .authority_part()
            .map(http::uri::Authority::to_string)
            .unwrap_or_else(String::new);

        self.by_http_request
            .entry(HttpRequestClass { authority })
            .or_insert_with(Default::default)
    }

    fn http_response_mut(&mut self, rsp: &ctx::http::Response) -> &mut HttpResponseTree {
        let status_code = rsp.status.as_u16();

        self.http_request_mut(rsp.request.as_ref())
            .by_response
            .entry(HttpResponseClass::Response { status_code })
            .or_insert_with(Default::default)
    }
}

impl FmtMetrics for DstTree {
    fn fmt_metrics<L>(&self, f: &mut fmt::Formatter, labels: &L) -> fmt::Result
    where
        L: FmtLabels,
    {
        self.src_tcp_metrics
            .fmt_metrics(f, &labels.append(&"peer=\"src\""))?;
        self.dst_tcp_metrics
            .fmt_metrics(f, &labels.append(&"peer=\"dst\""))?;

        for (ref class, ref tree) in &self.by_http_request {
            let authority = FmtLabelsFn::from(|f: &mut fmt::Formatter| {
                write!(f, "authority=\"{}\"", class.authority)
            });
            tree.fmt_metrics(f, &labels.append(&authority))?;
        }

        Ok(())
    }
}

const H2_REASONS: &'static [&'static str] = &[
    "NO_ERROR",
    "PROTOCOL_ERROR",
    "INTERNAL_ERROR",
    "FLOW_CONTROL_ERROR",
    "SETTINGS_TIMEOUT",
    "STREAM_CLOSED",
    "FRAME_SIZE_ERROR",
    "REFUSED_STREAM",
    "CANCEL",
    "COMPRESSION_ERROR",
    "CONNECT_ERROR",
    "ENHANCE_YOUR_CALM",
    "INADEQUATE_SECURITY",
    "HTTP_1_1_REQUIRED",
    "UNKNOWN",
];

impl HttpRequestTree {
    fn open(&mut self) {
        self.metrics.total.incr();
    }

    fn end(&mut self, _: &event::StreamRequestEnd) {}

    fn fail(&mut self, fail: &event::StreamRequestFail) {
        let reason = {
            let code = {
                let c: u32 = fail.error.into();
                c as usize
            };
            let idx = if code < H2_REASONS.len() {
                code as usize
            } else {
                H2_REASONS.len() - 1
            };
            H2_REASONS[idx]
        };

        let rsp = self.by_response
            .entry(HttpResponseClass::Error { reason })
            .or_insert_with(Default::default);

        let end = rsp.by_end
            .entry(HttpEndClass::Error { reason })
            .or_insert_with(Default::default);

        end.add(fail.since_request_open);
    }
}

impl FmtMetrics for HttpRequestTree {
    fn fmt_metrics<L>(&self, f: &mut fmt::Formatter, labels: &L) -> fmt::Result
    where
        L: FmtLabels,
    {
        super::HTTP_REQUEST_TOTAL.fmt_metric(f, &self.metrics.total, labels)?;

        for (ref class, ref tree) in &self.by_response {
            tree.fmt_metrics(f, class, labels)?;
        }

        Ok(())
    }
}

impl HttpResponseTree {
    fn open(&mut self, _: &event::StreamResponseOpen) {}

    fn end(&mut self, end: &event::StreamResponseEnd) {
        let class = match end.grpc_status {
            Some(status_code) => HttpEndClass::Grpc { status_code },
            None => HttpEndClass::Eos,
        };

        self.by_end
            .entry(class)
            .or_insert_with(Default::default)
            .add(end.since_request_open)
    }

    fn fail(&mut self, fail: &event::StreamResponseFail) {
        let reason = {
            let code = {
                let c: u32 = fail.error.into();
                c as usize
            };
            let idx = if code < H2_REASONS.len() {
                code
            } else {
                H2_REASONS.len() - 1
            };
            H2_REASONS[idx]
        };

        self.by_end
            .entry(HttpEndClass::Error { reason })
            .or_insert_with(Default::default)
            .add(fail.since_request_open)
    }

    fn fmt_metrics<L>(
        &self,
        f: &mut fmt::Formatter,
        rsp_class: &HttpResponseClass,
        labels: &L,
    ) -> fmt::Result
    where
        L: FmtLabels,
    {
        for (ref end_class, ref metrics) in &self.by_end {
            let rsp_labels = FmtLabelsFn::from(|f: &mut fmt::Formatter| match *rsp_class {
                HttpResponseClass::Error { reason } => {
                    f.write_str(FAILURE_CLASS)?;
                    write!(f, "error=\"{}\"", reason)
                },

                HttpResponseClass::Response {
                    status_code: http_status,
                } => match *end_class {
                    &HttpEndClass::Eos => {
                        f.write_str(if http_status < 500 {
                            SUCCESS_CLASS
                        } else {
                            FAILURE_CLASS
                        })?;
                        write!(f, ",status_code=\"{}\"", http_status)
                    },

                    &HttpEndClass::Grpc {
                        status_code: grpc_status,
                    } => {
                        f.write_str(if grpc_status == 0 {
                            SUCCESS_CLASS
                        } else {
                            FAILURE_CLASS
                        })?;
                        write!(f, ",status_code=\"{}\"", http_status)?;
                        write!(f, ",grpc_status_code=\"{}\"", grpc_status)
                    },

                    &HttpEndClass::Error { reason } => {
                        write!(f, "{},", FAILURE_CLASS)?;
                        write!(f, "error=\"{}\"", reason)
                    },
                },
            });

            metrics.fmt_metrics(f, &labels.append(&rsp_labels))?;
        }

        Ok(())
    }
}

impl HttpEndMetrics {
    fn add(&mut self, latency: Duration) {
        self.total.incr();
        self.latency.observe(latency);
    }
}

impl FmtMetrics for HttpEndMetrics {
    fn fmt_metrics<L>(&self, f: &mut fmt::Formatter, labels: &L) -> fmt::Result
    where
        L: FmtLabels,
    {
        super::HTTP_RESPONSE_LATENCY.fmt_metric(f, &self.latency, labels)?;
        super::HTTP_RESPONSE_TOTAL.fmt_metric(f, &self.total, labels)?;

        Ok(())
    }
}

impl TransportTree {
    fn open(&mut self) {
        self.open_total.incr();
        self.open_active.incr();
    }

    fn close(&mut self, close: &event::TransportClose) {
        self.open_active.decr();
        self.rx_bytes_total += close.rx_bytes;
        self.tx_bytes_total += close.tx_bytes;

        let class = if close.clean {
            TransportEndClass::Success
        } else {
            TransportEndClass::Failure
        };
        let end = self.by_end.entry(class).or_insert_with(Default::default);
        end.lifetime.observe(close.duration);
        end.close_total.incr();
    }
}

impl FmtMetrics for TransportTree {
    fn fmt_metrics<L>(&self, f: &mut fmt::Formatter, labels: &L) -> fmt::Result
    where
        L: FmtLabels,
    {
        super::TCP_OPEN_TOTAL.fmt_metric(f, &self.open_total, labels)?;
        super::TCP_OPEN_CONNECTIONS.fmt_metric(f, &self.open_active, labels)?;
        super::TCP_READ_BYTES.fmt_metric(f, &self.rx_bytes_total, labels)?;
        super::TCP_WRITE_BYTES.fmt_metric(f, &self.tx_bytes_total, labels)?;

        for (ref class, ref metrics) in &self.by_end {
            use self::TransportEndClass::*;
            let l = match *class {
                &Success => SUCCESS_CLASS,
                &Failure => FAILURE_CLASS,
            };

            metrics.fmt_metrics(f, &labels.append(&l))?;
        }

        Ok(())
    }
}

impl FmtMetrics for TransportEndMetrics {
    fn fmt_metrics<L>(&self, f: &mut fmt::Formatter, labels: &L) -> fmt::Result
    where
        L: FmtLabels,
    {
        super::TCP_CLOSE_TOTAL.fmt_metric(f, &self.close_total, labels)?;
        super::TCP_CONNECTION_DURATION.fmt_metric(f, &self.lifetime, labels)?;

        Ok(())
    }
}

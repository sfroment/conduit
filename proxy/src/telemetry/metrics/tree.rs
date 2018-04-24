use http;
use indexmap::IndexMap;
use std::fmt::{self, Write};
use std::sync::Arc;
use std::time::{UNIX_EPOCH, Duration};

use ctx;
use telemetry::event::{self, Event};

use super::counter::Counter;
use super::help;
use super::labels::{DstLabels, FmtLabels};
use super::latency::Histogram;

#[derive(Clone, Debug)]
pub struct Root {
    inbound: ProxyTree,
    outbound: ProxyTree,
    start_time: u64,
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
    accept_metrics: TransportMetrics,
    connect_metrics: TransportMetrics,

    by_http_request: IndexMap<HttpRequestClass, HttpRequestTree>,
}

#[derive(Clone, Debug, Default)]
struct TransportMetrics {
    open_total: Counter,
    close_total: Counter,
    lifetime: Histogram,
    rx_bytes_total: Counter,
    tx_bytes_total: Counter,
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
        let start_time = process.start_time
            .duration_since(UNIX_EPOCH)
            .expect(
                "process start time should not be before the beginning \
                 of the Unix epoch"
            )
            .as_secs();

        Self {
            inbound: ProxyTree { by_dst: IndexMap::new() },
            outbound: ProxyTree { by_dst: IndexMap::new() },
            start_time,
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
            }

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
        let mut out = String::new();

        write!(out, "{}", help::HTTP)?;
        write!(out, "{}", help::TCP)?;

        self.inbound.prometheus_fmt(f, &"direction=\"inbound\"")?;
        self.outbound.prometheus_fmt(f, &"direction=\"outbound\"")?;

        writeln!(out, "")?;
        writeln!(out, "process_start_time_seconds {}", self.start_time)?;

        Ok(())
    }
}

impl ProxyTree {
    fn dst_mut(&mut self, ctx: Option<&ctx::transport::Client>) -> &mut DstTree {
        let labels = ctx
            .and_then(|c| c.dst_labels.as_ref())
            .and_then(|w| w.borrow().clone());

        self.by_dst
            .entry(DstClass { labels })
            .or_insert_with(Default::default)
    }

    fn prometheus_fmt<L>(&self, f: &mut fmt::Formatter, labels: &L) -> fmt::Result
    where
        L: FmtLabels + Clone
    {
        for (ref class, ref tree) in &self.by_dst {
            let dst_labels = labels.append(&class.labels);
            tree.prometheus_fmt(f, &dst_labels)?;
        }

        Ok(())
    }
}

impl DstTree {
    fn transport_mut(&mut self, ctx: &ctx::transport::Ctx) -> &mut TransportMetrics {
        match *ctx {
            ctx::transport::Ctx::Client(_) => &mut self.connect_metrics,
            ctx::transport::Ctx::Server(_) => &mut self.accept_metrics
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

    fn prometheus_fmt<L>(&self, f: &mut fmt::Formatter, labels: &L) -> fmt::Result
    where
        L: FmtLabels
    {
        self.accept_metrics.prometheus_fmt(f, labels)?;
        self.connect_metrics.prometheus_fmt(f, labels)?;

        for (ref class, ref tree) in &self.by_http_request {
            let label_authority = |f: &mut fmt::Formatter| {
                write!(f, "authority=\"{}\"", class.authority)
            };
            let labels = labels.append(&label_authority);
            tree.prometheus_fmt(f, &labels)?;
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

    fn prometheus_fmt<L>(&self, f: &mut fmt::Formatter, labels: &L) -> fmt::Result
    where
        L: FmtLabels
    {
        self.metrics.total.prometheus_fmt(f, "request_total", labels)?;

        for (ref class, ref tree) in &self.by_response {
            let status_label = |f: &mut fmt::Formatter| {
                match *class {
                    &HttpResponseClass::Response { status_code } =>
                        write!(f, "status_code=\"{}\"", status_code),
                    &HttpResponseClass::Error { reason } =>
                        write!(f, "error=\"{}\"", reason),
                }
            };
            tree.prometheus_fmt(f, &labels.append(&status_label))?;
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

    fn prometheus_fmt<L>(&self, f: &mut fmt::Formatter, labels: &L) -> fmt::Result
    where
        L: FmtLabels
    {
        for (ref class, ref metrics) in &self.by_end {
            let end_label = |f: &mut fmt::Formatter| {
                match *class {
                    &HttpEndClass::Eos => Ok(()),
                    &HttpEndClass::Grpc { status_code } =>
                        write!(f, "grpc_status_code=\"{}\"", status_code),
                    &HttpEndClass::Error { reason } =>
                        write!(f, "error=\"{}\"", reason),
                }
            };

            metrics.prometheus_fmt(f, &labels.append(&end_label))?;
        }

        Ok(())
    }
}

impl HttpEndMetrics {
    fn add(&mut self, latency: Duration) {
        self.total.incr();
        self.latency.observe(latency);
    }

    fn prometheus_fmt<L>(&self, f: &mut fmt::Formatter, labels: &L) -> fmt::Result
    where
        L: FmtLabels
    {
        self.total.prometheus_fmt(f, "response_total", labels)?;
        self.latency.prometheus_fmt(f, "response_latency_ms", labels)?;

        Ok(())
    }
}

impl TransportMetrics {
    fn open(&mut self) {
        self.open_total.incr();
    }

    fn close(&mut self, close: &event::TransportClose) {
        self.lifetime.observe(close.duration);
        self.rx_bytes_total += close.rx_bytes;
        self.tx_bytes_total += close.tx_bytes;
        self.close_total.incr();
    }

    fn prometheus_fmt<L>(&self, f: &mut fmt::Formatter, labels: &L) -> fmt::Result
    where
        L: FmtLabels
    {
        self.open_total.prometheus_fmt(f, "tcp_open_total", labels)?;
        self.close_total.prometheus_fmt(f, "tcp_open_total", labels)?;
        self.lifetime.prometheus_fmt(f, "tcp_connection_duration_ms", labels)?;

        Ok(())
    }
}

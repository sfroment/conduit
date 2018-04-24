use http;
use indexmap::IndexMap;
use std::sync::Arc;
use std::time;

use ctx;
use telemetry::event::{self, Event};

use super::counter::Counter;
use super::labels::DstLabels;
use super::latency::Histogram;

#[derive(Clone, Debug)]
pub struct Root {
    inbound: ProxyTree,
    outbound: ProxyTree,
    start_time: u64,
}

#[derive(Clone, Debug, Default)]
struct ProxyTree {
    by_destination: IndexMap<DstClass, DstTree>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct DstClass {
    labels: Option<DstLabels>,
}

#[derive(Clone, Debug, Default)]
struct DstTree {
    transport: TransportMetrics,

    by_http_request: IndexMap<HttpClass, HttpTree>,
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
    by_response: IndexMap<HttpResponseClass, HttpResponseMetrics>,
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

// ===== impl Root =====

impl Root {
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

    pub fn record(&mut self, event: &Event) {
        trace!("Metrics::record({:?})", event);
        match *event {

            Event::TransportOpen(ref ctx) => {
                let dst = match ctx.as_ref() {
                    &ctx::transport::Ctx::Client(ref dst) => Some(dst.as_ref()),
                    &ctx::transport::Ctx::Server(_) => None,
                };
                self.proxy_mut(ctx.proxy().as_ref())
                    .destination_mut(dst)
                    .transport_mut()
                    .open();
            },

            Event::TransportClose(ref ctx, ref close) => {
                let dst = match ctx.as_ref() {
                    &ctx::transport::Ctx::Client(ref dst) => Some(dst.as_ref()),
                    &ctx::transport::Ctx::Server(_) => None,
                };
                self.proxy_mut(ctx.proxy().as_ref())
                    .destination_mut(dst)
                    .transport_mut()
                    .close(close);
            },

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
        };
    }
}

impl ProxyTree {
    fn destination_mut(&mut self, ctx: Option<&ctx::transport::Client>) -> &mut DstTree {
        let labels = ctx
            .and_then(|c| c.dst_labels.as_ref())
            .and_then(|w| w.borrow().clone());

        self.by_destination
            .entry(DstClass { labels })
            .or_insert_with(Default::default)
    }
}

impl DstTree {
    fn transport_mut(&mut self) -> &mut TransportMetrics {
        &mut self.transport
    }

    fn http_request_mut(&mut self, req: &ctx::http::Request) -> &mut HttpTree {
        let authority = req.uri
            .authority_part()
            .map(http::uri::Authority::to_string)
            .unwrap_or_else(String::new);

        self.by_http_request
            .entry(HttpClass { authority })
            .or_insert_with(Default::default)
    }

    fn http_response_mut(&mut self, rsp: &ctx::http::Response) -> &mut HttpResponseMetrics {
        let _status = rsp.status.as_u16();
        let mut _req = self.http_request_mut(rsp.request.as_ref());
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

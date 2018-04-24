use std::fmt;

pub const HTTP: Section = Section(&[
    Help {
        kind: "counter",
        name: "request_total",
        help: "A counter of the number of requests the proxy has received.",
    },
    Help {
        kind: "counter",
        name: "response_total",
        help: "A counter of the number of responses the proxy has received.",
    },
    Help {
        kind: "histogram",
        name: "response_latency_ms",
        help: "A histogram of the total latency of a response. This is measured \
        from when the request headers are received to when the response \
        stream has completed.",
    },
]);

pub const TCP: Section = Section(&[
    Help {
        kind: "counter",
        name: "tcp_accept_open_total",
        help: "A counter of the total number of transport connections which \
            have been accepted by the proxy.",
    },
    Help {
        kind: "counter",
        name: "tcp_accept_close_total",
        help: "A counter of the total number of transport connections accepted \
            by the proxy which have been closed.",
    },
    Help {
        kind: "counter",
        name: "tcp_connect_open_total",
        help: "A counter of the total number of transport connections which \
            have been opened by the proxy.",
    },
    Help {
        kind: "counter",
        name: "tcp_connect_close_total",
        help: "A counter of the total number of transport connections opened \
            by the proxy which have been closed.",
    },
    Help {
        kind: "histogram",
        name: "tcp_connection_duration_ms",
        help: "A histogram of the duration of the lifetime of a connection, in milliseconds",
    },
    Help {
        kind: "counter",
        name: "received_bytes",
        help: "A counter of the total number of recieved bytes."
    },
    Help {
        kind: "counter",
        name: "sent_bytes",
        help: "A counter of the total number of sent bytes."
    },
]);

/// Describes a metric.
struct Help<'a> {
    kind: &'a str,
    name: &'a str,
    help: &'a str,
}

pub struct Section<'a>(&'a [Help<'a>]);

impl<'a> fmt::Display for Help<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f,
            "# HELP {name} {help}\n# TYPE {name} {kind}\n",
            name = self.name,
            kind = self.kind,
            help = self.help,
        )
    }
}

impl<'a> fmt::Display for Section<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for help in self.0 {
            help.fmt(f)?;
            writeln!(f, "")?;
        }
        Ok(())
    }
}

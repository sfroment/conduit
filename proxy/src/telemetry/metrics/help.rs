use std::fmt;

pub const HTTP: Section = Section(&[
    Help {
        name: super::HTTP_REQUEST_TOTAL_KEY,
        help: "Total number of HTTP requests the proxy has routed.",
        kind: Kind::Counter,
    },
    Help {
        name: super::HTTP_RESPONSE_TOTAL_KEY,
        help: "Total number of HTTP resonses the proxy has served.",
        kind: Kind::Counter,
    },
    Help {
        name: super::HTTP_RESPONSE_LATENCY_KEY,
        help: "HTTP request latencies, in milliseconds.",
        kind: Kind::Histogram,
    },
]);

pub const TCP: Section = Section(&[
    Help {
        name: super::TCP_OPEN_TOTAL_KEY,
        help: "Total number of opened connections.",
        kind: Kind::Counter,
    },
    Help {
        name: super::TCP_CLOSE_TOTAL_KEY,
        help: "Total number of closed connections.",
        kind: Kind::Counter,
    },
    Help {
        name: super::TCP_OPEN_CONNECTIONS_KEY,
        help: "Open connections.",
        kind: Kind::Gauge,
    },
    Help {
        name: super::TCP_CONNECTION_DURATION_KEY,
        help: "Connection lifetimes, in milliseconds",
        kind: Kind::Histogram,
    },
    Help {
        name: super::TCP_READ_BYTES_KEY,
        help: "Total number of bytes read from peers.",
        kind: Kind::Counter,
    },
    Help {
        name: super::TCP_WRITE_BYTES_KEY,
        help: "Total number of bytes written to peers.",
        kind: Kind::Counter,
    },
]);

pub const SYSTEM: Section = Section(&[
    Help {
        name: super::PROCESS_START_TIME_KEY,
        help: "Number of seconds since the Unix epoch at the time the process started.",
        kind: Kind::Gauge,
    }
]);

/// Describes a metric.
struct Help<'a> {
    name: &'a str,
    help: &'a str,
    kind: Kind,
}

enum Kind {
    Counter,
    Gauge,
    Histogram,
}

pub struct Section<'a>(&'a [Help<'a>]);

impl<'a> fmt::Display for Help<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f, "# HELP {} {}", self.name, self.help)?;
        writeln!(f, "# TYPE {} {}", self.name, match self.kind {
            Kind::Counter => "counter",
            Kind::Gauge => "gauge",
            Kind::Histogram => "histogram",
        })?;
        Ok(())
    }
}

impl<'a> fmt::Display for Section<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for help in self.0 {
            help.fmt(f)?;
            f.write_str("\n")?;
        }
        Ok(())
    }
}

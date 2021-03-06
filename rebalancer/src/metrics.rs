// Copyright 2020 Joyent, Inc.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Mutex;

use gethostname::gethostname;
use hyper::header::{HeaderValue, CONTENT_TYPE};
use hyper::rt::{self, Future};
use hyper::server::Server;
use hyper::service::service_fn_ok;
use hyper::Body;
use hyper::StatusCode;
use hyper::{Request, Response};
use lazy_static::lazy_static;
use prometheus::{
    opts, register_counter, register_counter_vec, register_histogram, Counter,
    CounterVec, Encoder, Gauge, Histogram, TextEncoder,
};
use serde_derive::Deserialize;
use slog::{error, info, Logger};

pub type MetricsMap = HashMap<&'static str, Metrics>;

pub static OBJECT_COUNT: &str = "object_count";
pub static ERROR_COUNT: &str = "error_count";
pub static REQUEST_COUNT: &str = "request_count";
pub static BYTES_COUNT: &str = "bytes_count";
pub static ASSIGNMENT_TIME: &str = "assignment_time";

#[derive(Clone, Deserialize)]
pub struct ConfigMetrics {
    /// Rebalancer metrics server address
    pub host: String,
    /// Rebalancer metrics server port
    pub port: u16,
    pub datacenter: String,
    pub service: String,
    pub server: String,
}

impl Default for ConfigMetrics {
    fn default() -> Self {
        Self {
            host: Ipv4Addr::UNSPECIFIED.to_string(),
            port: 8878,
            datacenter: "development".into(),
            service: "1.rebalancer.localhost".into(),
            server: "127.0.0.1".into(),
        }
    }
}

// This enum exists so that we can take various prometheus counter types as the
// same data type.  This is necessary so that we can store all metrics that we
// create in the same hash map regardless of the type of counter.  Note, not
// all prometheus counters are enumerated below.  Add them as needed.
#[derive(Clone)]
pub enum Metrics {
    MetricsCounterVec(CounterVec),
    MetricsCounter(Counter),
    MetricsGauge(Gauge),
    MetricsHistogram(Histogram),
}

lazy_static! {
    static ref METRICS_LABELS: Mutex<Option<HashMap<String, String>>> =
        Mutex::new(None);
}

pub fn gauge_inc<S: ::std::hash::BuildHasher>(
    metrics: &HashMap<&'static str, Metrics, S>,
    key: &str,
) {
    match metrics.get(key) {
        Some(metric) => {
            if let Metrics::MetricsGauge(g) = metric {
                g.inc();
            }
        }
        None => error!(slog_scope::logger(), "Invalid metric: {}", key),
    }
}

pub fn gauge_dec<S: ::std::hash::BuildHasher>(
    metrics: &HashMap<&'static str, Metrics, S>,
    key: &str,
) {
    match metrics.get(key) {
        Some(metric) => {
            if let Metrics::MetricsGauge(g) = metric {
                g.dec();
            }
        }
        None => error!(slog_scope::logger(), "Invalid metric: {}", key),
    }
}

pub fn gauge_set<S: ::std::hash::BuildHasher>(
    metrics: &HashMap<&'static str, Metrics, S>,
    key: &str,
    val: usize,
) {
    let num = val as f64;

    match metrics.get(key) {
        Some(metric) => {
            if let Metrics::MetricsGauge(g) = metric {
                g.set(num);
            }
        }
        None => error!(slog_scope::logger(), "Invalid metric: {}", key),
    }
}

#[allow(irrefutable_let_patterns)]
pub fn counter_vec_inc<S: ::std::hash::BuildHasher>(
    metrics: &HashMap<&'static str, Metrics, S>,
    key: &str,
    bucket: Option<&str>,
) {
    counter_vec_inc_by(metrics, key, bucket, 1);
}

#[allow(irrefutable_let_patterns)]
pub fn counter_vec_inc_by<S: ::std::hash::BuildHasher>(
    metrics: &HashMap<&'static str, Metrics, S>,
    key: &str,
    bucket: Option<&str>,
    val: usize,
) {
    let num = val as f64;
    match metrics.get(key) {
        Some(metric) => {
            if let Metrics::MetricsCounterVec(c) = metric {
                // Increment the total.
                c.with_label_values(&["total"]).inc_by(num);

                // If a bucket was supplied, increment that as well.  The
                // bucket will represent some subset of the total for the
                // metric.
                if let Some(b) = bucket {
                    c.with_label_values(&[b]).inc_by(num);
                }
            }
        }
        None => error!(slog_scope::logger(), "Invalid metric: {}", key),
    }
}

#[allow(irrefutable_let_patterns)]
pub fn counter_inc_by<S: ::std::hash::BuildHasher>(
    metrics: &HashMap<&'static str, Metrics, S>,
    key: &str,
    val: u64,
) {
    let num = val as f64;
    match metrics.get(key) {
        Some(metric) => {
            if let Metrics::MetricsCounter(c) = metric {
                c.inc_by(num);
            }
        }
        None => error!(slog_scope::logger(), "Invalid metric: {}", key),
    }
}

pub fn histogram_observe<S: ::std::hash::BuildHasher>(
    metrics: &HashMap<&'static str, Metrics, S>,
    key: &str,
    val: f64,
) {
    match metrics.get(key) {
        Some(metric) => {
            if let Metrics::MetricsHistogram(h) = metric {
                h.observe(val);
            }
        }
        None => error!(slog_scope::logger(), "Invalid metric: {}", key),
    }
}

// It would be nice if this could be a HashMap<&str, &str>, however Prometheus
// requires the type HashMap<String, String>, for const_labels, so here we are.
pub fn get_const_labels() -> &'static Mutex<Option<HashMap<String, String>>> {
    &METRICS_LABELS
}

// Given the service configuration information, create (i.e. register) the
// desired metrics with prometheus.
pub fn register_metrics(labels: &ConfigMetrics) -> MetricsMap {
    let hostname = gethostname()
        .into_string()
        .unwrap_or_else(|_| String::from("unknown"));

    let mut metrics = HashMap::new();

    // Convert our ConfigAgentMetrics structure to a HashMap since that is what
    // Prometheus requires when creating a new metric with labels.  It is a
    // Manta-wide convention to require the (below) labels at a minimum as a
    // part of all metrics.  Other labels can be added, but these are required.
    let mut const_labels = HashMap::new();
    const_labels.insert("service".to_string(), labels.service.clone());
    const_labels.insert("server".to_string(), labels.server.clone());
    const_labels.insert("datacenter".to_string(), labels.datacenter.clone());
    const_labels.insert("zonename".to_string(), hostname);

    let mut labels = METRICS_LABELS.lock().unwrap();
    *labels = Some(const_labels.clone());

    // The request counter maintains a list of requests received, broken down
    // by the type of request (e.g. req=GET, req=POST).
    let request_counter = register_counter_vec!(
        opts!(REQUEST_COUNT, "Total number of requests handled.")
            .const_labels(const_labels.clone()),
        &["req"]
    )
    .expect("failed to register incoming_request_count counter");

    metrics.insert(REQUEST_COUNT, Metrics::MetricsCounterVec(request_counter));

    // The object counter maintains a count of the total number of objects that
    // have been processed (whether successfully or not).
    let object_counter = register_counter_vec!(
        opts!(OBJECT_COUNT, "Total number of objects processed.")
            .const_labels(const_labels.clone()),
        &["type"]
    )
    .expect("failed to register object_count counter");

    metrics.insert(OBJECT_COUNT, Metrics::MetricsCounterVec(object_counter));

    // The error counter maintains a list of errors encountered, broken down by
    // the type of error observed.  Note that in order to avoid a polynomial
    // explosion of buckets here, one should have a reasonable idea of the
    // different kinds of possible errors that a given application could
    // encounter and in the event that there are too many possibilities, only
    // track certain error types and maintain the rest in a generic bucket.
    let error_counter = register_counter_vec!(
        opts!(ERROR_COUNT, "Errors encountered.")
            .const_labels(const_labels.clone()),
        &["error"]
    )
    .expect("failed to register error_count counter");

    metrics.insert(ERROR_COUNT, Metrics::MetricsCounterVec(error_counter));

    // Track total number of bytes transferred.
    let bytes_counter =
        register_counter!(opts!(BYTES_COUNT, "Bytes transferred.")
            .const_labels(const_labels.clone()))
        .expect("failed to register bytes_count counter");

    metrics.insert(BYTES_COUNT, Metrics::MetricsCounter(bytes_counter));

    let assignment_times = register_histogram!(histogram_opts!(
        ASSIGNMENT_TIME,
        "Assignment completion time"
    )
    .const_labels(const_labels))
    .expect("failed to register assignment_times counter");

    metrics
        .insert(ASSIGNMENT_TIME, Metrics::MetricsHistogram(assignment_times));

    metrics
}

// Start the metrics server on the address and port specified by the caller.
pub fn start_server(address: &str, port: u16, log: &Logger) {
    let addr = [&address, ":", &port.to_string()]
        .concat()
        .parse::<SocketAddr>()
        .unwrap();

    let log_clone = log.clone();

    let server = Server::bind(&addr)
        .serve(move || {
            service_fn_ok(move |_: Request<Body>| {
                // Gather all metrics from the default registry.
                let metric_families = prometheus::gather();
                let mut buffer = vec![];

                // Convert the MetricFamily message into text format and store
                // the result in `buffer' which will be in the payload of the
                // reponse to a request for metrics.
                let encoder = TextEncoder::new();
                encoder.encode(&metric_families, &mut buffer).unwrap();
                let content_type =
                    encoder.format_type().parse::<HeaderValue>().unwrap();

                // Send the response.
                Response::builder()
                    .header(CONTENT_TYPE, content_type)
                    .status(StatusCode::OK)
                    .body(Body::from(buffer))
                    .unwrap()
            })
        })
        .map_err(
            move |e| error!(log_clone, "metrics server error"; "error" => %e),
        );

    info!(log, "listening"; "address" => addr);

    rt::run(server);
}

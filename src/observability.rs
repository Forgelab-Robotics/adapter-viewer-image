use std::ffi::OsStr;
use std::io::Write;
use std::os::fd::AsFd;
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use dora_node_api::{MetadataParameters, Parameter};
use forge_common::metrics::{BoundedMetricsSink, MetricsConfig, MetricsSnapshot};
use forge_common::observability::{
    Clock, MetadataContainer, MetadataValueRef, Observer, ReceiveContext,
};

pub struct Observation {
    observer: Arc<Observer>,
    stop: mpsc::Sender<()>,
    done: mpsc::Receiver<()>,
    reporter: Option<JoinHandle<()>>,
}

impl Observation {
    pub fn from_env() -> Option<Self> {
        if !enabled(std::env::var_os("FORGE_OBSERVABILITY").as_deref()) {
            return None;
        }
        let sink = Arc::new(
            BoundedMetricsSink::new(MetricsConfig {
                max_label_bytes: crate::config::MAX_INPUT_ID_LEN,
                ..MetricsConfig::default()
            })
            .expect("fixed metrics configuration is valid"),
        );
        let observer = Arc::new(Observer::new(Some(Arc::clone(&sink))));
        // A private descriptor avoids holding stdout/stderr's global Rust locks
        // across potentially blocked telemetry writes. Failure disables export only.
        let mut output = std::fs::File::from(std::io::stdout().as_fd().try_clone_to_owned().ok()?);
        let (stop, receiver) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        // Export belongs to the application, not the receive hot path or Common core.
        let reporter = match thread::Builder::new()
            .name("image-observability".into())
            .spawn(move || {
                loop {
                    let stopping = !matches!(
                        receiver.recv_timeout(Duration::from_secs(5)),
                        Err(mpsc::RecvTimeoutError::Timeout)
                    );
                    let _ = write_snapshot(&mut output, &sink.take_snapshot());
                    if stopping {
                        break;
                    }
                }
                let _ = finished.send(());
            }) {
            Ok(reporter) => reporter,
            Err(error) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "[image_viewer] observability unavailable: {error}"
                );
                return None;
            }
        };
        Some(Self {
            observer,
            stop,
            done,
            reporter: Some(reporter),
        })
    }

    pub fn observer(&self) -> Arc<Observer> {
        Arc::clone(&self.observer)
    }
}

impl Drop for Observation {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        // Export is best effort: a blocked reader must not keep the app alive.
        if self.done.recv_timeout(Duration::from_millis(250)).is_ok()
            && let Some(reporter) = self.reporter.take()
        {
            let _ = reporter.join();
        }
    }
}

fn enabled(value: Option<&OsStr>) -> bool {
    value == Some(OsStr::new("1"))
}

pub fn observe<C: Clock>(
    observer: &Observer<C>,
    input_id: &str,
    parameters: &MetadataParameters,
) -> ReceiveContext {
    observer.observe_receive(input_id, MetadataContainer::Mapping, |key| {
        parameters.get(key).map(|value| match value {
            Parameter::Integer(value) => MetadataValueRef::Integer(*value),
            Parameter::String(value) => MetadataValueRef::String(value),
            _ => MetadataValueRef::Other,
        })
    })
}

fn write_snapshot(writer: &mut impl Write, snapshot: &MetricsSnapshot) -> std::io::Result<()> {
    writeln!(
        writer,
        "[image_viewer] observability interval histograms={} counters={} invalid_inputs={} rejected_series={} saturated_updates={}",
        snapshot.histograms.len(),
        snapshot.counters.len(),
        snapshot.invalid_inputs,
        snapshot.rejected_series,
        snapshot.saturated_updates
    )?;
    for row in &snapshot.histograms {
        writeln!(
            writer,
            "[image_viewer] {} input={:?} count={} sum_ns={} min_ns={} max_ns={} buckets={:?}",
            row.metric.as_str(),
            row.input_id,
            row.count,
            row.sum_ns,
            row.min_ns,
            row.max_ns,
            row.buckets
        )?;
    }
    for row in &snapshot.counters {
        writeln!(
            writer,
            "[image_viewer] forge_observability_events_total reason={} input={:?} count={}",
            row.reason.as_str(),
            row.input_id,
            row.count
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_common::metrics::{EventReason, LatencyMetric};
    use forge_common::observability::{
        MetadataIssue, ORIGIN_TIME_KEY, PUBLISH_TIME_KEY, VERSION_KEY,
    };

    struct FixedClock;
    impl Clock for FixedClock {
        fn unix_time_ns(&self) -> Option<i64> {
            Some(150)
        }
        fn monotonic_time_ns(&self) -> Option<u64> {
            Some(10)
        }
    }

    #[test]
    fn reporter_shutdown_does_not_wait_forever_for_blocked_output() {
        let (stop, _receiver) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let reporter = thread::spawn(move || {
            let _ = blocked.recv();
            let _ = finished.send(());
        });
        let observation = Observation {
            observer: Arc::new(Observer::default()),
            stop,
            done,
            reporter: Some(reporter),
        };
        // Drop must return even though the simulated writer cannot finish until
        // the release signal below. The test command also has an external timeout.
        drop(observation);
        release.send(()).unwrap();
    }

    #[test]
    fn snapshot_output_errors_are_returned_and_empty_intervals_are_explicit() {
        struct BrokenWriter;
        impl Write for BrokenWriter {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let sink = BoundedMetricsSink::new(MetricsConfig::default()).unwrap();
        let snapshot = sink.take_snapshot();
        assert_eq!(
            write_snapshot(&mut BrokenWriter, &snapshot)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::BrokenPipe
        );
        let mut output = Vec::new();
        write_snapshot(&mut output, &snapshot).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("histograms=0 counters=0"));
        assert!(!text.contains("forge_hop_latency_seconds"));
    }

    #[test]
    fn only_exact_one_enables_observation() {
        assert!(!enabled(None));
        for value in ["", "0", "true", " 1", "1 "] {
            assert!(!enabled(Some(OsStr::new(value))));
        }
        assert!(enabled(Some(OsStr::new("1"))));
    }

    #[test]
    fn typed_dora_metadata_records_exact_receive_intervals() {
        let sink = Arc::new(BoundedMetricsSink::new(MetricsConfig::default()).unwrap());
        let observer = Observer::with_clock(FixedClock, Some(Arc::clone(&sink)));
        let parameters = MetadataParameters::from([
            (VERSION_KEY.into(), Parameter::Integer(1)),
            (PUBLISH_TIME_KEY.into(), Parameter::Integer(100)),
            (ORIGIN_TIME_KEY.into(), Parameter::Integer(20)),
            ("capture_timestamp_ns".into(), Parameter::Integer(1)),
        ]);
        let context = observe(&observer, "image", &parameters);
        assert!(context.metadata().issues().is_empty());
        assert_eq!(context.received_time_ns(), Some(150));
        let snapshot = sink.take_snapshot();
        assert_eq!(
            snapshot
                .histograms
                .iter()
                .find(|row| row.metric == LatencyMetric::Hop)
                .unwrap()
                .sum_ns,
            50
        );
        assert_eq!(
            snapshot
                .histograms
                .iter()
                .find(|row| row.metric == LatencyMetric::EndToEnd)
                .unwrap()
                .sum_ns,
            130
        );
        assert!(sink.snapshot().histograms.is_empty());
        let mut output = Vec::new();
        write_snapshot(&mut output, &snapshot).unwrap();
        assert!(String::from_utf8(output).unwrap().contains("sum_ns=130"));
    }

    #[test]
    fn legacy_unknown_and_bad_typed_values_do_not_create_latency_samples() {
        let sink = Arc::new(BoundedMetricsSink::new(MetricsConfig::default()).unwrap());
        let observer = Observer::with_clock(FixedClock, Some(Arc::clone(&sink)));
        let legacy = observe(&observer, "image", &MetadataParameters::new());
        assert_eq!(legacy.metadata().issues(), &[MetadataIssue::MissingContext]);
        let unknown = observe(
            &observer,
            "image",
            &MetadataParameters::from([
                (VERSION_KEY.into(), Parameter::Integer(2)),
                (PUBLISH_TIME_KEY.into(), Parameter::String("future".into())),
            ]),
        );
        assert_eq!(
            unknown.metadata().issues(),
            &[MetadataIssue::UnknownVersion]
        );
        for invalid in [
            Parameter::Bool(true),
            Parameter::Float(1.0),
            Parameter::String("1".into()),
            Parameter::ListInt(vec![1]),
        ] {
            let context = observe(
                &observer,
                "image",
                &MetadataParameters::from([(VERSION_KEY.into(), invalid)]),
            );
            assert_eq!(
                context.metadata().issues(),
                &[MetadataIssue::InvalidVersion]
            );
        }
        let snapshot = sink.snapshot();
        assert!(snapshot.histograms.is_empty());
        assert_eq!(
            snapshot
                .counters
                .iter()
                .find(|row| row.reason == EventReason::Received)
                .unwrap()
                .count,
            6
        );
    }
}

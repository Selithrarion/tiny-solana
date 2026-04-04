// TODO: OpenTelemetry

// use prometheus::{
//     core::{AtomicU64, GenericGauge},
//     register_histogram_vec, register_int_gauge_vec, HistogramVec, IntGaugeVec,
// };
// use std::sync::OnceLock;

// pub static LOCK_WAIT_TIME: OnceLock<HistogramVec> = OnceLock::new();
// pub static LOCK_HELD_TIME: OnceLock<HistogramVec> = OnceLock::new();
// pub static TRANSACTION_COUNT: OnceLock<IntGaugeVec> = OnceLock::new();

// pub fn init_metrics() {
//     LOCK_WAIT_TIME
//         .set(
//             register_histogram_vec!(
//                 "lock_wait_seconds",
//                 "Time spent waiting for a lock",
//                 &["lock_name", "mode"],
//                 vec![0.0001, 0.001, 0.005, 0.01, 0.05, 0.1]
//             )
//             .unwrap(),
//         )
//         .expect("failed to set LOCK_WAIT_TIME");

//     LOCK_HELD_TIME
//         .set(
//             register_histogram_vec!(
//                 "lock_held_seconds",
//                 "Time a lock was held",
//                 &["lock_name", "mode"],
//                 vec![0.0001, 0.001, 0.005, 0.01, 0.05, 0.1]
//             )
//             .unwrap(),
//         )
//         .expect("failed to set LOCK_HELD_TIME");

//     TRANSACTION_COUNT
//         .set(
//             register_int_gauge_vec!(
//                 "transaction_total",
//                 "Total number of transactions processed",
//                 &["status"] // "success", "failure", "dropped"
//             )
//             .unwrap(),
//         )
//         .expect("failed to set TRANSACTION_COUNT");
// }

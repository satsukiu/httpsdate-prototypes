use async_trait::async_trait;
use libc;
use chrono::{DateTime, Utc};
use hyper::{Body, Client, Uri, client::HttpConnector};
use hyper_rustls::HttpsConnector;
use tokio::{self, time::{Duration, Instant}};
use futures::stream::StreamExt;

#[allow(dead_code)]

#[tokio::main]
async fn main() {
    // let bench_sampler = BenchSampler;
    // let low_res_sampler = LowResSampler;
    // let multi_sampler = MultiSampler::new(LowResSampler);
    let https_sampler = HttpsSampler::new();
    let multi_https_sampler = MultiSampler::new(HttpsSampler::new());

    // println!("Accuracy check, bench: {:?}", run_accuracy(bench_sampler).await);
    // println!("Accuracy check, low: {:?}", run_accuracy(low_res_sampler).await);
    // println!("Accuracy check, low multi: {:?}", run_accuracy(multi_sampler).await);

    println!("Accuracy check, http multiple: {:?}", accuracy_check(&multi_https_sampler).await);
}

const CHECK_TIMES: u32 = 10;

async fn run_accuracy<T: TimeSampler + Send + Sync>(sampler: T) -> Vec<f64> {
    futures::stream::iter(0..CHECK_TIMES)
        .then(|_| async {
            tokio::time::delay_for(Duration::from_secs(60)).await;
            accuracy_check(&sampler).await
        })
        .collect()
        .await
}

async fn accuracy_check<T: TimeSampler + Send + Sync>(sampler: &T) -> f64 {
    let bench_collector_fut = async {
        let sampler = BenchSampler;
        futures::stream::iter(0u8..5).then(|_| async {
            let s = sampler.sample().await;
            tokio::time::delay_for(Duration::from_millis(500)).await;
            s
        }).collect::<Vec<Sample>>().await
    };

    let sampler_fut = async move {
        tokio::time::delay_for(Duration::from_millis(500)).await;
        sampler.sample().await
    };

    let (bench_times, sample) = futures::future::join(bench_collector_fut, sampler_fut).await;
    // guess what our benchmark would've given as utc at the mono time of our sample.
    let bench_mono = bench_times.iter().map(|sample| sample.mono.0 as f64).collect::<Vec<_>>();
    let bench_utc = bench_times.iter().map(|sample| sample.utc.0 as f64).collect::<Vec<_>>();
    let (slope, intercept): (f64, f64) = linreg::linear_regression(bench_mono.as_slice(), bench_utc.as_slice()).unwrap();

    let assumed_utc_time = sample.mono.0 as f64 * slope + intercept;
    let diff_nanos = assumed_utc_time - sample.utc.0 as f64;
    (diff_nanos) / NANOS_IN_SEC as f64
}

#[derive(Debug, Clone)]
struct Timestamp(u128);
const NANOS_IN_SEC: u128 = 1_000_000_000;

impl Timestamp {
    fn monotonic() -> Self {
        unsafe {
            let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
            libc::clock_gettime(
                libc::CLOCK_MONOTONIC,
                &mut ts
            );
            let nanos = ts.tv_sec as u128 * NANOS_IN_SEC + ts.tv_nsec as u128;
            Timestamp(nanos)
        }
    }

    fn avg(&self, other: &Self) -> Self {
        Timestamp((self.0 + other.0) >> 1)
    }
}

#[derive(Debug)]
struct Sample {
    utc: Timestamp,
    mono: Timestamp,
}

#[async_trait]
trait TimeSampler {
    async fn sample(&self) -> Sample;
}

/// Samples unix system time. Assuming this is well synched via NTP.
struct BenchSampler;

#[async_trait]
impl TimeSampler for BenchSampler {
    async fn sample(&self) -> Sample {
        let before = Timestamp::monotonic();
        let utc = Timestamp(Utc::now().timestamp_nanos() as u128);
        let after = Timestamp::monotonic();
        Sample {
            utc,
            mono: before.avg(&after),
        }
    }
}

struct LowResSampler;

#[async_trait]
impl TimeSampler for LowResSampler {
    async fn sample(&self) -> Sample {
        let before = Timestamp::monotonic();
        let utc = Timestamp(Utc::now().timestamp() as u128 * NANOS_IN_SEC);
        let after = Timestamp::monotonic();
        Sample {
            utc,
            mono: before.avg(&after),
        }
    }
}

struct HttpsSampler {
    client: Client<HttpsConnector<HttpConnector>, Body>,
    uri: Uri
}

impl HttpsSampler {
    fn new() -> Self {
        let https = HttpsConnector::new();
        let client = Client::builder().build(https);
        let uri = "https://clients1.google.com/generate_204".parse().unwrap();
        Self { client, uri }
    }
}

#[async_trait]
impl TimeSampler for HttpsSampler {
    async fn sample(&self) -> Sample {
        let before = Timestamp::monotonic();

        let resp = self.client.get(self.uri.clone()).await.unwrap();
        let utc_date = resp.headers()
            .get(&hyper::header::DATE).unwrap()
            .to_str().unwrap();
        let utc_parsed = DateTime::parse_from_rfc2822(utc_date).unwrap();
        let utc_ts = Timestamp(utc_parsed.timestamp() as u128 * NANOS_IN_SEC);

        let after = Timestamp::monotonic();
        println!("mono time diff nanos: {:?}", after.0 - before.0);
        Sample {
            utc: utc_ts,
            mono: before.avg(&after),
        }
    }
}

/// A sampler that uses a low resolution sampler (in sec) and samples
/// it multiple times to obtain a higher quality sample.
struct MultiSampler<S: TimeSampler> {
    low_res_sampler: S,
}

impl <S:TimeSampler> MultiSampler<S> {
    fn new(s: S) -> Self {
        MultiSampler {low_res_sampler: s}
    }
}

#[async_trait]
impl <S:TimeSampler + Sync + Send> TimeSampler for MultiSampler<S> {
    async fn sample(&self) -> Sample {
        // this samples a source that can only provide second resolution a few times
        // within a second.
        // it then looks for a sample pair during which the second changed, and
        // takes the midpoint of those samples to create a new sample.

        let now = Instant::now();
        let timing_delays = (0..5)
            .map(|i| now.checked_add(Duration::from_millis(250 * i)).unwrap())
            .collect::<Vec<_>>();
        let samples = futures::stream::iter(timing_delays)
            .then(|instant| async move {
                tokio::time::delay_until(instant).await;
                self.low_res_sampler.sample().await
            })
            .collect::<Vec<_>>()
            .await;
        
        let mut second_change_idx = 10000;
        for idx in 0..5 {
            let s1 = samples.get(idx).unwrap();
            let s2 = samples.get(idx + 1).unwrap();
            if s1.utc.0 == s2.utc.0 - NANOS_IN_SEC {
                second_change_idx = idx;
                break;
            }
        }

        let final_sample_1 = samples.get(second_change_idx).unwrap();
        let final_sample_2 = samples.get(second_change_idx + 1).unwrap();
        Sample {
            utc: final_sample_2.utc.clone(), // this is the later second.
            mono: final_sample_1.mono.avg(&final_sample_2.mono)
        } // note - we can put hard bounds on the monotonic times at which the second
        // changed, should be between the before sample for sample 1 and the after sample
        // for sample 2.
    }
}
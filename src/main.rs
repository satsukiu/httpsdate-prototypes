//! Script that polls an HTTP server repeatedly to obtain time, combines polls
//! to improve time estimates, and attempts to calculate the error of each estimate.
//! The easiest way to build and run this is via cargo `cargo run -- [args]`

use argh::FromArgs;
use chrono::{DateTime};
use hyper::{Body, Client, Uri, client::HttpConnector};
use hyper_rustls::HttpsConnector;
use itertools::Itertools;
use tokio::{self, time::{Duration, Instant}};
use rand::random;
use std::io::Write;

const BASE_POLLS_DEFAULT: usize = 20;
const POLLS_DEFAULT: usize = 50;

const MIN_BETWEEN_POLLS: Duration = Duration::from_secs(5);

type Timestamp = u128;
const NANOS_IN_SEC: u128 = 1_000_000_000;

const MAX_DRIFT_PPM: u128 = 200;
const ONE_MILLION: u128 = 1_000_000;

const SAMPLE_CHUNK_SIZE: usize = 10;

#[derive(FromArgs)]
/// A program that polls an HTTP server repeatedly, and produces data on the
/// estimated error of the samples. The data is intended to be ingested by a
/// separate tool.
struct Args {
    /// file to output results to. Prints to stdout if not provided
    #[argh(option)]
    outfile: Option<String>,
    
    /// number of polls used to obtain the base sample other samples are compared against
    #[argh(option, default="BASE_POLLS_DEFAULT")]
    base_polls: usize,

    /// number of polls taken to produce data
    #[argh(option, default="POLLS_DEFAULT")]
    polls: usize,
}

#[tokio::main]
async fn main() {
    let Args {outfile, base_polls, polls } = argh::from_env::<Args>();

    let https_sampler = HttpsSampler::new();
    // initial polls to get a good initial value
    let (base_bounds, _) = https_sampler.tight_bound(base_polls).await;
    println!("Initial bound size: {:?}", base_bounds.size());

    // Poll samples without combining
    let mut inter_bounds = vec![];
    for _ in 0..polls {
        let bounds = https_sampler.new_bounds().await;
        inter_bounds.push(bounds);
        // Poll at random intervals to try and get a variety of results
        let sleep_millis: f32 = random::<f32>() * 1000 as f32;
        let sleep_time = MIN_BETWEEN_POLLS + Duration::from_millis(sleep_millis as u64);
        tokio::time::delay_for(sleep_time).await;
    }
    // take another tight sample at the end.
    let (final_bounds, _) = https_sampler.tight_bound(base_polls).await;
    println!("Final bound size: {:?}", final_bounds.size());

    // assume initial and final bounds are pretty good and estimate errors using them
    let mut out = vec![];
    writeln!(&mut out, "polls,size,delta_avg,delta_max,error").unwrap();
    let estimator = ErrorEstimator::new(base_bounds.to_pair(), final_bounds.to_pair());
    
    // produce combinations of the bounds previously sampled and evaluate their errors. If we try
    // to produce combinations accross the whole data set we'll end up with millions of
    // combinations, so chunk the values up first instead.
    for sample_chunk_iter in &inter_bounds.into_iter().chunks(SAMPLE_CHUNK_SIZE) {
        let sample_chunk = sample_chunk_iter.collect::<Vec<Bounds>>();
        for combination_size in 1..sample_chunk.len()+1 {
            for combination in sample_chunk.iter().combinations(combination_size) {
                let bound = combination.into_iter().fold(Option::<Bounds>::None, |maybe_b1, b2| {
                    match maybe_b1 {
                        Some(b1) => Some(b1.combine(b2)),
                        None => Some(b2.clone()),
                    }
                }).unwrap();
                let err = estimator.estimate_error(bound.to_pair());
                writeln!(&mut out, "{:?},{:?},{:?},{:?},{:?}",
                    combination_size, bound.size(), bound.avg_delta(), bound.max_delta(), err).unwrap();
            }
        }
    }

    match outfile {
        None => std::io::stdout().write_all(&out).unwrap(),
        Some(filename) => {
            let path = std::path::Path::new(&filename);
            let mut file = std::fs::File::create(&path).unwrap();
            file.write_all(&out).unwrap();
        }
    }
}

#[derive(Clone, Debug)]
struct Bounds {
    mono: Instant,
    utc_min: Timestamp,
    utc_max: Timestamp,
    /// deltas of polls used to calculate this bound.
    deltas: Vec<u64>,
}

impl Bounds {
    fn project(&self, later_mono: Instant) -> Bounds {
        let time_diff = later_mono.duration_since(self.mono).as_nanos();
        let max_err = time_diff * MAX_DRIFT_PPM / ONE_MILLION;
        Bounds {
            mono: later_mono,
            utc_min: self.utc_min + time_diff - max_err,
            utc_max: self.utc_max + time_diff + max_err,
            deltas: self.deltas.clone()
        }
    }

    fn combine(&self, other: &Bounds) -> Bounds {
        let (earlier, later) = if self.mono < other.mono {
            (self, other)
        } else {
            (other, self)
        };

        let projected = earlier.project(later.mono);
        let mut new_deltas = self.deltas.clone();
        new_deltas.extend_from_slice(other.deltas.as_slice());
        let new = Bounds {
            mono: later.mono,
            utc_min: std::cmp::max(projected.utc_min, later.utc_min),
            utc_max: std::cmp::min(projected.utc_max, later.utc_max),
            deltas: new_deltas
        };
        assert!(new.utc_min <= new.utc_max);
        new
    }

    fn to_pair(&self) -> Pair {
        Pair { mono: self.mono, utc: (self.utc_min + self.utc_max) / 2}
    }

    fn size(&self) -> u128 {
        self.utc_max - self.utc_min
    }

    fn avg_delta(&self) -> u64 {
        self.deltas.iter().fold(0, |a,x| a + x) / self.deltas.len() as u64
    }

    fn max_delta(&self) -> u64 {
        self.deltas.iter().fold(0, |a,x| std::cmp::max(a,*x))
    }
}

struct HttpsSampler {
    client: Client<HttpsConnector<HttpConnector>, Body>,
    uri: Uri,
}

impl HttpsSampler {
    fn new() -> Self {
        let https = HttpsConnector::new();
        let client = Client::builder().build(https);
        let uri = "https://clients1.google.com/generate_204".parse().unwrap();
        Self { client, uri }
    }

    /// Poll for a new bounds.
    async fn new_bounds(&self) -> Bounds {
        let before = tokio::time::Instant::now();
        let resp = self.client.get(self.uri.clone()).await.unwrap();
        let rtt = before.elapsed();

        let utc_date = resp.headers()
            .get(&hyper::header::DATE).unwrap()
            .to_str().unwrap();
        let utc_parsed = DateTime::parse_from_rfc2822(utc_date).unwrap();
        let utc_ts = utc_parsed.timestamp() as u128 * NANOS_IN_SEC;
        let delta = (rtt.as_nanos()) / 2;
        Bounds {
            mono: before + Duration::from_nanos(delta as u64),
            utc_min: utc_ts - delta,
            utc_max: utc_ts + NANOS_IN_SEC + delta,
            deltas: vec![delta as u64]
        }
    }

    /// Get a tight bound by polling multiple times.
    /// Returns (final bound, list of multiple bounds created)
    async fn tight_bound(&self, num_polls: usize) -> (Bounds, Vec<Bounds>) {
        let mut inter_bounds = vec![];
        let bound = self.new_bounds().await;
        let mut acc_bound = bound.clone();
        inter_bounds.push(bound);

        for _ in 1..num_polls {
            tokio::time::delay_until(Self::ideal_time(&acc_bound)).await;
            let bound = self.new_bounds().await;
            inter_bounds.push(bound.clone());
            let i = acc_bound.combine(&bound);
            acc_bound = i;
        }
        (acc_bound, inter_bounds)
    }

    fn ideal_time(bounds: &Bounds) -> Instant {
        let delta_est = bounds.avg_delta();

        let subs_off = subs((bounds.utc_min + bounds.utc_max) / 2);
        let ideal = bounds.mono + Duration::from_nanos(NANOS_IN_SEC as u64)
            + MIN_BETWEEN_POLLS
            - Duration::from_nanos(subs_off as u64) - Duration::from_nanos(delta_est);
        let now = Instant::now();
        match now.checked_duration_since(ideal) {
            None => ideal,
            Some(d) => ideal + Duration::from_secs(d.as_secs() + 1)
        }
    }
}

fn subs(timestamp: Timestamp) -> Timestamp {
    timestamp % NANOS_IN_SEC
}

struct Pair {
    mono: Instant,
    utc: Timestamp,
}

// Estimates sample errors based on two pairs assumed to be very accurate
struct ErrorEstimator {
    /// Instant considered monotonic time zero
    base_instant: Instant,
    slope_num: u128,
    slope_den: u128,
    intercept: u128,
}

impl ErrorEstimator {
    fn new(first: Pair, last: Pair) -> Self {
        Self {
            base_instant: first.mono,
            slope_num: last.utc - first.utc,
            slope_den: last.mono.duration_since(first.mono).as_nanos(),
            intercept: first.utc
        }
    }

    fn estimate_utc(&self, mono: Instant) -> Timestamp {
        let mono_nanos = mono.duration_since(self.base_instant).as_nanos();
        self.intercept + (mono_nanos * self.slope_num) / self.slope_den
    }

    fn estimate_error(&self, pair: Pair) -> i128 {
        pair.utc as i128 - self.estimate_utc(pair.mono) as i128
    }
}
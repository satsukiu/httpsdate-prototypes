use chrono::{DateTime};
use hyper::{Body, Client, Uri, client::HttpConnector};
use hyper_rustls::HttpsConnector;
use itertools::Itertools;
use tokio::{self, time::{Duration, Instant}};
use rand::random;

const INITIAL_POLLS: usize = 10;
const POLLS: usize = 10;

const MIN_BETWEEN_POLLS: Duration = Duration::from_secs(5);

type Timestamp = u128;
const NANOS_IN_SEC: u128 = 1_000_000_000;

const MAX_DRIFT_PPM: u128 = 200;
const ONE_MILLION: u128 = 1_000_000;

#[tokio::main]
async fn main() {
    let https_sampler = HttpsSampler::new();
    // initial polls to get a good initial value
    let (base_bounds, _, _) = https_sampler.tight_bound(INITIAL_POLLS).await;

    let mut inter_bounds = vec![];
    let mut deltas = vec![];
    for _ in 0..POLLS {
        let (bounds, delta) = https_sampler.new_bounds().await;
        inter_bounds.push(bounds);
        deltas.push(delta);

        let sleep_millis: f32 = random::<f32>() * 1000 as f32;
        let sleep_time = MIN_BETWEEN_POLLS + Duration::from_millis(sleep_millis as u64);
        tokio::time::delay_for(sleep_time).await;
    }
    // take another tight sample at the end. Don't reuse the existing bounds bc that
    // results in many error estimates of 0.
    let (final_bounds, _, _) = https_sampler.tight_bound(INITIAL_POLLS).await;

    let estimator = ErrorEstimator::new(base_bounds.to_pair(), final_bounds.to_pair());
    for combination_size in 1..inter_bounds.len() {
        for combination in inter_bounds.clone().into_iter().combinations(combination_size) {
            let bound = combination.into_iter().fold1(|b1, b2| b1.combine(b2)).unwrap();
            let err = estimator.estimate_error(bound.to_pair());
            // todo add delta average or something?
            println!("polls: {:?}, size: {:?}, error: {:?}", combination_size, bound.size(), err);
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Bounds {
    mono: Instant,
    utc_min: Timestamp,
    utc_max: Timestamp,
}

impl Bounds {
    fn project(&self, later_mono: Instant) -> Bounds {
        let time_diff = later_mono.duration_since(self.mono).as_nanos();
        let max_err = time_diff * MAX_DRIFT_PPM / ONE_MILLION;
        Bounds {
            mono: later_mono,
            utc_min: self.utc_min + time_diff - max_err,
            utc_max: self.utc_max + time_diff + max_err,
        }
    }

    fn combine(&self, other: Bounds) -> Bounds {
        let (earlier, later) = if self.mono < other.mono {
            (self, &other)
        } else {
            (&other, self)
        };

        let projected = earlier.project(later.mono);
        let new = Bounds {
            mono: later.mono,
            utc_min: std::cmp::max(projected.utc_min, later.utc_min),
            utc_max: std::cmp::min(projected.utc_max, later.utc_max)
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
}

struct HttpsSampler {
    client: Client<HttpsConnector<HttpConnector>, Body>,
    uri: Uri,
    base: tokio::time::Instant,
}

impl HttpsSampler {
    fn new() -> Self {
        let https = HttpsConnector::new();
        let client = Client::builder().build(https);
        let uri = "https://clients1.google.com/generate_204".parse().unwrap();
        Self { client, uri, base: tokio::time::Instant::now() }
    }

    /// Poll for a new bounds. Returns bounds and delta (rtt/2)
    async fn new_bounds(&self) -> (Bounds, u64) {
        let before = tokio::time::Instant::now();
        let resp = self.client.get(self.uri.clone()).await.unwrap();
        let rtt = before.elapsed();

        let utc_date = resp.headers()
            .get(&hyper::header::DATE).unwrap()
            .to_str().unwrap();
        let utc_parsed = DateTime::parse_from_rfc2822(utc_date).unwrap();
        let utc_ts = utc_parsed.timestamp() as u128 * NANOS_IN_SEC;
        let delta = (rtt.as_nanos()) / 2;
        let bounds = Bounds {
            mono: before + Duration::from_nanos(delta as u64),
            utc_min: utc_ts - delta,
            utc_max: utc_ts + NANOS_IN_SEC + delta
        };
        (bounds, delta as u64)
    }

    /// Get a tight bound by polling multiple times.
    /// Returns (final bound, list of multiple bounds created, list of deltas)
    async fn tight_bound(&self, num_polls: usize) -> (Bounds, Vec<Bounds>, Vec<u64>) {
        let mut deltas = vec![];
        let mut inter_bounds = vec![];
        let (bound, delta) = self.new_bounds().await;
        let mut acc_bound = bound;
        deltas.push(delta);
        inter_bounds.push(bound);

        for _ in 1..num_polls {
            tokio::time::delay_until(Self::ideal_time(&acc_bound, &deltas)).await;
            let (bound, delta) = self.new_bounds().await;
            deltas.push(delta);
            inter_bounds.push(bound);
            acc_bound = acc_bound.combine(bound);
        }
        (acc_bound, inter_bounds, deltas)
    }

    fn ideal_time(bounds: &Bounds, deltas: &Vec<u64>) -> Instant {
        let delta_est = deltas.iter().fold(0, |a,x| a + x) / deltas.len() as u64;

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

struct ErrorEstimator {
    /// Instant considered monotonic time zero
    base_instant: Instant,
    slope_num: u128,
    slope_den: u128,
    intercept: u128,
}

impl ErrorEstimator {
    fn new(first: Pair, last: Pair) -> Self {
        let last_mono_nanos = last.mono.duration_since(first.mono).as_nanos();
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
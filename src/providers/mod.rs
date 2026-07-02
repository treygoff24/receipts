use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::error::{Provider, ReconError};

pub mod cerebras;
pub mod exa;

pub const USER_AGENT: &str = concat!(
    "recon/",
    env!("CARGO_PKG_VERSION"),
    " (github.com/treygoff/recon)"
);
const MAX_ATTEMPTS: usize = 6;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Spend {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub dollars: f64,
    pub search_dollars: f64,
    pub call_count: u64,
}

impl Spend {
    pub fn total_dollars(&self) -> f64 {
        self.dollars + self.search_dollars
    }
}

pub type SharedSpend = Arc<Mutex<Spend>>;
pub type SleepFn = Arc<dyn Fn(Duration) + Send + Sync>;

pub fn new_spend() -> SharedSpend {
    Arc::new(Mutex::new(Spend::default()))
}

pub(crate) fn default_sleep() -> SleepFn {
    Arc::new(std::thread::sleep)
}

pub(crate) fn join_url(base_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

#[derive(Debug)]
pub(crate) enum HttpFailure {
    Status(u16),
    Transport(String),
}

impl From<ureq::Error> for HttpFailure {
    fn from(err: ureq::Error) -> Self {
        match err {
            ureq::Error::StatusCode(status) => HttpFailure::Status(status),
            other => HttpFailure::Transport(other.to_string()),
        }
    }
}

pub(crate) fn run_with_retries<T>(
    provider: Provider,
    mut send: impl FnMut() -> Result<T, HttpFailure>,
    sleep: &(dyn Fn(Duration) + Send + Sync),
) -> Result<(T, u32), ReconError> {
    let mut retries = 0;

    for attempt in 0..MAX_ATTEMPTS {
        match send() {
            Ok(value) => return Ok((value, retries)),
            Err(HttpFailure::Status(status)) if retry_delay(status, attempt).is_some() => {
                if attempt + 1 == MAX_ATTEMPTS {
                    return Err(status_error(provider, status));
                }
                retries += 1;
                sleep(retry_delay(status, attempt).expect("checked above"));
            }
            Err(HttpFailure::Status(status)) => return Err(status_error(provider, status)),
            Err(HttpFailure::Transport(message)) => {
                return Err(ReconError::network(message)
                    .with_provider(provider)
                    .with_retryable(true));
            }
        }
    }

    unreachable!("retry loop always returns on final attempt")
}

fn retry_delay(status: u16, attempt: usize) -> Option<Duration> {
    match status {
        429 => Some(Duration::from_secs(20 * (attempt as u64 + 1))),
        500..=599 => Some(Duration::from_secs(2_u64.pow(attempt as u32))),
        _ => None,
    }
}

fn status_error(provider: Provider, status: u16) -> ReconError {
    let message = format!("{provider} returned HTTP {status}");
    match status {
        401 | 403 => ReconError::auth(message)
            .with_provider(provider)
            .with_retryable(false),
        429 => ReconError::rate_limit(message)
            .with_provider(provider)
            .with_retryable(true),
        500..=599 => ReconError::upstream(message)
            .with_provider(provider)
            .with_retryable(true),
        _ => ReconError::upstream(message)
            .with_provider(provider)
            .with_retryable(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn joins_base_url_without_double_slashes() {
        assert_eq!(
            join_url("http://localhost:8000/", "/chat/completions"),
            "http://localhost:8000/chat/completions"
        );
    }

    #[test]
    fn retry_ladder_records_delays_without_sleeping() {
        let mut outcomes = VecDeque::from([
            Err(HttpFailure::Status(429)),
            Err(HttpFailure::Status(500)),
            Ok("done"),
        ]);
        let sleeps = Mutex::new(Vec::new());

        let (value, retries) = run_with_retries(
            Provider::Cerebras,
            || outcomes.pop_front().expect("test outcome"),
            &|duration| sleeps.lock().unwrap().push(duration),
        )
        .unwrap();

        assert_eq!(value, "done");
        assert_eq!(retries, 2);
        assert_eq!(
            *sleeps.lock().unwrap(),
            vec![Duration::from_secs(20), Duration::from_secs(2)]
        );
    }

    #[test]
    fn exhausted_429_maps_to_retryable_rate_limit() {
        let mut calls = 0;
        let sleeps = Mutex::new(Vec::new());

        let err = run_with_retries::<()>(
            Provider::Exa,
            || {
                calls += 1;
                Err(HttpFailure::Status(429))
            },
            &|duration| sleeps.lock().unwrap().push(duration),
        )
        .unwrap_err();

        assert_eq!(calls, 6);
        assert_eq!(err.code(), "rate_limited");
        assert_eq!(err.provider(), Some(Provider::Exa));
        assert!(err.is_retryable());
        assert_eq!(sleeps.lock().unwrap().len(), 5);
    }

    #[test]
    fn auth_status_does_not_retry() {
        let mut calls = 0;

        let err = run_with_retries::<()>(
            Provider::Cerebras,
            || {
                calls += 1;
                Err(HttpFailure::Status(403))
            },
            &|_| panic!("auth must not sleep"),
        )
        .unwrap_err();

        assert_eq!(calls, 1);
        assert_eq!(err.code(), "auth");
        assert!(!err.is_retryable());
    }

    #[test]
    fn spend_total_adds_model_and_search_buckets() {
        let spend = Spend {
            dollars: 0.09,
            search_dollars: 0.04,
            ..Spend::default()
        };

        assert_eq!(spend.total_dollars(), 0.13);
    }
}

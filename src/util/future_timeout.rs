/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use std::time::Duration;

use futures::Future;
use futures::FutureExt;
use futures::pin_mut;
use futures::select_biased;
use tokio::time::Instant;
use tokio::time::sleep_until;

/// Run a Future and take action if it takes longer than the given timeout.
pub async fn future_soft_timeout<Fut: Future>(
    fut: Fut,
    soft_timeout: Duration,
    after_soft_timeout: impl FnOnce(Duration),
    at_end_if_timedout: impl FnOnce(Duration),
) -> <Fut as Future>::Output {
    let start = Instant::now();
    let soft_timeout_at = start + soft_timeout;

    let fut = async move {
        let result = fut.await;
        let now = Instant::now();
        if soft_timeout_at < now {
            at_end_if_timedout(now.duration_since(start));
        }
        result
    }
    .fuse();
    pin_mut!(fut);
    let soft_timeout = async move {
        sleep_until(soft_timeout_at).await;
        after_soft_timeout(Instant::now().duration_since(start))
    }
    .fuse();
    pin_mut!(soft_timeout);

    loop {
        select_biased! {
            result = fut => { break result; }
            _ = soft_timeout => {}
        }
    }
}

#[cfg(test)]
mod test {
    //! NOTE: I made an attempt of writing the following tests using
    //! tokio::time::{pause, advance, resume}, but at least with tokio 0.2.13
    //! those functions don't seem to work for delay futures. It is worth
    //! revisiting this approach after we update to tokio >=1.x.
    use futures::future::select;
    use tokio::time::sleep;

    use super::*;

    #[derive(Debug, Eq, PartialEq)]
    struct TestResult {
        ready: bool,
        after: bool,
        end: bool,
    }

    #[tokio::test]
    async fn future_soft_timeout_test() {
        let secs = Duration::from_secs;
        let millis = Duration::from_millis;
        let test_cases = vec![
            (secs(10), secs(5), millis(0), false, false, false),
            (secs(10), secs(0), millis(100), false, true, false),
            (millis(100), secs(0), millis(200), true, true, true),
        ];

        for (fut_dur, timeout, wait, exp_ready, exp_after, exp_end) in test_cases {
            let mut ready = false;
            let mut after = false;
            let mut end = false;

            {
                let test = future_soft_timeout(
                    async {
                        sleep(fut_dur).await;
                        ready = true;
                    },
                    timeout,
                    |_| {
                        after = true;
                    },
                    |_| {
                        end = true;
                    },
                );
                pin_mut!(test);

                let delay = sleep(wait);
                pin_mut!(delay);

                select(test, delay).await;
            }

            assert_eq!(
                TestResult { ready, after, end },
                TestResult {
                    ready: exp_ready,
                    after: exp_after,
                    end: exp_end
                },
                "While running test with fut_dur {:?} timeout {:?}",
                fut_dur,
                timeout
            );
        }
    }
}

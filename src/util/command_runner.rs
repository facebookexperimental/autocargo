/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use std::process::Output;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use cfg_if::cfg_if;
use futures::Future;
use slog::Logger;
use slog::warn;
use tokio::process::Command;

use super::future_timeout::future_soft_timeout;

/// Run provided command reporting if it is running for longer than soft_timeout
/// and if the output of that command is unsuccessful.
pub async fn run_command(
    logger: &Logger,
    command_dbg_name: &str,
    soft_timeout: Duration,
    cmd_fut: impl Future<Output = Result<(Command, Output)>> + '_,
) -> Result<Output> {
    let (command, output) = future_soft_timeout(
        cmd_fut,
        soft_timeout,
        |duration| {
            warn!(
                logger,
                "'{}' running for more than {:.1?}", command_dbg_name, duration
            )
        },
        |duration| {
            warn!(
                logger,
                "'{}' finished after {:.1?}", command_dbg_name, duration
            )
        },
    )
    .await
    .with_context(|| format!("While running '{command_dbg_name}'"))?;

    if !output.status.success() {
        warn!(
            logger,
            concat!(
                "'{}' failed with Exit Status: {:?}\n",
                "with Command: {:?}\n",
                "with Stdout:\n{}\n",
                "with Stderr:\n{}",
            ),
            command_dbg_name,
            output.status,
            command.as_std(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    Ok(output)
}

cfg_if! {
    if #[cfg(test)] {
        pub(crate) use self::r#impl::MockCommandRunner as MockableCommandRunner;
    } else {
        pub(crate) use self::r#impl::CommandRunner as MockableCommandRunner;
    }
}

mod r#impl {
    use futures::future::LocalBoxFuture;
    use mockall::automock;

    use super::*;

    /// This structure might be used in place of run_command if mocking of running
    /// command in tests is required.
    #[derive(Default)]
    pub struct CommandRunner {}

    #[automock]
    impl CommandRunner {
        /// Call run_command, can be mocked in tests.
        #[allow(dead_code)]
        pub async fn run<'a>(
            &self,
            logger: &Logger,
            command_dbg_name: &str,
            soft_timeout: Duration,
            command: LocalBoxFuture<'a, Result<(Command, Output)>>,
        ) -> Result<Output> {
            run_command(logger, command_dbg_name, soft_timeout, command).await
        }
    }
}

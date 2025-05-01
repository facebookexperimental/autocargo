/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use std::io::Write;

use chrono::Local;
use slog::Drain;
use slog::Logger;
use slog::o;
use slog_term::FullFormat;
use slog_term::TermDecorator;

pub fn logger() -> Logger {
    let decorator = TermDecorator::new().build();
    let drain = FullFormat::new(decorator)
        .use_custom_timestamp(move |rd: &mut dyn Write| {
            write!(rd, "{}", Local::now().format("%T %Z"))
        })
        .build()
        .fuse();
    let drain = slog_async::Async::new(drain).build().fuse();

    slog::Logger::root(drain, o!())
}

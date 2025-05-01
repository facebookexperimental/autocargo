// (c) Meta Platforms, Inc. and affiliates. Confidential and proprietary.

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

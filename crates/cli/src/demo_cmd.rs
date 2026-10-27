// SPDX-License-Identifier: MIT
//! `harness demo [--port N] [--no-browser] [--speed X] [--data-dir D] [--loop] [--keep]`: the panel on a fake project with mocked, moving loop data
//! (never calls claude, never touches your data folder).

use app::demo::{run_demo, DemoOptions};
use clap::Args as ClapArgs;

#[derive(ClapArgs)]
pub struct Args {
    /// fixed port (default: first free one from 47800 to 47899)
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// does not open the browser
    #[arg(long)]
    no_browser: bool,
    /// animation speed multiplier (2 = twice as fast)
    #[arg(long, default_value_t = 1.0)]
    speed: f64,
    /// app data folder; the demo project goes in <data-dir>/demo/ (default: a temporary folder)
    #[arg(long, default_value = "")]
    data_dir: String,
    /// restart the story after it ends, forever (for screen recorders)
    #[arg(long = "loop")]
    loop_forever: bool,
    /// keep the demo files when it stops
    #[arg(long)]
    keep: bool,
}

pub fn run(args: Args) -> i32 {
    run_demo(&DemoOptions { port: args.port, no_browser: args.no_browser, speed: args.speed, data_dir: args.data_dir, loop_forever: args.loop_forever, keep: args.keep })
}

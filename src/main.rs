//src/main.rs

use anyhow::Result;
use std::time::Instant;

mod itchy_parser;
mod arbiter;

use itchy_parser::parse_file;
use arbiter::MatchingEngine;

const MAX_MSG: usize = 1_000_000;

fn main() -> Result<()> {
    // Hardcoded path; no CLI arguments.
    let path = "12302019.NASDAQ_ITCH50";

    // 1. Parse & count up to MAX_MSG
    let parse_start = Instant::now();
    let mut parse_count = 0usize;
    {
        let iter = parse_file(path)?;
        for _ev in iter.take(MAX_MSG) {
            parse_count += 1;
        }
    }
    let parse_dur = parse_start.elapsed();

    // 2. Matching stage on the same number of messages
    let mut engine = MatchingEngine::new();
    let match_start = Instant::now();
    {
        let iter2 = parse_file(path)?;
        for ev in iter2.take(parse_count) {
            engine.handle(ev);
        }
    }
    let match_dur = match_start.elapsed();

    // 3. Print performance summary of our engine 
    println!("Processed {} messages (max {})", parse_count, MAX_MSG);
    println!("\nITCH Parsing");
    println!("  Parse Time:  {:.3} secs", parse_dur.as_secs_f64());
    println!("  Parse Speed: {:.0} msg/sec", parse_count as f64 / parse_dur.as_secs_f64());

    println!("\nLOB Matching");
    println!("  Match Time:  {:.3} secs", match_dur.as_secs_f64());
    println!("  Match Speed: {:.0} msg/sec", parse_count as f64 / match_dur.as_secs_f64());

    println!();
    engine.print_stats();

    Ok(())
}

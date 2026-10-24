//! Report printing. Everything the binary writes goes through these four labels so a
//! reader can never mistake an observation for an inference.

use std::fmt::Display;

pub fn banner(title: &str) {
    println!();
    println!("{}", "=".repeat(78));
    println!("## {title}");
    println!("{}", "=".repeat(78));
}

pub fn sub(title: &str) {
    println!();
    println!(
        "--- {title} {}",
        "-".repeat(78usize.saturating_sub(title.len() + 5))
    );
}

/// A fact this run produced, with its numbers.
pub fn observed(msg: impl Display) {
    for (i, line) in msg.to_string().lines().enumerate() {
        if i == 0 {
            println!("OBSERVED: {line}");
        } else {
            println!("          {line}");
        }
    }
}

/// A behaviour that would bite a `TerminalBackend` implementation.
pub fn hazard(msg: impl Display) {
    for (i, line) in msg.to_string().lines().enumerate() {
        if i == 0 {
            println!("HAZARD:   {line}");
        } else {
            println!("          {line}");
        }
    }
}

pub fn verdict(msg: impl Display) {
    for (i, line) in msg.to_string().lines().enumerate() {
        if i == 0 {
            println!("VERDICT:  {line}");
        } else {
            println!("          {line}");
        }
    }
}

/// Something read out of a source tree or documentation, not produced by this run.
pub fn from_source(msg: impl Display) {
    for (i, line) in msg.to_string().lines().enumerate() {
        if i == 0 {
            println!("READ-FROM-SOURCE (not measured here): {line}");
        } else {
            println!("          {line}");
        }
    }
}

pub fn kv(key: &str, value: impl Display) {
    println!("          {key:<34} {value}");
}

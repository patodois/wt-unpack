//! The viewer's command line, run as the real program: `--help` and a mistake must answer without opening a window.

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Runs wt-viewer with the arguments and waits for it to exit by itself; a program that opens its window
/// instead is killed after a while and fails the test
fn run(args: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_wt-viewer"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("starts");
    let start = Instant::now();
    while child.try_wait().expect("polls").is_none() {
        if start.elapsed() > Duration::from_secs(20) {
            child.kill().ok();
            panic!("`wt-viewer {}` did not exit: it opened its window instead", args.join(" "));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    child.wait_with_output().expect("collects")
}

#[test]
fn help_prints_the_options_and_exits() {
    let out = run(&["--help"]);
    assert!(out.status.success(), "{:?}", out.status);
    let text = String::from_utf8_lossy(&out.stdout);
    for flag in ["--game", "--unit", "--turret", "--gun", "--orbit", "--target", "--lift", "--drive", "--auto", "--screenshot", "--help"] {
        assert!(text.contains(flag), "{flag} is not in the help:\n{text}");
    }
    assert!(text.starts_with("wt-viewer:") && text.contains("Usage: wt-viewer"));
}

#[test]
fn an_unknown_option_is_an_error_with_the_usage() {
    let out = run(&["--turrett", "60"]);
    assert_eq!(out.status.code(), Some(2));
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("error: unknown option --turrett") && text.contains("Usage: wt-viewer"), "{text}");
}

#[test]
fn a_model_without_its_pack_is_an_error() {
    let out = run(&["only_one.grp"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("expected a pack and a model name"));
}

#[test]
fn the_readme_lists_every_option_the_help_does() {
    let readme = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../README.md")).expect("README.md");
    let help = String::from_utf8_lossy(&run(&["--help"]).stdout).into_owned();
    for flag in help.split_whitespace().filter(|w| w.starts_with("--")).map(|w| w.trim_end_matches([',', ':', '.', ')', ';'])) {
        assert!(readme.contains(flag), "{flag} is in `wt-viewer --help` but not in the README");
    }
}

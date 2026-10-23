//! The command line: argument handling that needs no game files, and `check-bridge` on a few vehicles of a
//! real install (when `WT_GAME_DIR` is set).

#![cfg(feature = "bridge")]

use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wt-unpack")).args(args).output().unwrap()
}

#[test]
fn conformance_options_need_no_double_dash() {
    // Both spellings reach the checker, which finds nothing to check in an empty game folder
    let dir = std::env::temp_dir();
    let dir = dir.to_str().unwrap();
    for args in [
        vec!["check-conformance", "--game", dir, "--filter", "zz_nothing", "--no-textures"],
        vec!["check-conformance", "--game", dir, "--", "--filter", "zz_nothing"],
    ] {
        let out = run(&args);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{args:?}: {stdout}{}", String::from_utf8_lossy(&out.stderr));
        assert!(stdout.contains("no asset matches"), "{args:?}: {stdout}");
    }
    // and the checker's own help is reachable
    let help = run(&["check-conformance", "--help"]);
    assert!(String::from_utf8_lossy(&help.stdout).contains("--compare-load"));
}

#[test]
fn check_bridge_says_how_many_it_checks_and_reports_progress() {
    let Some(game) = std::env::var_os("WT_GAME_DIR") else { return };
    let game = game.to_str().unwrap();
    let out = run(&["check-bridge", "--game", game, "--filter", "tanks/usa_", "--limit", "3", "--progress", "1"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}{}", String::from_utf8_lossy(&out.stderr));
    // The count comes first, then a line per vehicle but the last with the elapsed time and memory
    assert!(stdout.starts_with("checking 3 "), "{stdout}");
    assert!(stdout.contains("1/3  elapsed ") && stdout.contains("2/3  elapsed "), "{stdout}");
    assert!(stdout.contains("MB resident"), "{stdout}");
    assert!(stdout.contains("3/3 vehicles loaded and valid"), "{stdout}");
}

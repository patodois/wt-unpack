//! The viewer's command line: which options exist, what `--help` prints, and the mistakes that are
//! refused (an unknown option, an option without its value, a model without its pack) before any window
//! opens.

/// Options that take the next argument as their value
pub const VALUE_FLAGS: [&str; 9] = ["--game", "--unit", "--screenshot", "--turret", "--gun", "--orbit", "--target", "--auto", "--lift"];
/// Options that stand alone
pub const SWITCHES: [&str; 1] = ["--drive"];

/// What the command line asks for
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// `--help` or `-h`: print the usage and exit
    Help,
    /// Open the window
    Run,
}

/// The text `--help` prints; `default_game` is where `--game` points when it is not given
pub fn usage(default_game: &str) -> String {
    format!(
        "wt-viewer: open a War Thunder vehicle from the game files in a 3D window

Usage: wt-viewer [PACK.grp MODEL] [OPTIONS]

With no PACK.grp and MODEL it opens the M4A1 (76) W Sherman. PACK.grp is a .grp file of the game's
content/base/res/tanks, aircrafts or ships folder and MODEL the name of a model in it
(`wt-unpack grp PACK.grp` lists them).

Options:
  --game <folder>           the game folder (the one with aces.vromfs.bin)
                            [default: {default_game}]
  --unit <name>             the unit BLK that gives the turret limits and the drive train, when its name
                            is not found from the model's
  --turret <degrees>        starting traverse of the turret, positive to the left (within its limits)
  --gun <degrees>           starting elevation of the gun, positive up (within its limits)
  --orbit <yaw,pitch,dist>  starting camera: angles in degrees and distance in metres
  --target <x,y,z>          the point the camera looks at, in the model's space, metres
  --lift <node=metres>      raise a skeleton node, to see the skinned tracks follow it; several with commas
  --drive                   put the tank on flat ground to drive with WASD
  --auto <script>           drive by script instead of keys, e.g. W:20,WA:3 (see below); needs --drive
  --screenshot <file.png>   save one frame (after the --auto script, if any) and close
  -h, --help                print this text and exit

Controls: drag to orbit, scroll to zoom, Q/E turn the turret, R/F elevate the gun. With --drive: W/S
throttle, A/D steer.

A script is a list of key groups and seconds: W:20,WA:3,S:2 holds W for 20 s, then W and A for 3 s,
then S for 2 s. `-` is no input.
"
    )
}

/// Checks the arguments (without the program name). `Err` holds what is wrong, for a message above the usage.
pub fn check(args: &[String]) -> Result<Action, String> {
    let mut positional = 0;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--help" || a == "-h" {
            return Ok(Action::Help);
        } else if SWITCHES.contains(&a) {
            i += 1;
        } else if VALUE_FLAGS.contains(&a) {
            if args.get(i + 1).is_none() {
                return Err(format!("{a} needs a value"));
            }
            i += 2;
        } else if a.starts_with('-') && a.parse::<f32>().is_err() {
            let hint = a.split_once('=').filter(|(flag, _)| VALUE_FLAGS.contains(flag)).map(|(flag, _)| format!(" (write `{flag} <value>`, with a space)"));
            return Err(format!("unknown option {a}{}", hint.unwrap_or_default()));
        } else {
            positional += 1;
            i += 1;
        }
    }
    match positional {
        0 | 2 => Ok(Action::Run),
        n => Err(format!("expected a pack and a model name (or neither), got {n} argument{}", if n == 1 { "" } else { "s" })),
    }
}

/// The arguments that are neither options nor an option's value: the pack and the model
pub fn positional(args: &[String]) -> Vec<&String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if SWITCHES.contains(&args[i].as_str()) {
            i += 1;
        } else if VALUE_FLAGS.contains(&args[i].as_str()) {
            i += 2;
        } else {
            out.push(&args[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_line(line: &str) -> Result<Action, String> {
        check(&line.split_whitespace().map(str::to_string).collect::<Vec<_>>())
    }

    #[test]
    fn help_is_asked_for_by_either_spelling_wherever_it_stands() {
        for line in ["--help", "-h", "pack.grp model --help", "--turret 60 --help"] {
            assert_eq!(check_line(line), Ok(Action::Help), "{line}");
        }
    }

    #[test]
    fn every_documented_option_is_listed_in_the_usage() {
        let text = usage("/game");
        for flag in VALUE_FLAGS.iter().chain(&SWITCHES).chain(&["--help"]) {
            assert!(text.contains(flag), "{flag}");
        }
        assert!(text.contains("/game"));
    }

    #[test]
    fn the_commands_of_the_readme_are_accepted() {
        for line in [
            "",
            "g.grp panther --game /game",
            "s.grp sherman --drive",
            "--turret 60 --gun 10 --screenshot sherman.png",
            "--drive --auto W:30,WA:3 --turret 90 --screenshot drive.png",
            "--turret -30 --lift bone_wheel_l_03=0.15 --orbit 30,20,9 --target 0,1,0 --unit us_m4a1_76w_sherman",
        ] {
            assert_eq!(check_line(line), Ok(Action::Run), "{line}");
        }
    }

    #[test]
    fn mistakes_are_named() {
        let err = |line: &str| check_line(line).unwrap_err();
        assert!(err("--turrett 60").contains("unknown option --turrett"));
        assert!(err("-x").contains("unknown option -x"));
        assert!(err("--game=/g").contains("with a space"), "{}", err("--game=/g"));
        assert!(err("a.grp m --gun").contains("--gun needs a value"));
        assert!(err("only.grp").contains("got 1 argument"));
        assert!(err("a.grp b c").contains("got 3 arguments"));
    }

    #[test]
    fn positional_arguments_skip_options_and_their_values() {
        let args: Vec<String> = "--game /g pack.grp --drive model --turret 5".split_whitespace().map(str::to_string).collect();
        assert_eq!(positional(&args), ["pack.grp", "model"]);
    }
}

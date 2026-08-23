use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::time::Instant;

const USAGE: &str = "\
Usage:
  cargo run --manifest-path tools/xtask/Cargo.toml -- verify [OPTIONS]

Options:
  --offline                 Pass --offline to Cargo commands which may resolve packages
  --stress-repetitions N    Run each named pressure regression N times (default: 1)
  --dry-run                 Print the exact commands without executing them
  -h, --help                Print this help
";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Options {
    offline: bool,
    stress_repetitions: usize,
    dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Step {
    label: String,
    args: Vec<&'static str>,
    accepts_offline: bool,
}

impl Step {
    fn cargo(label: impl Into<String>, args: &[&'static str]) -> Self {
        Self {
            label: label.into(),
            args: args.to_vec(),
            accepts_offline: true,
        }
    }

    fn cargo_without_offline(label: impl Into<String>, args: &[&'static str]) -> Self {
        Self {
            label: label.into(),
            args: args.to_vec(),
            accepts_offline: false,
        }
    }

    fn command_args(&self, offline: bool) -> Vec<OsString> {
        let mut args: Vec<OsString> = self.args.iter().map(OsString::from).collect();
        if offline && self.accepts_offline {
            args.insert(1, OsString::from("--offline"));
        }
        args
    }
}

fn main() {
    if let Err(message) = run(env::args_os().skip(1).collect()) {
        eprintln!("error: {message}");
        process::exit(1);
    }
}

fn run(args: Vec<OsString>) -> Result<(), String> {
    let Some(first) = args.first() else {
        return Err(format!("missing command\n\n{USAGE}"));
    };
    if first == "-h" || first == "--help" {
        print!("{USAGE}");
        return Ok(());
    }
    if first != "verify" {
        return Err(format!("unknown command {}\n\n{USAGE}", display_arg(first)));
    }

    let options = parse_verify_options(&args[1..])?;
    let root = repository_root()?;
    let steps = verification_steps(options.stress_repetitions);
    let verification_started = Instant::now();

    println!("NBReq verification root: {}", root.display());
    println!("Verification steps: {}", steps.len());
    if options.dry_run {
        println!("Dry run: commands will not be executed.");
    }

    for (index, step) in steps.iter().enumerate() {
        let command_args = step.command_args(options.offline);
        println!();
        println!("[{}/{}] {}", index + 1, steps.len(), step.label);
        println!("> cargo {}", display_args(&command_args));
        if options.dry_run {
            continue;
        }
        io::stdout()
            .flush()
            .map_err(|error| format!("could not flush verification output: {error}"))?;

        let step_started = Instant::now();
        let status = Command::new("cargo")
            .args(&command_args)
            .current_dir(&root)
            .status()
            .map_err(|error| format!("could not start Cargo for '{}': {error}", step.label))?;
        if !status.success() {
            return Err(match status.code() {
                Some(code) => format!("'{}' failed with exit code {code}", step.label),
                None => format!("'{}' was terminated", step.label),
            });
        }
        println!(
            "PASS {} ({:.3}s)",
            step.label,
            step_started.elapsed().as_secs_f64()
        );
    }

    println!();
    if options.dry_run {
        println!(
            "NBReq verification dry run complete: {} steps planned.",
            steps.len()
        );
    } else {
        println!(
            "NBReq verification complete: all {} steps passed in {:.3}s.",
            steps.len(),
            verification_started.elapsed().as_secs_f64()
        );
    }
    Ok(())
}

fn parse_verify_options(args: &[OsString]) -> Result<Options, String> {
    let mut options = Options {
        offline: false,
        stress_repetitions: 1,
        dry_run: false,
    };
    let mut index = 0;
    while index < args.len() {
        match args[index].to_str() {
            Some("--offline") => options.offline = true,
            Some("--dry-run") => options.dry_run = true,
            Some("-h" | "--help") => {
                print!("{USAGE}");
                process::exit(0);
            }
            Some("--stress-repetitions") => {
                index += 1;
                let Some(value) = args.get(index) else {
                    return Err("--stress-repetitions requires a positive integer".into());
                };
                options.stress_repetitions = value
                    .to_str()
                    .and_then(|value| value.parse().ok())
                    .filter(|value| *value > 0)
                    .ok_or_else(|| "--stress-repetitions requires a positive integer".to_owned())?;
            }
            Some(option) => return Err(format!("unknown verify option {option}\n\n{USAGE}")),
            None => {
                return Err(format!(
                    "verify option is not valid Unicode: {}",
                    display_arg(&args[index])
                ));
            }
        }
        index += 1;
    }
    Ok(options)
}

fn verification_steps(stress_repetitions: usize) -> Vec<Step> {
    let mut steps = vec![
        Step::cargo_without_offline(
            "verification-runner formatting",
            &[
                "fmt",
                "--manifest-path",
                "tools/xtask/Cargo.toml",
                "--check",
            ],
        ),
        Step::cargo(
            "verification-runner tests",
            &["test", "--manifest-path", "tools/xtask/Cargo.toml"],
        ),
        Step::cargo(
            "verification-runner warning-denied lint",
            &[
                "clippy",
                "--manifest-path",
                "tools/xtask/Cargo.toml",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
        ),
        Step::cargo_without_offline("NBReq formatting", &["fmt", "--check"]),
        Step::cargo_without_offline(
            "WinSock compatibility wrapper formatting",
            &[
                "fmt",
                "--manifest-path",
                "support/winpoll/Cargo.toml",
                "--check",
            ],
        ),
        Step::cargo(
            "WinSock compatibility wrapper compilation",
            &[
                "check",
                "--manifest-path",
                "support/winpoll/Cargo.toml",
                "--all-targets",
            ],
        ),
        Step::cargo(
            "WinSock compatibility wrapper warning-denied lint",
            &[
                "clippy",
                "--manifest-path",
                "support/winpoll/Cargo.toml",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
        ),
        Step::cargo(
            "WinSock compatibility wrapper tests",
            &["test", "--manifest-path", "support/winpoll/Cargo.toml"],
        ),
        Step::cargo(
            "minimal-feature compilation",
            &["check", "--no-default-features"],
        ),
        Step::cargo(
            "all-feature/all-target compilation",
            &["check", "--all-features", "--all-targets"],
        ),
        Step::cargo(
            "warning-denied lint",
            &[
                "clippy",
                "--all-features",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
        ),
        Step::cargo("ordinary default tests", &["test"]),
        Step::cargo("minimal-feature tests", &["test", "--no-default-features"]),
        Step::cargo(
            "native tests",
            &["test", "--features", "native,test-support"],
        ),
        Step::cargo(
            "curl reference tests",
            &["test", "--features", "curl-pilot"],
        ),
        Step::cargo("all-feature tests", &["test", "--all-features"]),
        Step::cargo("all-feature doctests", &["test", "--all-features", "--doc"]),
        Step::cargo(
            "all-feature documentation",
            &["doc", "--all-features", "--no-deps"],
        ),
    ];

    let stress_tests = [
        "capped_connection_pressure_survives_mixed_peer_interruptions",
        "aggregate_stream_pressure_releases_windows_after_cancel_and_drain",
        "resolver_pressure_cancels_live_queries_without_starving_healthy_peers",
    ];
    for repetition in 1..=stress_repetitions {
        for test in stress_tests {
            steps.push(Step::cargo(
                format!("pressure regression {test} ({repetition}/{stress_repetitions})"),
                &["test", "--features", "native,test-support", test],
            ));
        }
    }
    steps
}

fn repository_root() -> Result<PathBuf, String> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| "xtask must live at tools/xtask beneath the repository root".to_owned())?;
    if !root.join("Cargo.toml").is_file() {
        return Err(format!(
            "repository Cargo.toml not found at {}",
            root.display()
        ));
    }
    Ok(root.to_path_buf())
}

fn display_args(args: &[OsString]) -> String {
    let mut rendered = String::new();
    for (index, arg) in args.iter().enumerate() {
        if index != 0 {
            rendered.push(' ');
        }
        let _ = write!(rendered, "{}", display_arg(arg));
    }
    rendered
}

fn display_arg(arg: &OsStr) -> String {
    let value = arg.to_string_lossy();
    if value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "-_,./:=+".contains(character))
    {
        value.into_owned()
    } else {
        format!("{value:?}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_plan_covers_the_frozen_gate() {
        let steps = verification_steps(2);
        assert_eq!(steps.len(), 24);
        assert!(steps.iter().any(|step| step.args == ["test"]));
        assert!(
            steps
                .iter()
                .any(|step| { step.args == ["test", "--features", "native,test-support"] })
        );
        assert!(
            steps
                .iter()
                .any(|step| step.args == ["test", "--features", "curl-pilot"])
        );
        assert!(
            steps
                .iter()
                .any(|step| step.args == ["test", "--all-features"])
        );
        assert_eq!(
            steps
                .iter()
                .filter(|step| step.label.starts_with("pressure regression"))
                .count(),
            6
        );
    }

    #[test]
    fn offline_is_added_only_to_package_resolving_commands() {
        let steps = verification_steps(1);
        assert_eq!(
            steps[0].command_args(true),
            [
                "fmt",
                "--manifest-path",
                "tools/xtask/Cargo.toml",
                "--check"
            ]
        );
        assert_eq!(
            steps[1].command_args(true),
            [
                "test",
                "--offline",
                "--manifest-path",
                "tools/xtask/Cargo.toml"
            ]
        );
        assert_eq!(
            steps[8].command_args(true),
            ["check", "--offline", "--no-default-features"]
        );
    }

    #[test]
    fn verify_options_fail_closed() {
        assert!(parse_verify_options(&["--stress-repetitions".into(), "0".into()]).is_err());
        assert!(parse_verify_options(&["--unknown".into()]).is_err());
        assert_eq!(
            parse_verify_options(&[
                "--offline".into(),
                "--stress-repetitions".into(),
                "25".into(),
                "--dry-run".into(),
            ]),
            Ok(Options {
                offline: true,
                stress_repetitions: 25,
                dry_run: true,
            })
        );
    }
}

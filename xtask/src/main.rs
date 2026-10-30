//! Workspace automation entry point.
//!
//! Run with `cargo xtask <task>`.

fn main() {
    let task = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "help".to_string());

    match task.as_str() {
        "help" | "--help" | "-h" => {
            println!("cargo xtask <task>");
            println!();
            println!("Tasks:");
            println!("  help    Show this message");
            println!("  check   Run fmt + clippy + tests");
            println!("  deny    Run cargo-deny (licenses and advisories)");
        }
        "check" => {
            run("cargo", &["fmt", "--all", "--check"]);
            run(
                "cargo",
                &[
                    "clippy",
                    "--workspace",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ],
            );
            run("cargo", &["test", "--workspace"]);
        }
        "deny" => run("cargo", &["deny", "check"]),
        other => {
            eprintln!("unknown task: {other}");
            std::process::exit(2);
        }
    }
}

fn run(program: &str, args: &[&str]) {
    let status = std::process::Command::new(program)
        .args(args)
        .status()
        .unwrap_or_else(|err| {
            eprintln!("failed to run {program}: {err}");
            std::process::exit(2);
        });
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
}

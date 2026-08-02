//! davimci binary entrypoint.
//!
//! There is no frontend yet (plan.md Phase 9), so the binary opens a
//! workspace, answers the crash-recovery prompt, and runs any `:` commands
//! given on the command line. That makes the Phase 8 lifecycle usable and
//! scriptable before a window exists.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use davimci_cli::{ExOutcome, OnRecovery, Workspace};
use davimci_core::Classify;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut open: Option<PathBuf> = None;
    let mut commands: Vec<String> = Vec::new();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            "--version" => {
                println!("davimci {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "-c" => commands.push(args.next().context("-c needs a command")?),
            other if other.starts_with('-') => {
                anyhow::bail!("unknown option {other}; try --help")
            }
            other => open = Some(PathBuf::from(other)),
        }
    }

    let root = std::env::current_dir().context("the working directory is unreadable")?;
    let mut ws = Workspace::new(root);

    if let Some(path) = open {
        let recovery = ws.pending_recovery(&path);
        let choice = match &recovery {
            Some(r) => {
                println!(
                    "{} has an autosave with {} unsaved edit(s) from a previous session.",
                    path.display(),
                    r.commands
                );
                if prompt_yes("recover them?") {
                    OnRecovery::Recover
                } else {
                    OnRecovery::Discard
                }
            }
            None => OnRecovery::Discard,
        };
        report(ws.run(&format!("e {}", path.display()), choice));
    }

    for line in &commands {
        report(ws.run(line, OnRecovery::Discard));
        if ws.should_quit() {
            return Ok(());
        }
    }

    for line in ws.list() {
        println!("{line}");
    }
    Ok(())
}

fn report(result: Result<ExOutcome, davimci_cli::CliError>) {
    match result {
        Ok(ExOutcome::Message(m)) => println!("{m}"),
        Ok(ExOutcome::Lines(lines)) => {
            for l in lines {
                println!("{l}");
            }
        }
        Ok(ExOutcome::Quit) => println!("closed the last timeline"),
        // Phase 0: the user sees a sentence, never Debug output.
        Err(e) => eprintln!("{}", e.user_message()),
    }
}

fn prompt_yes(question: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim(), "y" | "Y" | "yes")
}

fn print_help() {
    println!(
        "davimci {}\n\n\
         usage: davimci [project|media] [-c <:command>]...\n\n\
         options:\n  \
           -c <cmd>    run a : command after opening (repeatable)\n  \
           --version   print the version\n  \
           -h, --help  this text\n\n\
         no frontend is implemented yet (plan.md Phase 9); this drives the\n\
         project lifecycle from spec \u{a7}12.",
        env!("CARGO_PKG_VERSION")
    );
}

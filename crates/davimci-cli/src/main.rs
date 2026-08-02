//! davimci binary entrypoint.
//!
//! There is no *window* yet (plan.md Phase 9c's shell), but the editor is
//! assembled: `-k` runs a scripted key session through the real editor -
//! key grammar, command layer, MLT backend, presenter and transport - with
//! the headless frontend standing in for the window. That makes everything
//! except pixel output exercisable from the command line.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use davimci_app::{App, Event, Surface};
use davimci_backend::RenderBackend;
use davimci_cli::{Editor, ExOutcome, OnRecovery, Workspace};
use davimci_core::{Classify, Fps, Resolution};
use davimci_headless::HeadlessFrontend;
use davimci_present::{Host as PresentHost, Presenter};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut open: Option<PathBuf> = None;
    let mut commands: Vec<String> = Vec::new();
    let mut keys: Option<String> = None;
    let mut ticks: u32 = 0;

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
            "-k" => keys = Some(args.next().context("-k needs a key sequence")?),
            "--ticks" => {
                ticks = args
                    .next()
                    .context("--ticks needs a count")?
                    .parse()
                    .context("--ticks needs a number")?;
            }
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

    if let Some(script) = keys {
        return run_session(ws, &script, ticks);
    }

    for line in ws.list() {
        println!("{line}");
    }
    Ok(())
}

/// Drive a scripted session through the whole editor.
///
/// This is the real thing minus the window: the same `App`, `Editor` and
/// `RenderBackend` a GUI would use, with `HeadlessFrontend` in the window's
/// place. `--ticks` runs presentation ticks afterwards so playback started
/// with `<Space><Space>` actually advances.
fn run_session(ws: Workspace, script: &str, ticks: u32) -> Result<()> {
    let session = ws.current_session();
    let props = session.timeline().props;

    // MLT is only touched here, in the binary: no frontend may reference it.
    let backend: Box<dyn RenderBackend> = match davimci_mlt::MltBackend::new(props) {
        Ok(b) => Box::new(b),
        Err(e) => {
            eprintln!("{e}");
            eprintln!("falling back to the mock backend; preview will be synthetic");
            Box::new(davimci_backend::MockBackend::new(props.resolution))
        }
    };

    let presenter = Presenter::new(
        PresentHost::Embedded,
        Resolution {
            width: 640,
            height: 360,
        },
        Fps::new(props.fps.num, props.fps.den).unwrap_or(Fps::FPS_60),
    );

    let mut editor = Editor::new(ws, backend, presenter);
    let mut app = App::new(session);
    editor.prime(app.session());

    let mut frontend = HeadlessFrontend::script(
        Surface {
            columns: 100,
            rows: 6,
        },
        script,
    );
    app.run(&mut frontend, &mut editor)?;

    for _ in 0..ticks {
        app.event(Event::Tick, &mut editor);
    }
    if let Some(swapped) = editor.take_session_swap() {
        app.replace_session(swapped);
    }

    for m in editor.take_notices() {
        println!("{}", m.text);
    }
    for m in app.messages().history() {
        println!("{}", m.text);
    }
    print!("{}", app.view().dump());
    if let Some(p) = editor.presentation() {
        println!(
            "preview: frame {:?} in {}x{} (quad {}x{}), pacing {:?}",
            p.position.map(davimci_core::Frame::get),
            p.surface.width,
            p.surface.height,
            p.quad.width,
            p.quad.height,
            editor.presenter().stats()
        );
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
           -k <keys>   run a vim-style key sequence through the editor\n  \
           --ticks <n> presentation ticks to run after the keys\n  \
           --version   print the version\n  \
           -h, --help  this text\n\n\
         there is no window yet (plan.md Phase 9c's shell). -c drives the\n\
         spec \u{a7}12 lifecycle; -k drives the whole editor - key grammar,\n\
         commands, MLT backend, presenter and transport - with the headless\n\
         frontend in the window's place.",
        env!("CARGO_PKG_VERSION")
    );
}

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
use davimci_cli::{Editor, ExCommand, ExOutcome, OnRecovery, Workspace};
use davimci_core::{Classify, Fps, Resolution};
use davimci_headless::HeadlessFrontend;
use davimci_present::{Host as PresentHost, Presenter};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut open: Option<PathBuf> = None;
    let mut commands: Vec<String> = Vec::new();
    let mut keys: Option<String> = None;
    let mut ticks: u32 = 0;
    #[allow(unused_mut, unused_assignments)]
    let mut no_window = false;

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
            "--no-window" => no_window = true,
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
        // Straight to the command, not through the `:` parser: a path from
        // argv is already exact, and stringifying it just to re-split it on
        // whitespace is how filenames with spaces get lost.
        report(ws.run_command(&davimci_cli::ExCommand::Edit(path), choice));
    }

    // Export needs a render backend, which a bare workspace has no business
    // owning. When a `-c` line asks for one, run the whole list through a
    // real editor instead - that is what makes batch export from a script
    // possible.
    if commands.iter().any(|l| needs_backend(l)) {
        return run_commands_with_editor(ws, &commands);
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

    // With no script and no `:` commands, the editor is what the user asked
    // for: open the window.
    #[cfg(feature = "window")]
    if commands.is_empty() && !no_window {
        return run_window(ws);
    }

    for line in ws.list() {
        println!("{line}");
    }
    Ok(())
}

/// True for `:` lines only the editor can answer.
fn needs_backend(line: &str) -> bool {
    matches!(
        davimci_cli::excmd::parse(line),
        Ok(ExCommand::Export { .. }
            | ExCommand::Render { .. }
            | ExCommand::Presets
            | ExCommand::CancelRender)
    )
}

/// Run `-c` lines through the assembled editor, so exporting works with no
/// window. Ticks until any export finishes, because a script that returned
/// before the file was written would be useless.
fn run_commands_with_editor(ws: Workspace, commands: &[String]) -> Result<()> {
    let session = ws.current_session();
    let (backend, presenter) = engine_for(&session);
    let mut editor = Editor::new(ws, backend, presenter);
    let mut app = App::new(session);
    app.set_command_candidates(davimci_cli::excmd::vocabulary());
    editor.prime(app.session());

    for line in commands {
        app.event(Event::Command(line.clone()), &mut editor);
        if let Some(m) = app.view().message {
            println!("{}", m.text);
        }
        // An export is a background job; wait it out before the next line.
        while editor.exporter().is_running() {
            app.event(Event::Tick, &mut editor);
            // Transport and export speak through notices; without draining
            // them the final "exported ..." never reaches the terminal.
            for notice in editor.take_notices() {
                app.notify(notice);
            }
            if let Some(job) = app.view().job {
                print!("\r{} {}%   ", job.label, job.percent());
                let _ = std::io::Write::flush(&mut std::io::stdout());
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if davimci_app::Host::wants_quit(&editor) {
            break;
        }
    }
    // Drain the last notices, e.g. "exported /path".
    app.event(Event::Tick, &mut editor);
    for notice in editor.take_notices() {
        app.notify(notice);
    }
    if let Some(m) = app.view().message {
        println!("\r{}   ", m.text);
    }
    Ok(())
}

/// Open the editor window.
#[cfg(feature = "window")]
fn run_window(ws: Workspace) -> Result<()> {
    let session = ws.current_session();
    let (backend, presenter) = engine_for(&session);
    let mut editor = davimci_cli::Editor::new(ws, backend, presenter);
    let mut app = App::new(session);
    app.set_command_candidates(davimci_cli::excmd::vocabulary());
    editor.prime(app.session());
    davimci_cli::Window::new(app, editor)
        .run()
        .map_err(|e| anyhow::anyhow!("the window could not open: {e}"))
}

/// Build the render backend and presenter for a session.
///
/// MLT is only touched here, in the binary: no frontend may reference it
/// (spec §10.1). A missing or broken `libmlt` degrades to the mock backend
/// rather than refusing to start, so editing still works without a working
/// decoder (Phase 0: recoverable errors degrade locally).
fn engine_for(session: &davimci_cmd::Session) -> (Box<dyn RenderBackend>, Presenter) {
    let props = session.timeline().props;
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
    (backend, presenter)
}

/// Drive a scripted session through the whole editor.
///
/// This is the real thing minus the window: the same `App`, `Editor` and
/// `RenderBackend` a GUI would use, with `HeadlessFrontend` in the window's
/// place. `--ticks` runs presentation ticks afterwards so playback started
/// with `<Space><Space>` actually advances.
fn run_session(ws: Workspace, script: &str, ticks: u32) -> Result<()> {
    let session = ws.current_session();
    let (backend, presenter) = engine_for(&session);
    let mut editor = Editor::new(ws, backend, presenter);
    let mut app = App::new(session);
    app.set_command_candidates(davimci_cli::excmd::vocabulary());
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
           --no-window stay on the command line instead of opening a window\n  \
           --version   print the version\n  \
           -h, --help  this text\n\n\
         with no -c and no -k, davimci opens the editor window. -c drives\n\
         the spec \u{a7}12 lifecycle from the command line; -k runs a scripted\n\
         key session through the whole editor with the headless frontend in\n\
         the window's place.",
        env!("CARGO_PKG_VERSION")
    );
}

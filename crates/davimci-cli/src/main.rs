//! davimci binary entrypoint.
//!
//! There is no *window* yet, but the editor is
//! assembled: `-k` runs a scripted key session through the real editor -
//! key grammar, command layer, MLT backend, presenter and transport - with
//! the headless frontend standing in for the window. That makes everything
//! except pixel output exercisable from the command line.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use davimci_app::{App, Event, Surface};
use davimci_backend::RenderBackend;
use davimci_cli::{AskOnTerminal, Editor, ExCommand, ExOutcome, OnRecovery, Plugins, Workspace};
use davimci_core::{Classify, Fps, Resolution};
use davimci_headless::HeadlessFrontend;
use davimci_present::{Host as PresentHost, Presenter};

/// Everything the command line can ask for, once parsed.
#[derive(Debug, Default)]
struct Args {
    open: Option<PathBuf>,
    commands: Vec<String>,
    keys: Option<String>,
    script: Option<PathBuf>,
    ticks: u32,
    no_window: bool,
    tui: bool,
    numbers: davimci_cli::Numbers,
}

/// `--help` and `--version` answer themselves and stop; anything else is a
/// session to run.
enum Invocation {
    Done,
    Run(Box<Args>),
}

impl Args {
    fn parse(command_line: impl Iterator<Item = String>) -> Result<Invocation> {
        let mut words = command_line;

        let mut args = Self::default();
        while let Some(arg) = words.next() {
            match arg.as_str() {
                "-h" | "--help" => {
                    print_help();
                    return Ok(Invocation::Done);
                }
                "--version" => {
                    println!("davimci {}", env!("CARGO_PKG_VERSION"));
                    return Ok(Invocation::Done);
                }
                "-c" => args
                    .commands
                    .push(words.next().context("-c needs a command")?),
                "-k" => args.keys = Some(words.next().context("-k needs a key sequence")?),
                "--script" => {
                    args.script = Some(PathBuf::from(
                        words.next().context("--script needs a path")?,
                    ));
                }
                "--no-window" => args.no_window = true,
                "--tui" => {
                    if cfg!(not(feature = "tui")) {
                        anyhow::bail!(
                            "this build has no terminal frontend; rebuild with --features tui"
                        );
                    }
                    args.tui = true;
                }
                "--numbers" => {
                    let value = words.next().context("--numbers needs a mode")?;
                    args.numbers = davimci_cli::Numbers::parse(&value).with_context(|| {
                        format!("--numbers takes none, absolute or relative, not {value}")
                    })?;
                }
                "--ticks" => {
                    args.ticks = words
                        .next()
                        .context("--ticks needs a count")?
                        .parse()
                        .context("--ticks needs a number")?;
                }
                other if other.starts_with('-') => {
                    anyhow::bail!("unknown option {other}; try --help")
                }
                other => args.open = Some(PathBuf::from(other)),
            }
        }
        Ok(Invocation::Run(Box::new(args)))
    }
}

fn main() -> Result<()> {
    match Args::parse(std::env::args().skip(1))? {
        Invocation::Done => Ok(()),
        Invocation::Run(args) => run(*args),
    }
}

fn run(args: Args) -> Result<()> {
    let root = std::env::current_dir().context("the working directory is unreadable")?;
    let mut ws = Workspace::new(root);

    if let Some(path) = args.open {
        open_project(&mut ws, path);
    }

    // Export needs a render backend, which a bare workspace has no business
    // owning. When a `-c` line asks for one, run the whole list through a
    // real editor instead - that is what makes batch export from a script
    // possible.
    if args.commands.iter().any(|l| needs_backend(l)) {
        run_commands_with_editor(ws, &args.commands);
        return Ok(());
    }

    for line in &args.commands {
        report(ws.run(line, OnRecovery::Discard));
        if ws.should_quit() {
            return Ok(());
        }
    }

    if let Some(path) = args.script {
        return run_script(ws, &path);
    }

    if let Some(keys) = args.keys {
        return run_session(ws, &keys, args.ticks);
    }

    #[cfg(feature = "tui")]
    if args.tui {
        return run_tui(ws, args.numbers);
    }
    // Without the feature the flag never gets this far, but the bindings are
    // still read so the parser and the build agree.
    let _ = (args.tui, args.numbers);

    // With no script and no `:` commands, the editor is what the user asked
    // for: open the window.
    #[cfg(feature = "window")]
    if args.commands.is_empty() && !args.no_window {
        return run_window(ws, args.numbers);
    }

    for line in ws.list() {
        println!("{line}");
    }
    Ok(())
}

/// Open the project named on argv, asking about any autosave first.
fn open_project(ws: &mut Workspace, path: PathBuf) {
    let choice = match ws.pending_recovery(&path) {
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
    // Straight to the command, not through the `:` parser: a path from argv
    // is already exact, and stringifying it just to re-split it on
    // whitespace is how filenames with spaces get lost.
    report(ws.run_command(&davimci_cli::ExCommand::Edit(path), choice));
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
fn run_commands_with_editor(ws: Workspace, commands: &[String]) {
    let (mut app, mut editor) = assemble(ws);

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
}

/// Open the editor window.
#[cfg(feature = "window")]
fn run_window(ws: Workspace, numbers: davimci_cli::Numbers) -> Result<()> {
    let (app, mut editor) = assemble(ws);
    editor.set_numbers(numbers);
    davimci_cli::Window::new(app, editor)
        .run()
        .map_err(|e| anyhow::anyhow!("the window could not open: {e}"))
}

/// Run the editor in the terminal.
#[cfg(feature = "tui")]
fn run_tui(ws: Workspace, numbers: davimci_cli::Numbers) -> Result<()> {
    // A terminal cannot hold the picture, so the preview is detached before
    // the editor is assembled around it.
    let (app, mut editor) = assemble_with(ws, PresentHost::Detached);
    editor.set_numbers(numbers);
    davimci_cli::tui::run(app, editor)
}

/// Load user config and assemble the editor around it.
///
/// The config is loaded before the `App` exists because it decides what the
/// keymap is: a user binding has to be in the table the grammar consults,
/// not layered on afterwards by whoever remembers to.
fn assemble(ws: Workspace) -> (App, Editor) {
    assemble_with(ws, PresentHost::Embedded)
}

fn assemble_with(ws: Workspace, host: PresentHost) -> (App, Editor) {
    let root = ws.root().to_path_buf();
    let mut plugins = Plugins::load(
        davimci_lua::ConfigPaths::from_env().as_ref(),
        &root,
        &AskOnTerminal,
    );
    let notices = plugins.take_notices();
    let keymap = plugins.keymap();
    let jump = plugins.timeline_config().jump;

    let session = ws.current_session();
    let (backend, presenter) = engine_for(&session, host);
    let mut editor = Editor::new(ws, backend, presenter).with_plugins(plugins);
    let mut app = App::with_keymap(session, keymap);
    app.set_jump_config(jump);
    app.set_command_vocabulary(davimci_cli::excmd::vocabulary());
    // A config that failed to load says so on the status line and the editor
    // starts anyway: one broken plugin is not a reason to refuse to open.
    for notice in notices {
        app.notify(notice);
    }
    editor.prime(app.session());
    (app, editor)
}

/// Build the render backend and presenter for a session.
///
/// MLT is only touched here, in the binary: no frontend may reference it
///. A missing or broken `libmlt` degrades to the mock backend
/// rather than refusing to start, so editing still works without a working
/// decoder (Phase 0: recoverable errors degrade locally).
fn engine_for(
    session: &davimci_cmd::Session,
    host: PresentHost,
) -> (Box<dyn RenderBackend>, Presenter) {
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
        host,
        Resolution {
            width: 640,
            height: 360,
        },
        Fps::new(props.fps.num, props.fps.den).unwrap_or(Fps::FPS_60),
    );
    (backend, presenter)
}

/// Run a scripted-session file through the whole editor.
///
/// The same format the integration tests use, so a failing test and a bug
/// report are the same artefact: the assertions are checked here too, and a
/// failure is an error exit rather than a printed grumble.
fn run_script(ws: Workspace, path: &std::path::Path) -> Result<()> {
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("{} could not be read", path.display()))?;
    let script = davimci_headless::Script::parse(&source)
        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;

    let (mut app, mut editor) = assemble(ws);
    let report = script.run(&mut app, &mut editor);
    print!("{}", report.summary());
    if report.passed() {
        Ok(())
    } else {
        anyhow::bail!(
            "{}: {} assertion(s) failed",
            path.display(),
            report.failures.len()
        )
    }
}

/// Drive a scripted session through the whole editor.
///
/// This is the real thing minus the window: the same `App`, `Editor` and
/// `RenderBackend` a GUI would use, with `HeadlessFrontend` in the window's
/// place. `--ticks` runs presentation ticks afterwards so playback started
/// with `<Space><Space>` actually advances.
fn run_session(ws: Workspace, script: &str, ticks: u32) -> Result<()> {
    let (mut app, mut editor) = assemble(ws);

    let mut frontend = HeadlessFrontend::script(
        Surface {
            columns: 100,
            rows: 6,
            // A scripted session draws nothing, so it decodes no
            // thumbnails either.
            thumbnail_columns: 0,
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
           --script <f> run a scripted-session file (keys plus assertions)\n  \
           --ticks <n> presentation ticks to run after the keys\n  \
           --no-window stay on the command line instead of opening a window\n  \
           --tui       run in the terminal instead of a window\n  \
           --numbers <mode> ruler jump-point numbers, window or terminal:\n              \
                            none (default), absolute or relative\n  \
           --version   print the version\n  \
           -h, --help  this text\n\n\
         with no -c and no -k, davimci opens the editor window. -c drives\n\
         the project lifecycle from the command line; -k runs a scripted\n\
         key session through the whole editor with the headless frontend in\n\
         the window's place.",
        env!("CARGO_PKG_VERSION")
    );
}

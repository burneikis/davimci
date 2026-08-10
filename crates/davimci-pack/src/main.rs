//! `davimci-pack`: install, pin and remove davimci plugins.
//!
//! The editor never runs this. It writes a directory and a lockfile, and the
//! editor reads them on its next start, so nothing here can change what a
//! running session already loaded.

use anyhow::{Result, bail};
use davimci_pack::{Kind, Lock, Paths, Spec, add, list, remove, sync, update};

const USAGE: &str = "\
davimci-pack - install davimci plugins

  davimci-pack add [--opt] <user/repo | url>   install and pin
  davimci-pack update [name...]                pull and re-pin
  davimci-pack sync                            install what the lockfile names
  davimci-pack remove <name>                   delete an installed plugin
  davimci-pack list                            what is installed, and its pin

Plugins install under <site>/pack/fetched/{start,opt}/ and are pinned in
<config>/davimci-lock.json. Enable one in plugins.lua; an opt plugin runs
when davimci.pack.add names it.";

fn main() {
    if let Err(e) = run() {
        eprintln!("davimci-pack: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths = Paths::from_env()?;
    let lockfile = paths.lockfile();
    let mut lock = Lock::read(&lockfile)?;

    match args.first().map(String::as_str) {
        Some("add") => {
            let rest = &args[1..];
            let kind = if rest.iter().any(|a| a == "--opt") {
                Kind::Opt
            } else {
                Kind::Start
            };
            let Some(spec) = rest.iter().find(|a| !a.starts_with("--")) else {
                bail!("add needs a plugin, as 'user/repo' or a git URL");
            };
            let spec = Spec::parse(spec)?;
            let pin = add(&paths, &spec, kind, None)?;
            println!("installed {} at {}", spec.name, short(&pin.rev));
            lock.plugins.insert(spec.name, pin);
            lock.write(&lockfile)?;
        }
        Some("update") => {
            let names: Vec<String> = if args.len() > 1 {
                args[1..].to_vec()
            } else {
                lock.plugins.keys().cloned().collect()
            };
            if names.is_empty() {
                println!("nothing is installed");
            }
            for name in names {
                let pin = update(&paths, &name)?;
                let changed = lock.plugins.get(&name).is_none_or(|p| p.rev != pin.rev);
                println!(
                    "{name} {}",
                    if changed {
                        format!("updated to {}", short(&pin.rev))
                    } else {
                        "already current".to_string()
                    }
                );
                lock.plugins.insert(name, pin);
            }
            lock.write(&lockfile)?;
        }
        Some("sync") => {
            let restored = sync(&paths, &lock)?;
            if restored.is_empty() {
                println!("every pinned plugin is already installed");
            } else {
                println!("installed {}", restored.join(", "));
            }
        }
        Some("remove") => {
            let Some(name) = args.get(1) else {
                bail!("remove needs the name of an installed plugin");
            };
            remove(&paths, name)?;
            lock.plugins.remove(name);
            lock.write(&lockfile)?;
            println!("removed {name}");
        }
        Some("list") => {
            let lines = list(&paths, &lock);
            if lines.is_empty() {
                println!("nothing is installed");
            }
            for line in lines {
                println!("{line}");
            }
        }
        Some("-h" | "--help" | "help") | None => println!("{USAGE}"),
        Some(other) => bail!("'{other}' is not a davimci-pack command; try --help"),
    }
    Ok(())
}

fn short(rev: &str) -> String {
    rev.chars().take(9).collect()
}

mod app;
mod auth;
mod clipboard;
mod config;
mod gmail;
mod keys;
mod ui;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "mnml-msg-gmail",
    version,
    about = "Gmail browse + send for mnml — terminal TUI"
)]
struct Cli {
    /// Print resolved config + auth state and exit.
    #[arg(long)]
    check: bool,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the loopback OAuth flow, persist the token, then exit.
    Auth {
        /// Delete the persisted token instead.
        #[arg(long)]
        logout: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.check {
        return run_check();
    }

    if let Some(Cmd::Auth { logout }) = cli.cmd {
        return run_auth(logout);
    }

    // Default: launch the TUI.
    let cfg = config::load()?;
    let creds = match auth::ClientCreds::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!();
            print_setup_hint();
            std::process::exit(2);
        }
    };
    let token = match auth::ensure_fresh(&creds) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!();
            eprintln!("Run `mnml-msg-gmail auth` to sign in.");
            std::process::exit(2);
        }
    };

    let mut app = app::App::new(cfg, creds, token)?;
    ui::run(&mut app)
}

fn run_check() -> Result<()> {
    let cfg = config::load();
    let creds = auth::ClientCreds::from_env();
    let token_on_disk = auth::load_token().ok().flatten();

    println!("{} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    println!("config: {}", config::config_path().display());
    match &cfg {
        Ok(cfg) => {
            println!("tabs:");
            for (i, t) in cfg.tabs.iter().enumerate() {
                println!(
                    "  {} ({}): kind={} query={:?}",
                    i + 1,
                    t.name,
                    t.kind,
                    t.query
                );
            }
        }
        Err(e) => println!("config: ERROR — {e}"),
    }
    println!();
    println!("env: GMAIL_CLIENT_ID={}", mask_env("GMAIL_CLIENT_ID"));
    println!(
        "env: GMAIL_CLIENT_SECRET={}",
        mask_env("GMAIL_CLIENT_SECRET")
    );
    println!("token: {}", auth::token_path().display());

    match (&creds, &token_on_disk) {
        (Err(e), _) => {
            println!();
            println!("auth: ERROR — {e}");
            std::process::exit(2);
        }
        (Ok(_), None) => {
            println!();
            println!("auth: MISSING — run `mnml-msg-gmail auth` to sign in");
            std::process::exit(2);
        }
        (Ok(creds), Some(_tok)) => {
            // Probe — refresh if needed, then call /profile.
            match auth::ensure_fresh(creds) {
                Ok(tok) => match gmail::get_profile(&tok) {
                    Ok(p) => {
                        println!();
                        println!("auth: ok");
                        println!("email: {}", p.email_address);
                        println!("messages_total: {}", p.messages_total);
                        println!("threads_total:  {}", p.threads_total);
                    }
                    Err(e) => {
                        println!();
                        println!("auth: ERROR — profile probe failed: {e}");
                        std::process::exit(2);
                    }
                },
                Err(e) => {
                    println!();
                    println!("auth: ERROR — refresh failed: {e}");
                    println!("Run `mnml-msg-gmail auth` to re-consent.");
                    std::process::exit(2);
                }
            }
        }
    }
    if cfg.is_err() {
        std::process::exit(2);
    }
    Ok(())
}

fn run_auth(logout: bool) -> Result<()> {
    if logout {
        match auth::delete_token()? {
            true => println!("Deleted token at {}", auth::token_path().display()),
            false => println!("No token to delete."),
        }
        return Ok(());
    }
    let creds = match auth::ClientCreds::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!();
            print_setup_hint();
            std::process::exit(2);
        }
    };
    let tok = auth::interactive_login(&creds)?;
    let profile = gmail::get_profile(&tok)?;
    println!("Signed in as {}.", profile.email_address);
    Ok(())
}

fn print_setup_hint() {
    eprintln!("setup:");
    eprintln!("  1. Create a Google Cloud project: https://console.cloud.google.com/");
    eprintln!("  2. Enable the Gmail API for that project.");
    eprintln!("  3. Configure OAuth consent screen (External, Testing mode is fine).");
    eprintln!("  4. Add your Gmail address as a Test user.");
    eprintln!("  5. Create OAuth credentials of type \"Desktop app\".");
    eprintln!("  6. Export the credentials:");
    eprintln!("       export GMAIL_CLIENT_ID=...");
    eprintln!("       export GMAIL_CLIENT_SECRET=...");
    eprintln!("  7. Run `mnml-msg-gmail auth` to do the loopback consent flow.");
    eprintln!();
    eprintln!("See the README for the full walkthrough.");
}

fn mask_env(name: &str) -> String {
    // 2026-06-08 sibling-sweep fix: was leaking the last 4 chars of
    // the env value to stderr / stdout. Modern API keys (Gmail OAuth
    // client secret etc.) are short enough that 4 chars is ~20% of
    // the entropy — a real, if low-exposure, leak. Also fixes a
    // latent panic: `&v[v.len()-4..]` indexes into the BYTE buffer,
    // so a 2-byte-per-char tail like `é` could panic with a non-
    // char-boundary slice. Just report the length.
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => format!("set ({} chars)", v.len()),
        _ => "(unset)".into(),
    }
}

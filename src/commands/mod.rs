pub mod ask;
pub mod capabilities;
pub mod doctor;
pub mod schema;

use clap::Parser;
use clap::error::ErrorKind;

use crate::cli::{Cli, Commands, args_with_default_ask, edit_distance};
use crate::envelope::{
    Budget, CostDollars, Diagnostics, ErrorEnvelope, SuccessEnvelope, emit_error, emit_success,
};

pub struct CommandSuccess {
    pub envelope: SuccessEnvelope,
    pub exit_code: i32,
    pub hint: Option<&'static str>,
}

pub fn run() -> i32 {
    let raw_args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let force_json = raw_args.iter().any(|arg| arg == "--json");
    let args = args_with_default_ask(raw_args.clone());
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err)
            if matches!(
                err.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            if force_json {
                let text = err.to_string();
                let command = if matches!(err.kind(), ErrorKind::DisplayVersion) {
                    "version"
                } else {
                    "help"
                };
                let envelope = SuccessEnvelope::new(
                    command,
                    serde_json::json!({"text": text}),
                    CostDollars {
                        model: 0.0,
                        search: 0.0,
                        total: 0.0,
                        estimated: false,
                    },
                    Budget { hit: None },
                    Diagnostics {
                        duration_ms: 0,
                        retries: 0,
                    },
                    None,
                );
                emit_success(&envelope, true);
                return 0;
            }
            err.exit();
        }
        Err(err) => return emit_parse_error(&raw_args, err),
    };

    let command_name = command_name(&cli.command);
    match dispatch(cli) {
        Ok(success) => {
            emit_success(
                &success.envelope,
                force_json || success.envelope.command != command_name,
            );
            if let Some(hint) = success.hint
                && !force_json
                && std::io::IsTerminal::is_terminal(&std::io::stdout())
            {
                eprintln!("hint: {hint}");
            }
            success.exit_code
        }
        Err(err) => {
            let envelope = ErrorEnvelope::from_error(command_name, &err, None);
            emit_error(&envelope, force_json)
        }
    }
}

fn dispatch(cli: Cli) -> Result<CommandSuccess, crate::error::ReconError> {
    let global = cli.global;
    match cli.command {
        Commands::Ask(args) => ask::run(&global, &args),
        Commands::Doctor(args) => doctor::run(&global, &args),
        Commands::Capabilities => capabilities::run(&global),
        Commands::Schema(args) => schema::run(&global, &args),
    }
}

fn command_name(command: &Commands) -> &'static str {
    match command {
        Commands::Ask(_) => "ask",
        Commands::Doctor(_) => "doctor",
        Commands::Capabilities => "capabilities",
        Commands::Schema(_) => "schema",
    }
}

fn emit_parse_error(raw_args: &[std::ffi::OsString], err: clap::Error) -> i32 {
    let message = clean_clap_message(&err.to_string());
    let suggested_fix = suggested_fix(raw_args, &message);
    let command = parse_command_name(raw_args);
    let recon_err = crate::error::ReconError::usage(format!("usage error: {message}"))
        .with_suggested_fix(suggested_fix);
    let envelope = ErrorEnvelope::from_error(command, &recon_err, None);
    emit_error(&envelope, false)
}

fn clean_clap_message(message: &str) -> String {
    message
        .lines()
        .filter(|line| !line.trim_start().starts_with("Usage:"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn parse_command_name(args: &[std::ffi::OsString]) -> &'static str {
    for arg in args.iter().skip(1) {
        let text = arg.to_string_lossy();
        if text.starts_with('-') {
            continue;
        }
        return match text.as_ref() {
            "ask" => "ask",
            "doctor" => "doctor",
            "capabilities" => "capabilities",
            "schema" => "schema",
            _ => "recon",
        };
    }
    "recon"
}

fn suggested_fix(args: &[std::ffi::OsString], message: &str) -> String {
    if let Some(tip) = message.lines().find(|line| line.contains("similar")) {
        return tip.trim().trim_start_matches("tip:").trim().to_string();
    }

    let bad = args
        .iter()
        .skip(1)
        .map(|arg| arg.to_string_lossy())
        .find(|arg| arg.starts_with("--") && !known_flags().contains(&arg.as_ref()));
    if let Some(bad) = bad
        && let Some(best) = best_match(&bad, known_flags())
    {
        return format!("did you mean '{best}'?");
    }

    let command = args
        .iter()
        .skip(1)
        .map(|arg| arg.to_string_lossy())
        .find(|arg| !arg.starts_with('-'));
    if let Some(command) = command
        && let Some(best) = best_match(&command, &["ask", "doctor", "capabilities", "schema"])
    {
        return format!("did you mean '{best}'?");
    }

    "Run `recon --help` or `recon capabilities` for the supported contract.".to_string()
}

fn known_flags() -> &'static [&'static str] {
    &[
        "--json",
        "--model",
        "--depth",
        "--max-seconds",
        "--max-dollars",
        "--verify",
        "--brief",
        "--dry-run",
        "--online",
        "--help",
        "--version",
    ]
}

fn best_match<'a>(needle: &str, choices: &'a [&'a str]) -> Option<&'a str> {
    choices
        .iter()
        .copied()
        .map(|choice| (choice, edit_distance(needle, choice)))
        .filter(|(_, distance)| *distance <= 3)
        .min_by_key(|(_, distance)| *distance)
        .map(|(choice, _)| choice)
}

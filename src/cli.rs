use std::ffi::OsString;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = env!("CARGO_PKG_NAME"),
    version,
    about = "Source-verified research at function-call latency.",
    long_about = None,
    arg_required_else_help = true,
    color = clap::ColorChoice::Never,
    rename_all = "kebab-case"
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Clone, Args)]
pub struct GlobalArgs {
    /// Force the machine JSON envelope on stdout/stderr.
    #[arg(long, global = true, default_value_t = false)]
    pub json: bool,

    /// Override the Cerebras model.
    #[arg(long, global = true, value_name = "MODEL")]
    pub model: Option<String>,

    /// Research depth tier.
    #[arg(long, global = true, value_enum, default_value_t = DepthArg::Standard)]
    pub depth: DepthArg,

    /// Hard wall-clock cap in seconds.
    #[arg(long, global = true, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    pub max_seconds: Option<u64>,

    /// Hard spend cap in dollars.
    #[arg(long, global = true, value_name = "X", value_parser = parse_positive_f64)]
    pub max_dollars: Option<f64>,

    /// Verification policy.
    #[arg(long, global = true, value_enum, default_value_t = VerifyArg::Adaptive)]
    pub verify: VerifyArg,

    /// Add a brief convenience summary when supported by the pipeline.
    #[arg(long, global = true, default_value_t = false)]
    pub brief: bool,

    /// Print planned fan-out and estimated cost without spending.
    #[arg(long, global = true, default_value_t = false)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DepthArg {
    Quick,
    Standard,
    Deep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum VerifyArg {
    Adaptive,
    Paranoid,
    Off,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Run a source-verified research question.
    Ask(AskArgs),
    /// Diagnose config and provider credentials.
    Doctor(DoctorArgs),
    /// Print the machine-readable CLI contract.
    Capabilities,
    /// Print JSON Schema for response and error envelopes.
    Schema(SchemaArgs),
}

#[derive(Debug, Clone, Args)]
pub struct AskArgs {
    /// Research question. Multiple words are joined with spaces.
    #[arg(value_name = "QUESTION", num_args = 0..)]
    pub question: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub struct DoctorArgs {
    /// Probe Cerebras and Exa online with minimal calls.
    #[arg(long, default_value_t = false)]
    pub online: bool,
}

#[derive(Debug, Clone, Args)]
pub struct SchemaArgs {
    /// Which schema to print.
    #[arg(value_enum, default_value_t = SchemaTarget::All)]
    pub target: SchemaTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SchemaTarget {
    Response,
    Error,
    All,
}

impl From<DepthArg> for crate::tiers::Depth {
    fn from(value: DepthArg) -> Self {
        match value {
            DepthArg::Quick => crate::tiers::Depth::Quick,
            DepthArg::Standard => crate::tiers::Depth::Standard,
            DepthArg::Deep => crate::tiers::Depth::Deep,
        }
    }
}

impl From<VerifyArg> for crate::pipeline::VerifyPolicy {
    fn from(value: VerifyArg) -> Self {
        match value {
            VerifyArg::Adaptive => crate::pipeline::VerifyPolicy::Adaptive,
            VerifyArg::Paranoid => crate::pipeline::VerifyPolicy::Paranoid,
            VerifyArg::Off => crate::pipeline::VerifyPolicy::Off,
        }
    }
}

pub fn args_with_default_ask(args: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
    let mut args: Vec<OsString> = args.into_iter().collect();
    if first_commandish_arg(&args).is_some_and(|idx| should_default_to_ask(&args[idx])) {
        args.insert(1, OsString::from("ask"));
    }
    args
}

fn first_commandish_arg(args: &[OsString]) -> Option<usize> {
    let mut idx = 1;
    while idx < args.len() {
        let token = args[idx].to_string_lossy();
        if token == "--" {
            return (idx + 1 < args.len()).then_some(idx + 1);
        }
        if token.starts_with('-') {
            if flag_takes_value(&token) && !token.contains('=') {
                idx += 2;
            } else {
                idx += 1;
            }
            continue;
        }
        return Some(idx);
    }
    None
}

fn flag_takes_value(flag: &str) -> bool {
    matches!(
        flag,
        "--model" | "--depth" | "--max-seconds" | "--max-dollars" | "--verify"
    )
}

fn should_default_to_ask(token: &OsString) -> bool {
    let token = token.to_string_lossy();
    if matches!(token.as_ref(), "ask" | "doctor" | "capabilities" | "schema") {
        return false;
    }
    !looks_like_command_typo(&token)
}

fn looks_like_command_typo(token: &str) -> bool {
    ["ask", "doctor", "capabilities", "schema"]
        .iter()
        .any(|command| edit_distance(token, command) <= 2)
}

fn parse_positive_f64(value: &str) -> Result<f64, String> {
    let parsed = value
        .parse::<f64>()
        .map_err(|_| format!("{value:?} is not a number"))?;
    if parsed.is_finite() && parsed >= 0.0 {
        Ok(parsed)
    } else {
        Err("value must be a non-negative finite number".to_string())
    }
}

pub(crate) fn edit_distance(a: &str, b: &str) -> usize {
    let mut costs: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = costs[0];
        costs[0] = i + 1;
        for (j, cb) in b.chars().enumerate() {
            let old = costs[j + 1];
            costs[j + 1] = if ca == cb {
                prev
            } else {
                1 + prev.min(costs[j]).min(costs[j + 1])
            };
            prev = old;
        }
    }
    costs[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn quoted_question_defaults_to_ask() {
        let args = args_with_default_ask(os(&["receipts", "what is x?"]));
        let cli = Cli::try_parse_from(args).unwrap();
        assert!(matches!(cli.command, Commands::Ask(_)));
    }

    #[test]
    fn global_flags_before_default_question_are_preserved() {
        let args = args_with_default_ask(os(&["receipts", "--json", "--depth", "quick", "what?"]));
        let cli = Cli::try_parse_from(args).unwrap();
        assert!(cli.global.json);
        assert_eq!(cli.global.depth, DepthArg::Quick);
        assert!(matches!(cli.command, Commands::Ask(_)));
    }

    #[test]
    fn command_typos_are_left_for_clap_suggestions() {
        let args = args_with_default_ask(os(&["receipts", "capabilties"]));
        let err = Cli::try_parse_from(args).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidSubcommand);
    }
}

use clap::Parser;

#[derive(Debug, Parser, PartialEq, Eq)]
#[command(name = fastab_util::PTY_BINARY_NAME, version, about)]
pub struct Cli {
    #[arg(last = true)]
    pub command: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_app() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn test_command() {
        let cli = Cli::parse_from(["fterm"]);
        assert_eq!(cli, Cli { command: None });

        let cli = Cli::parse_from(["fterm", "--", "exe", "arg1", "arg2"]);
        assert_eq!(
            cli,
            Cli {
                command: Some(vec!["exe".into(), "arg1".into(), "arg2".into()])
            }
        );
    }
}

//! Command-line arguments, taken by name so each tool can parse its own.

/// Arguments not yet taken. Everything after `--` is positional.
#[derive(Debug)]
pub struct Arguments {
    options: Vec<String>,
    rest: Vec<String>,
}

impl Arguments {
    /// Holds `arguments`, which exclude the program name.
    pub fn new(arguments: impl IntoIterator<Item = String>) -> Self {
        let mut options: Vec<String> = arguments.into_iter().collect();
        let rest = match options.iter().position(|argument| argument == "--") {
            Some(index) => {
                let rest = options.split_off(index + 1);
                options.pop();
                rest
            }
            None => Vec::new(),
        };
        Self { options, rest }
    }

    /// Takes a flag such as `--force`, reporting whether it was given.
    pub fn flag(&mut self, name: &str) -> bool {
        let before = self.options.len();
        self.options.retain(|argument| argument != name);
        self.options.len() != before
    }

    /// Takes `--name VALUE` or `--name=VALUE`.
    pub fn option(&mut self, name: &str) -> Result<Option<String>, String> {
        let prefix = format!("{name}=");
        let mut value = None;
        let mut index = 0;
        while index < self.options.len() {
            let argument = &self.options[index];
            let found = if argument == name {
                if index + 1 == self.options.len() {
                    return Err(format!("{name} requires a value"));
                }
                self.options.remove(index);
                Some(self.options.remove(index))
            } else if let Some(inline) = argument.strip_prefix(&prefix) {
                let inline = inline.to_owned();
                self.options.remove(index);
                Some(inline)
            } else {
                index += 1;
                None
            };
            if found.is_some() {
                if value.is_some() {
                    return Err(format!("{name} was supplied twice"));
                }
                value = found;
            }
        }
        Ok(value)
    }

    /// Takes an option whose value must be one of `choices`.
    pub fn choice<T: Copy>(
        &mut self,
        name: &str,
        choices: &[T],
        label: fn(T) -> &'static str,
    ) -> Result<Option<T>, String> {
        let Some(value) = self.option(name)? else {
            return Ok(None);
        };
        choices
            .iter()
            .copied()
            .find(|choice| label(*choice) == value)
            .map(Some)
            .ok_or_else(|| {
                let names: Vec<_> = choices.iter().map(|choice| label(*choice)).collect();
                format!("{name} must be one of {}", names.join(", "))
            })
    }

    /// The remaining positional arguments; any other option is an error.
    pub fn finish(self) -> Result<Vec<String>, String> {
        if let Some(unknown) = self
            .options
            .iter()
            .find(|argument| argument.len() > 1 && argument.starts_with('-'))
        {
            return Err(format!("unrecognized argument {unknown}"));
        }
        Ok(self.options.into_iter().chain(self.rest).collect())
    }

    /// Exactly `N` remaining positional arguments.
    pub fn finish_exact<const N: usize>(self) -> Result<[String; N], String> {
        let positionals = self.finish()?;
        let count = positionals.len();
        positionals
            .try_into()
            .map_err(|_| format!("expected {N} argument(s), got {count}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(items: &[&str]) -> Arguments {
        Arguments::new(items.iter().map(|item| (*item).to_owned()))
    }

    #[test]
    fn options_flags_and_positionals() {
        let mut parsed = arguments(&[
            "report.json",
            "--os=linux",
            "--interaction",
            "--mode",
            "editable",
        ]);
        assert_eq!(
            parsed.option("--mode").unwrap().as_deref(),
            Some("editable")
        );
        assert_eq!(parsed.option("--os").unwrap().as_deref(), Some("linux"));
        assert_eq!(parsed.option("--revision").unwrap(), None);
        assert!(parsed.flag("--interaction"));
        assert!(!parsed.flag("--interaction"));
        assert_eq!(parsed.finish().unwrap(), ["report.json"]);
    }

    #[test]
    fn missing_repeated_and_unknown_options_are_errors() {
        assert!(arguments(&["--mode"]).option("--mode").is_err());
        assert!(
            arguments(&["--mode", "a", "--mode=b"])
                .option("--mode")
                .is_err()
        );
        assert!(arguments(&["--unknown"]).finish().is_err());
    }

    #[test]
    fn a_dash_and_everything_after_a_double_dash_are_positional() {
        let mut parsed = arguments(&["-", "--admin", "--", "--admin", "-x"]);
        assert!(parsed.flag("--admin"));
        assert_eq!(parsed.finish().unwrap(), ["-", "--admin", "-x"]);
    }
}

//! Questions the CLI asks on the terminal.

use std::io::{self, IsTerminal, Write};

use anyhow::{Context, Result, bail};

pub(crate) fn prompt(label: &str) -> Result<String> {
    prompt_with_default(label, "")
}

pub(crate) fn prompt_with_default(label: &str, default: &str) -> Result<String> {
    let mut stdout = io::stdout();
    if default.is_empty() {
        write!(stdout, "{label}: ")?;
    } else {
        write!(stdout, "{label} [{default}]: ")?;
    }
    stdout.flush()?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read terminal input")?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        if default.is_empty() {
            bail!("{label} must not be empty");
        }
        Ok(default.to_owned())
    } else {
        Ok(trimmed.to_owned())
    }
}

pub(crate) fn prompt_optional(label: &str) -> Result<Option<String>> {
    let mut stdout = io::stdout();
    write!(stdout, "{label}: ")?;
    stdout.flush()?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read terminal input")?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_owned()))
    }
}

/// Asks for a secret without echoing it.
pub(crate) fn prompt_secret(label: &str) -> Result<String> {
    let secret = prompt_secret_optional(label)?;
    secret.with_context(|| format!("{label} must not be empty"))
}

/// Asks for a secret that may be left empty, without echoing it.
pub(crate) fn prompt_secret_optional(label: &str) -> Result<Option<String>> {
    let secret = rpassword::prompt_password(format!("{label}: "))
        .with_context(|| format!("failed to read the {}", label.to_lowercase()))?;
    Ok((!secret.is_empty()).then_some(secret))
}

pub(crate) fn prompt_recipients(label: &str, current: &[String]) -> Result<Vec<String>> {
    let default = current.join(", ");
    let value = prompt_with_default(label, &default)?;
    Ok(value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect())
}

pub(crate) fn prompt_multiline_with_default(label: &str, default: &str) -> Result<String> {
    println!("{label} (finish with a single '.' on its own line):");
    if !default.is_empty() {
        println!("Current value:");
        println!("{}", crate::output::text(default));
    }

    let mut lines = Vec::new();
    loop {
        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .context("failed to read terminal input")?;
        let trimmed = input.trim_end_matches(['\r', '\n']);
        if trimmed == "." {
            break;
        }
        if trimmed.is_empty() && lines.is_empty() {
            return Ok(default.to_owned());
        }
        lines.push(trimmed.to_owned());
    }

    Ok(lines.join("\n"))
}

pub(crate) fn prompt_yes_no(label: &str, default: bool) -> Result<bool> {
    let default_label = if default { "Y/n" } else { "y/N" };
    let response = prompt_with_default(label, default_label)?;
    match response.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        value if value == default_label.to_ascii_lowercase() => Ok(default),
        _ if response == default_label => Ok(default),
        other => bail!("expected yes or no, got {other}"),
    }
}

/// Asks before something that can't be undone: y/N on a terminal. Off a
/// terminal nothing can answer, so only `yes` (the command's `--yes`)
/// goes ahead.
pub(crate) fn confirm(question: &str, yes: bool) -> Result<bool> {
    confirm_in(question, yes, io::stdin().is_terminal(), || {
        prompt_yes_no(question, false)
    })
}

fn confirm_in(
    question: &str,
    yes: bool,
    terminal: bool,
    ask: impl FnOnce() -> Result<bool>,
) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    if !terminal {
        bail!("{question}? There is no terminal to ask on; pass --yes to go ahead");
    }
    ask()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirm_asks_on_a_terminal_and_needs_yes_elsewhere() {
        let never = || -> Result<bool> { panic!("nothing asks") };
        assert!(confirm_in("Delete it", true, false, never).expect("yes goes ahead"));
        assert!(confirm_in("Delete it", true, true, never).expect("yes goes ahead"));
        let error =
            confirm_in("Delete it", false, false, never).expect_err("refused off a terminal");
        assert!(format!("{error}").contains("pass --yes"), "{error}");
        assert!(!confirm_in("Delete it", false, true, || Ok(false)).expect("asked"));
        assert!(confirm_in("Delete it", false, true, || Ok(true)).expect("asked"));
    }
}

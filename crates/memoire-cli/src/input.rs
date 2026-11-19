//! Secrets and sign-in details, read from a terminal prompt without echo,
//! from standard input, a file or the environment; never from arguments.
use crate::config::{Secret, io_error};
use crate::error::{Error, Result, exit};
use std::io::{BufRead as _, IsTerminal as _, Read as _, Write as _};
use std::path::Path;

fn missing(what: &str) -> Error {
    Error::local(
        "missing_input",
        exit::USAGE,
        format!("no {what} on standard input"),
    )
}

/// The API key for `login`: `--key-file`, else `MEMOIRE_API_KEY`, else
/// standard input (a prompt without echo on a terminal).
pub fn api_key(key_file: Option<&Path>, from_env: Option<&Secret>) -> Result<Secret> {
    let text = if let Some(path) = key_file {
        std::fs::read_to_string(path).map_err(|e| io_error(path, &e))?
    } else if let Some(key) = from_env {
        key.expose().to_owned()
    } else if std::io::stdin().is_terminal() {
        rpassword::prompt_password("API key: ")
            .map_err(|e| Error::local("missing_input", exit::USAGE, e.to_string()))?
    } else {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|e| Error::local("missing_input", exit::USAGE, e.to_string()))?;
        text
    };
    let key = text.trim();
    if key.is_empty() {
        return Err(missing("API key"));
    }
    if !key.starts_with("mzk_") || key.chars().any(char::is_whitespace) {
        return Err(Error::local(
            "invalid_key",
            exit::AUTH,
            "that is not a memoire API key (mzk_…)",
        ));
    }
    Ok(Secret::new(key))
}

/// A member's email and password: prompted on a terminal (the password
/// without echo), else the first two lines of standard input.
pub fn sign_in() -> Result<(String, Secret)> {
    let stdin = std::io::stdin();
    let fail = |e: std::io::Error| Error::local("missing_input", exit::USAGE, e.to_string());
    if stdin.is_terminal() {
        eprint!("Email: ");
        std::io::stderr().flush().map_err(fail)?;
        let mut email = String::new();
        stdin.lock().read_line(&mut email).map_err(fail)?;
        let password = rpassword::prompt_password("Password: ").map_err(fail)?;
        return finish(email, password);
    }
    let mut lines = stdin.lock().lines();
    let email = lines.next().transpose().map_err(fail)?.unwrap_or_default();
    let password = lines.next().transpose().map_err(fail)?.unwrap_or_default();
    finish(email, password)
}

fn finish(email: String, password: String) -> Result<(String, Secret)> {
    let email = email.trim().to_owned();
    if email.is_empty() {
        return Err(missing("email"));
    }
    // A password keeps its spaces; only the line ending goes.
    let password = password.trim_end_matches(['\r', '\n']).to_owned();
    if password.is_empty() {
        return Err(missing("password"));
    }
    Ok((email, Secret::new(password)))
}

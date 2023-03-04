use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::{fs, io, process};

use tracing::{debug, error, info};

use crate::error::Error;

#[derive(Debug, thiserror::Error)]
pub enum GitCliError {
    #[error(transparent)]
    IoError(#[from] io::Error),

    #[error("Fail to execute command `{0}` returning status {1}")]
    CommandError(String, ExitStatus),

    #[error("No parent folder for {0}")]
    NoParentError(PathBuf),
}

#[tracing::instrument(fields(dst = ?dst.as_ref(), url = url.as_ref(), rev = rev.as_ref()))]
pub fn retrieve<P, U, R>(dst: P, url: U, rev: R) -> Result<(), Error>
where
    P: AsRef<Path>,
    R: AsRef<str>,
    U: AsRef<str>,
{
    let dst = dst.as_ref();
    let url = url.as_ref();
    let rev = rev.as_ref();

    // Helper closure to build error
    let build_err = |cmd: String, source: GitCliError| -> Error {
        Error::GitRetrieve {
            dst: dst.to_path_buf(),
            url: url.to_owned(),
            rev: rev.to_owned(),
            source,
            msg: format!("Fail to execute command: `{cmd}`"),
        }
    };

    // Helper closure to run a git command
    let git_cmd = |args: Vec<&str>, current_dir: &Path| -> Result<(), Error> {
        let cmd = format!("git {}", args.join(" "));
        info!("Run command: `{cmd}` in {current_dir:?}");
        let result = process::Command::new("git")
            .args(&args)
            .current_dir(current_dir)
            .status()
            .map_err(|err| build_err(cmd.clone(), GitCliError::IoError(err)))?;
        if result.success() {
            Ok(())
        } else {
            let err = GitCliError::CommandError(cmd.clone(), result);
            Err(build_err(cmd, err))
        }
    };

    if dst.exists() {
        debug!("Repository already exists, update it");
        git_cmd(vec!["fetch", "origin"], dst)?;
    } else if let Some(dst) = dst.parent() {
        debug!("Repository does not exists, create it");
        fs::create_dir_all(dst).map_err(|source| Error::CreateFolder {
            path: dst.to_path_buf(),
            source,
        })?;
        git_cmd(vec!["clone", url, rev], dst)?;
    } else {
        let msg = format!("No parent folder for {dst:?}");
        return Err(build_err(
            msg,
            GitCliError::NoParentError(dst.to_path_buf()),
        ));
    }

    git_cmd(vec!["checkout", "--force", rev], dst)?;
    git_cmd(vec!["reset", "--hard", &format!("origin/{rev}")], dst)?;

    Ok(())
}

pub fn find_cmd_tool(kind: &str) -> Result<String, GitCliError> {
    let tool = config_get_string(&format!("{}.tool", kind))?;
    config_get_string(&format!("{}tool.{}.cmd", kind, tool))
}

fn config_get_string(value: &str) -> Result<String, GitCliError> {
    let output = process::Command::new("git")
        .args(["config", value])
        .output()?;
    let status = output.status;
    if status.success() {
        let result = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(result)
    } else {
        let msg = format!("git config {value}");
        Err(GitCliError::CommandError(msg, status))
    }
}

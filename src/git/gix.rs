use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use gix::bstr::BStr;
use gix::clone::checkout::main_worktree::ProgressId;
use gix::interrupt::IS_INTERRUPTED;
use gix::prelude::*;
use gix::progress::Discard;
use gix::remote::{ref_map, Direction};
use gix::{Progress, Repository};

use gix_ref::transaction::{LogChange, RefLog};
use tracing::{debug, error};

use super::GitError;

#[derive(Debug, thiserror::Error)]
pub enum GixError {
    #[error(transparent)]
    InvalidGitUrl(#[from] gix::url::parse::Error),

    #[error(transparent)]
    CloningError(#[from] gix::clone::Error),

    #[error(transparent)]
    CloningFetchError(#[from] gix::clone::fetch::Error),

    #[error(transparent)]
    CheckoutMainWorkTreeError(#[from] gix::clone::checkout::main_worktree::Error),

    #[error(transparent)]
    RepositoryMissing(#[from] gix::open::Error),

    #[error(transparent)]
    ConnectError(#[from] gix::remote::connect::Error),

    #[error(transparent)]
    FetchPrepareError(#[from] gix::remote::fetch::prepare::Error),

    #[error(transparent)]
    FetchError(#[from] gix::remote::fetch::Error),

    #[error("Oops: {0}")]
    Other(String),
}

#[tracing::instrument]
pub fn retrieve(dst: &Path, url: &str, rev: &str) -> Result<(), GitError> {
    if dst.exists() {
        debug!("Repository already exists, update it");
        fetch_and_reset(url, dst, rev)?;
    } else {
        debug!("Repository does not exists, create it");
        fs::create_dir_all(dst).map_err(|source| GitError::CreateFolder {
            path: dst.to_path_buf(),
            source,
        })?;
        clone(url, dst, rev)?;
    }

    Ok(())
}

#[tracing::instrument]
fn fetch_and_reset(url: &str, dest: &Path, rev: &str) -> Result<(), GixError> {
    let mut repo = gix::open(dest)?;
    let remote = repo.find_remote("origin").unwrap();

    let opts = ref_map::Options::default();

    let connection = remote.connect(Direction::Fetch, Discard)?;
    let prepare_fetch = connection
        .prepare_fetch(opts)?
        .with_write_packed_refs_only(true);
    let outcome = prepare_fetch.receive(&IS_INTERRUPTED)?;

    update_head(&mut repo, &outcome.ref_map.remote_refs, "plop".into())?;
    // dbg!(outcome);

    // let url = gix::url::parse(url.into())?;

    // let mut prepare_clone = gix::prepare_clone(url, dest)?;
    // let (repo, _) = prepare_clone.fetch_only(Discard, &IS_INTERRUPTED).unwrap();

    // // outcome.;
    // update_head(
    //     repo,
    //     &outcome.ref_map.remote_refs,
    //     reflog_message.as_ref(),
    //     remote_name.as_ref(),
    // )?;

    // let tree_id = find_remote_ref_id(&outcome.ref_map, rev).unwrap();
    // dbg!(tree_id);

    hack_head(dest, rev);

    checkout(&repo, Discard, &IS_INTERRUPTED)?;

    Ok(())
}

// HACK
fn hack_head(workdir: &Path, rev: &str) {
    // FIXME handle rev as commit id
    let target = format!("ref: refs/remotes/origin/{rev}");
    fs::write(workdir.join(".git").join("HEAD"), target).unwrap()
}

#[tracing::instrument]
fn clone(url: &str, dest: &Path, rev: &str) -> Result<(), GixError> {
    let url = gix::url::parse(url.into())?;

    let mut prepare_clone = gix::prepare_clone(url, dest)?;
    let (mut prepare_checkout, _outcome) =
        prepare_clone.fetch_then_checkout(Discard, &IS_INTERRUPTED)?;

    hack_head(dest, rev);

    let _ = prepare_checkout.main_worktree(Discard, &IS_INTERRUPTED)?;

    // hack_head(repo, rev);
    // let tree_id = find_remote_ref_id(&outcome.ref_map, rev).unwrap();
    // checkout(&repo, Discard, &IS_INTERRUPTED)?;

    Ok(())
}

// #[tracing::instrument]
// fn find_remote_ref_id<'a>(refs: &'a RefMap, rev: &str) -> Option<&'a ObjectId> {
//     let target_ref = format!("refs/heads/{rev}");
//     refs.remote_refs.iter().find_map(|r| match r {
//         Ref::Direct {
//             full_ref_name,
//             object,
//         } if full_ref_name == &target_ref => Some(object),
//         _ => None,
//     })
// }

#[tracing::instrument(skip(progress))]
fn checkout(
    repo: &Repository,
    mut progress: impl Progress,
    should_interrupt: &AtomicBool,
) -> Result<(), GixError> {
    let workdir = repo.work_dir().unwrap();

    let root_tree = match repo
        .head()
        .unwrap()
        .peel_to_id_in_place()
        .transpose()
        .unwrap()
    {
        Some(id) => {
            id.object()
                .expect("downloaded from remote")
                .peel_to_tree()
                .unwrap()
                .id
        }
        None => return Ok(()),
    };

    let index = gix_index::State::from_tree(&root_tree, |oid, buf| {
        repo.objects.find_tree_iter(oid, buf).ok()
    })
    .unwrap();
    let mut index = gix_index::File::from_state(index, repo.index_path());

    let opts = gix::worktree::index::checkout::Options::default();
    // let mut opts = repo.config.checkout_options(repo.git_dir())?;
    // opts.destination_is_initially_empty = true;

    let mut files = progress.add_child_with_id("checkout", ProgressId::CheckoutFiles.into());
    let mut bytes = progress.add_child_with_id("writing", ProgressId::BytesWritten.into());

    files.init(Some(index.entries().len()), gix::progress::count("files"));
    bytes.init(None, gix::progress::bytes());

    let start = std::time::Instant::now();
    let _outcome = gix::worktree::index::checkout(
        &mut index,
        workdir,
        {
            let objects = repo.objects.clone().into_arc().unwrap();
            move |oid, buf| objects.find_blob(oid, buf)
        },
        &mut files,
        &mut bytes,
        should_interrupt,
        opts,
    )
    .unwrap();
    files.show_throughput(start);
    bytes.show_throughput(start);

    index.write(Default::default()).unwrap();
    Ok(())
}

/// HEAD cannot be written by means of refspec by design, so we have to do it manually here. Also create the pointed-to ref
/// if we have to, as it might not have been naturally included in the ref-specs.
pub fn update_head(
    repo: &mut Repository,
    remote_refs: &[gix_protocol::handshake::Ref],
    reflog_message: &BStr,
) -> Result<(), GixError> {
    use gix_ref::{
        transaction::{PreviousValue, RefEdit},
        Target,
    };
    let (head_peeled_id, head_ref) = match remote_refs.iter().find_map(|r| {
        Some(match r {
            gix_protocol::handshake::Ref::Symbolic {
                full_ref_name,
                target,
                object,
            } if full_ref_name == "HEAD" => (Some(object.as_ref()), Some(target)),
            gix_protocol::handshake::Ref::Direct {
                full_ref_name,
                object,
            } if full_ref_name == "HEAD" => (Some(object.as_ref()), None),
            gix_protocol::handshake::Ref::Unborn {
                full_ref_name,
                target,
            } if full_ref_name == "HEAD" => (None, Some(target)),
            _ => return None,
        })
    }) {
        Some(t) => t,
        None => return Ok(()),
    };

    let head: gix_ref::FullName = "HEAD".try_into().expect("valid");
    let reflog_message = || LogChange {
        mode: RefLog::AndReference,
        force_create_reflog: false,
        message: reflog_message.to_owned(),
    };
    match head_ref {
        Some(referent) => {
            let referent: gix_ref::FullName = referent.try_into().map_err(|err| {
                let msg = format!(
                    "Error::InvalidHeadRef(head_ref_name: {}, source: {})",
                    referent.to_owned(),
                    err
                );
                GixError::Other(msg)
            })?;
            repo.refs
                .transaction()
                .packed_refs(
                    gix_ref::file::transaction::PackedRefs::DeletionsAndNonSymbolicUpdates(
                        Box::new(|oid, buf| {
                            repo.objects
                                .try_find(oid, buf)
                                .map(|obj| obj.map(|obj| obj.kind))
                                .map_err(|err| {
                                    Box::new(err)
                                        as Box<dyn std::error::Error + Send + Sync + 'static>
                                })
                        }),
                    ),
                )
                .prepare(
                    {
                        let mut edits = vec![RefEdit {
                            change: gix_ref::transaction::Change::Update {
                                log: reflog_message(),
                                expected: PreviousValue::Any,
                                new: Target::Symbolic(referent.clone()),
                            },
                            name: head.clone(),
                            deref: false,
                        }];
                        if let Some(head_peeled_id) = head_peeled_id {
                            edits.push(RefEdit {
                                change: gix_ref::transaction::Change::Update {
                                    log: reflog_message(),
                                    expected: PreviousValue::Any,
                                    new: Target::Peeled(head_peeled_id.to_owned()),
                                },
                                name: referent.clone(),
                                deref: false,
                            });
                        };
                        edits
                    },
                    gix_lock::acquire::Fail::Immediately,
                    gix_lock::acquire::Fail::Immediately,
                )
                .map_err(|err| {
                    let msg = format!("gix::reference::edit::Error::from({})", err);
                    GixError::Other(msg)
                })?
                .commit(repo.committer().transpose().map_err(|err| {
                    let msg = format!(
                        "Error::HeadUpdate(gix::reference::edit::Error::ParseCommitterTime({}))",
                        err
                    );
                    GixError::Other(msg)
                })?)
                .map_err(|err| {
                    let msg = format!("gix::reference::edit::Error::from({})", err);
                    GixError::Other(msg)
                })?;

            if let Some(head_peeled_id) = head_peeled_id {
                let mut log = reflog_message();
                log.mode = RefLog::Only;
                repo.edit_reference(RefEdit {
                    change: gix_ref::transaction::Change::Update {
                        log,
                        expected: PreviousValue::Any,
                        new: Target::Peeled(head_peeled_id.to_owned()),
                    },
                    name: head,
                    deref: false,
                })
                .map_err(|err| GixError::Other(err.to_string()))?;
            }
        }
        None => {
            repo.edit_reference(RefEdit {
                change: gix_ref::transaction::Change::Update {
                    log: reflog_message(),
                    expected: PreviousValue::Any,
                    new: Target::Peeled(
                        head_peeled_id
                            .expect("detached heads always point to something")
                            .to_owned(),
                    ),
                },
                name: head,
                deref: false,
            })
            .map_err(|err| GixError::Other(err.to_string()))?;
        }
    };
    Ok(())
}

pub fn find_cmd_tool(kind: &str) -> Result<String, GixError> {
    let tool = config_get_string(&format!("{}.tool", kind))?;
    config_get_string(&format!("{}tool.{}.cmd", kind, tool))
}

fn config_get_string(_value: &str) -> Result<String, GixError> {
    todo!()
    // let output = process::Command::new("git")
    //     .args(["config", value])
    //     .output()?;
    // let status = output.status;
    // if status.success() {
    //     let result = String::from_utf8_lossy(&output.stdout).trim().to_string();
    //     Ok(result)
    // } else {
    //     let msg = format!("git config {value}");
    //     Err(GitCliError::CommandError(msg, status))
    // }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn should_retrieve_remote_git_repository() {
        let _ = tracing_subscriber::fmt::try_init();

        let tmp = tempdir().unwrap();
        let mut dst = tmp.into_path();
        let url = "https://github.com/ilaborie/ffizer-templates.git";
        let rev = "wip";
        dst.push(rev);

        retrieve(&dst, url, rev).unwrap();

        let content = fs::read_dir(&dst).unwrap();

        let mut files = vec![];
        for entry in content {
            let entry = entry.unwrap();
            let file = entry.file_name().to_string_lossy().to_string();
            files.push(file);
        }
        assert!(
            files.contains(&String::from("wip")),
            "File wip not found in {:?}",
            files
        );
    }
}

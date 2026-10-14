// Copyright 2026 The fastjj Authors
// SPDX-License-Identifier: Apache-2.0
//
// In-process implementation of jj's Git transport interface. The internal type
// and method names are retained to keep the upstream call sites small; no
// process is created here, including for credentials, hooks, or SSH.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::OnceLock;

use thiserror::Error;
use zeroize::Zeroizing;

use crate::git::{
    FetchTagsOverride, GitPushStats, GitSubprocessOptions, NegativeRefSpec, Progress, RefSpec,
    RefToPush, RemoteCallbacks,
};
use crate::git_backend::GitBackend;
use crate::object_id::ObjectId as _;
use crate::ref_name::{RefNameBuf, RemoteName};

/// The embedding client supplies terminal or private IPC credential prompts.
type CredentialPrompt = fn(&str, bool) -> io::Result<String>;
static CREDENTIAL_PROMPT: OnceLock<CredentialPrompt> = OnceLock::new();

/// Registers the embedding application's in-process credential prompt.
pub fn register_credential_prompt(prompt: CredentialPrompt) {
    let _ = CREDENTIAL_PROMPT.set(prompt);
}

#[derive(Error, Debug)]
pub enum GitSubprocessError {
    #[error(transparent)]
    Native(#[from] git2::Error),
    #[error("Native Git transport: {0}")]
    Invalid(String),
}

pub(crate) struct GitSubprocessContext {
    git_dir: PathBuf,
}

impl GitSubprocessContext {
    pub(crate) fn from_git_backend(backend: &GitBackend, _options: GitSubprocessOptions) -> Self {
        Self {
            git_dir: backend.git_repo_path().to_owned(),
        }
    }

    fn open(&self) -> Result<git2::Repository, git2::Error> {
        git2::Repository::open_ext(
            &self.git_dir,
            git2::RepositoryOpenFlags::NO_SEARCH,
            &[] as &[&std::ffi::OsStr],
        )
    }

    pub(crate) fn spawn_fetch(
        &self,
        remote_name: &RemoteName,
        refspecs: &[RefSpec],
        negative_refspecs: &[NegativeRefSpec],
        callbacks: &mut RemoteCallbacks<'_>,
        depth: Option<NonZeroU32>,
        tags: Option<FetchTagsOverride>,
    ) -> Result<Option<String>, GitSubprocessError> {
        if refspecs.is_empty() {
            return Ok(None);
        }
        let repo = self.open()?;
        let mut remote = repo.find_remote(remote_name.as_str())?;
        let advertised: HashSet<String> = {
            let connection = remote.connect_auth(
                git2::Direction::Fetch,
                Some(native_callbacks(callbacks)),
                Some(proxy()),
            )?;
            connection
                .list()?
                .iter()
                .map(|head| head.name().to_owned())
                .collect()
        };
        let excluded = |name: &str| {
            negative_refspecs
                .iter()
                .any(|spec| map_ref(&spec.source, "*", name).is_some())
        };
        // Let jj retain its missing-exact-bookmark and pruning behavior.
        for spec in refspecs {
            if let Some(src) = &spec.source {
                if !src.contains('*') && !excluded(src) && !advertised.contains(src) {
                    return Ok(Some(src.clone()));
                }
            }
        }
        let mut selected = BTreeSet::new();
        for name in &advertised {
            if excluded(name) {
                continue;
            }
            for spec in refspecs {
                if let Some(src) = &spec.source {
                    if let Some(dst) = map_ref(src, &spec.destination, name) {
                        selected.insert(format!("+{name}:{dst}"));
                    }
                }
            }
        }
        // libgit2 does not accept negative refspecs. Expanding against the
        // advertisement implements exclusions without fetching or pruning them.
        // An empty refspec array would fetch the configured defaults: never do it.
        if matches!(tags, Some(FetchTagsOverride::AllTags)) {
            selected.insert("refs/tags/*:refs/tags/*".to_owned());
        }
        if !selected.is_empty() {
            let mut options = git2::FetchOptions::new();
            options.remote_callbacks(native_callbacks(callbacks));
            options.proxy_options(proxy());
            options.update_fetchhead(false).prune(git2::FetchPrune::Off);
            if let Some(depth) = depth {
                let depth = i32::try_from(depth.get()).map_err(|_| {
                    GitSubprocessError::Invalid("fetch depth exceeds supported range".into())
                })?;
                options.depth(depth);
            }
            options.download_tags(match tags {
                Some(FetchTagsOverride::AllTags) => git2::AutotagOption::All,
                Some(FetchTagsOverride::NoTags) => git2::AutotagOption::None,
                None => git2::AutotagOption::Unspecified,
            });
            remote.fetch(
                &selected.into_iter().collect::<Vec<_>>(),
                Some(&mut options),
                None,
            )?;
        }
        // Prune only destinations in the requested positive-minus-negative set.
        // Symbolic refs such as origin/HEAD are not remote-tracking bookmarks.
        for reference in repo.references()? {
            let mut reference = reference?;
            if reference.symbolic_target().is_some() {
                continue;
            }
            let Some(name) = reference.name() else {
                continue;
            };
            let stale = refspecs.iter().any(|spec| {
                spec.source
                    .as_ref()
                    .and_then(|src| map_ref(&spec.destination, src, name))
                    .is_some_and(|src| !excluded(&src) && !advertised.contains(&src))
            });
            if stale {
                reference.delete()?;
            }
        }
        Ok(None)
    }

    pub(crate) fn spawn_branch_prune(&self, branches: &[String]) -> Result<(), GitSubprocessError> {
        if branches.is_empty() {
            return Ok(());
        }
        let repo = self.open()?;
        for name in branches {
            match repo.find_reference(&format!("refs/remotes/{name}")) {
                Ok(mut reference) => reference.delete()?,
                Err(error) if error.code() == git2::ErrorCode::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    pub(crate) fn spawn_remote_show(
        &self,
        name: &RemoteName,
    ) -> Result<Option<RefNameBuf>, GitSubprocessError> {
        let repo = self.open()?;
        let mut remote = repo.find_remote(name.as_str())?;
        let mut callbacks = RemoteCallbacks::default();
        let connection = remote.connect_auth(
            git2::Direction::Fetch,
            Some(native_callbacks(&mut callbacks)),
            Some(proxy()),
        )?;
        match connection.default_branch() {
            Ok(branch) => Ok(branch
                .as_str()
                .and_then(|s| s.strip_prefix("refs/heads/"))
                .map(Into::into)),
            Err(error) if error.code() == git2::ErrorCode::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub(crate) fn spawn_push(
        &self,
        name: &RemoteName,
        references: &[RefToPush],
        callbacks: &mut RemoteCallbacks<'_>,
    ) -> Result<GitPushStats, GitSubprocessError> {
        if references.is_empty() {
            return Ok(GitPushStats::default());
        }
        let repo = self.open()?;
        let mut remote = repo.find_remote(name.as_str())?;
        let expected: HashMap<_, _> = references
            .iter()
            .map(|r| {
                let oid = r
                    .expected_location
                    .map(|id| git2::Oid::from_bytes(id.as_bytes()))
                    .transpose();
                oid.map(|oid| {
                    (
                        r.refspec.destination.as_str(),
                        oid.unwrap_or_else(git2::Oid::zero),
                    )
                })
            })
            .collect::<Result<_, _>>()?;
        let specs: Vec<_> = references
            .iter()
            .map(|r| r.refspec.to_git_format())
            .collect();
        let mut rejected = Vec::new();
        let mut pushed = Vec::new();
        let mut remote_rejected = Vec::new();
        let result = {
            let mut native = native_callbacks(callbacks);
            native.push_negotiation(|updates| {
                for update in updates {
                    let destination = update
                        .dst_refname()
                        .ok_or_else(|| git2::Error::from_str("non-UTF-8 pushed reference"))?;
                    let Some(expected) = expected.get(destination) else {
                        return Err(git2::Error::from_str("unexpected pushed reference"));
                    };
                    // Never replace a remote ref that moved after the last fetch.
                    // An already-applied update is an idempotent success.
                    if update.src() != *expected && update.src() != update.dst() {
                        rejected.push((
                            destination.into(),
                            Some("remote changed since last fetch".to_owned()),
                        ));
                    }
                }
                if rejected.is_empty() {
                    Ok(())
                } else {
                    Err(git2::Error::from_str("push lease rejected"))
                }
            });
            native.push_update_reference(|name, status| {
                if let Some(reason) = status {
                    remote_rejected.push((name.into(), Some(reason.to_owned())));
                } else {
                    pushed.push(name.into());
                }
                Ok(())
            });
            let mut options = git2::PushOptions::new();
            options.remote_callbacks(native).proxy_options(proxy());
            remote.push(&specs, Some(&mut options))
        };
        if rejected.is_empty() {
            result?;
        } else {
            // Negotiation aborts the entire push; report every unattempted ref.
            for reference in references {
                if !rejected
                    .iter()
                    .any(|(n, _)| n == &*reference.refspec.destination)
                {
                    remote_rejected.push((
                        reference.refspec.destination.as_str().into(),
                        Some("push aborted by another ref's failed lease".into()),
                    ));
                }
            }
        }
        Ok(GitPushStats {
            pushed,
            rejected,
            remote_rejected,
        })
    }
}

fn proxy<'a>() -> git2::ProxyOptions<'a> {
    let mut options = git2::ProxyOptions::new();
    options.auto();
    options
}

fn native_callbacks<'a>(callbacks: &'a mut RemoteCallbacks<'_>) -> git2::RemoteCallbacks<'a> {
    let mut native = git2::RemoteCallbacks::new();
    if let Some(progress) = &mut callbacks.progress {
        native.transfer_progress(move |stats| {
            let total = stats.total_objects() + stats.total_deltas();
            progress(&Progress {
                bytes_downloaded: Some(stats.received_bytes() as u64),
                overall: if total == 0 {
                    0.0
                } else {
                    (stats.indexed_objects() + stats.indexed_deltas()) as f32 / total as f32
                },
            });
            true
        });
    }
    if let Some(sideband) = &mut callbacks.sideband_progress {
        native.sideband_progress(move |data| {
            sideband(data);
            true
        });
    }
    let mut attempts = 0;
    native.credentials(move |url, username, allowed| {
        attempts += 1;
        if attempts > 3 {
            return Err(git2::Error::from_str("authentication attempts exhausted"));
        }
        let prompt = CREDENTIAL_PROMPT
            .get()
            .ok_or_else(|| git2::Error::from_str("no native credential prompt available"))?;
        let username = match username {
            Some(name) => name.to_owned(),
            None => prompt(&format!("Username for {url}"), false)
                .map_err(|_| git2::Error::from_str("username prompt cancelled"))?,
        };
        if allowed.contains(git2::CredentialType::USER_PASS_PLAINTEXT) {
            let password = Zeroizing::new(
                prompt(&format!("Password or token for {url}"), true)
                    .map_err(|_| git2::Error::from_str("password prompt cancelled"))?,
            );
            git2::Cred::userpass_plaintext(&username, &password)
        } else if allowed.contains(git2::CredentialType::USERNAME) {
            git2::Cred::username(&username)
        } else {
            Err(git2::Error::from_str("unsupported native credential type"))
        }
    });
    native
}

/// Git refspecs have at most one '*' which matches across path separators.
fn map_ref(pattern: &str, replacement: &str, name: &str) -> Option<String> {
    if let Some((prefix, suffix)) = pattern.split_once('*') {
        let middle = name.strip_prefix(prefix)?.strip_suffix(suffix)?;
        Some(replacement.replacen('*', middle, 1))
    } else {
        (pattern == name).then(|| replacement.to_owned())
    }
}

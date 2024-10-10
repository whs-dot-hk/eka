#[cfg(test)]
mod test;

use crate::id::CalculateRoot;

use bstr::BStr;
use gix::{
    discover::upwards::Options,
    sec::{trust::Mapping, Trust},
    Commit, ObjectId, ThreadSafeRepository,
};
use std::sync::OnceLock;
use thiserror::Error as ThisError;

#[derive(ThisError, Debug)]
pub enum Error {
    #[error("No ref named `{0}` found for remote `{1}`")]
    NoRef(String, String),
    #[error("Repository does not have a working directory")]
    NoWorkDir,
    #[error("Failed to calculate the repositories root commit")]
    RootNotFound,
    #[error(transparent)]
    WalkFailure(#[from] gix::revision::walk::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    NormalizationFailed(#[from] std::path::StripPrefixError),
    #[error(transparent)]
    NoRemote(#[from] Box<gix::remote::find::existing::Error>),
    #[error(transparent)]
    Connect(#[from] Box<gix::remote::connect::Error>),
    #[error(transparent)]
    Refs(#[from] Box<gix::remote::fetch::prepare::Error>),
    #[error(transparent)]
    Fetch(#[from] Box<gix::remote::fetch::Error>),
    #[error(transparent)]
    NoCommit(#[from] Box<gix::object::find::existing::with_conversion::Error>),
    #[error(transparent)]
    AddRefFailed(#[from] Box<gix::refspec::parse::Error>),
    #[error(transparent)]
    WriteRef(#[from] Box<gix::reference::edit::Error>),
}

/// Provide a lazyily instantiated static reference to the git repository.
static REPO: OnceLock<Option<ThreadSafeRepository>> = OnceLock::new();

use std::borrow::Cow;
static DEFAULT_REMOTE: OnceLock<Cow<str>> = OnceLock::new();

#[derive(Clone)]
pub struct Root(ObjectId);

pub fn repo() -> Result<Option<&'static ThreadSafeRepository>, Box<gix::discover::Error>> {
    let mut error = None;
    let repo = REPO.get_or_init(|| match get_repo() {
        Ok(repo) => Some(repo),
        Err(e) => {
            error = Some(e);
            None
        }
    });
    if let Some(e) = error {
        Err(e)
    } else {
        Ok(repo.as_ref())
    }
}

use std::io;
pub fn run_git_command(args: &[&str]) -> io::Result<Vec<u8>> {
    use std::process::Command;
    let output = Command::new("git").args(args).output()?;

    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            String::from_utf8_lossy(&output.stderr),
        ))
    }
}

fn get_repo() -> Result<ThreadSafeRepository, Box<gix::discover::Error>> {
    let opts = Options {
        required_trust: Trust::Full,
        ..Default::default()
    };
    ThreadSafeRepository::discover_opts(".", opts, Mapping::default()).map_err(Box::new)
}

pub fn default_remote() -> &'static str {
    use gix::remote::Direction;
    DEFAULT_REMOTE
        .get_or_init(|| {
            repo()
                .ok()
                .flatten()
                .and_then(|repo| {
                    repo.to_thread_local()
                        .remote_default_name(Direction::Push)
                        .map(|s| s.to_string().into())
                })
                .unwrap_or("origin".into())
        })
        .as_ref()
}

use std::ops::Deref;
impl Deref for Root {
    type Target = ObjectId;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'a> CalculateRoot<Root> for Commit<'a> {
    type Error = Error;
    fn calculate_root(&self) -> Result<Root, Self::Error> {
        use gix::traverse::commit::simple::{CommitTimeOrder, Sorting};
        // FIXME: we rely on a custom crate patch to search the commit graph
        // with a bias for older commits. The default gix behavior is the opposite
        // starting with bias for newer commits.
        //
        // it is based on the more general concept of an OldestFirst traversal
        // introduce by @nrdxp upstream: https://github.com/Byron/gitoxide/pull/1610
        //
        // However, that work tracks main and the goal of this patch is to remain
        // as minimal as possible on top of a release tag, for easier maintenance
        // assuming it may take a while to merge upstream.
        let mut walk = self
            .ancestors()
            .use_commit_graph(true)
            .sorting(Sorting::ByCommitTime(CommitTimeOrder::OldestFirst))
            .all()?;

        while let Some(Ok(info)) = walk.next() {
            if info.parent_ids.is_empty() {
                return Ok(Root(info.id));
            }
        }

        Err(Error::RootNotFound)
    }
}

use super::{NormalizeStorePath, QueryStore};
use gix::Repository;
use std::path::{Path, PathBuf};

impl NormalizeStorePath for Repository {
    type Error = Error;
    fn normalize<P: AsRef<Path>>(&self, path: P) -> Result<PathBuf, Error> {
        use path_clean::PathClean;
        use std::fs;
        let path = path.as_ref();

        let rel_repo_root = self.work_dir().ok_or(Error::NoWorkDir)?;
        let repo_root = fs::canonicalize(rel_repo_root)?;
        let current = self.current_dir();
        let rel = current.join(path).clean();

        rel.strip_prefix(&repo_root)
            .map_or_else(
                |e| {
                    // handle absolute paths as if they were relative to the repo root
                    if !path.is_absolute() {
                        return Err(e);
                    }
                    let cleaned = path.clean();
                    // Preserve the platform-specific root
                    let p = cleaned.strip_prefix(Path::new("/"))?;
                    repo_root
                        .join(p)
                        .clean()
                        .strip_prefix(&repo_root)
                        .map(Path::to_path_buf)
                },
                |p| Ok(p.to_path_buf()),
            )
            .map_err(|e| {
                tracing::warn!(
                    message = "Ignoring path outside repo root",
                    path = %path.display(),
                );
                Error::NormalizationFailed(e)
            })
    }
}

impl AsRef<[u8]> for Root {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

const V1_ROOT: &str = "refs/tags/ekala/root/v1";

use super::Init;
impl<'repo> Init<ObjectId> for gix::Remote<'repo> {
    type Error = Error;
    /// Determines if this remote is a valid Ekala store by pulling HEAD and [`V1_ROOT`]
    /// and ensuring the latter is actually the root of HEAD.
    fn is_ekala_store(&self) -> bool {
        use crate::id::CalculateRoot;

        let repo = self.repo();
        self.get_refs(["HEAD", V1_ROOT])
            .map(|i| {
                let mut i = i.into_iter();
                let fst = i
                    .next()
                    .and_then(|id| repo.find_commit(id).ok())
                    .and_then(|c| {
                        (c.parent_ids().count() != 0)
                            .then(|| c.calculate_root().ok().map(|r| *r))
                            .unwrap_or(Some(c.id))
                    });
                let snd = i
                    .next()
                    .and_then(|id| repo.find_commit(id).ok())
                    .and_then(|c| {
                        (c.parent_ids().count() != 0)
                            .then(|| c.calculate_root().ok().map(|r| *r))
                            .unwrap_or(Some(c.id))
                    });
                fst == snd
            })
            .unwrap_or(false)
    }
    /// Sync with the given remote and get the most up to date HEAD according to it.
    fn sync(&self) -> Result<ObjectId, Error> {
        self.get_ref("HEAD")
    }

    /// Initialize the repository by calculating the root, according to the latest HEAD.
    fn ekala_init(&self) -> Result<(), Error> {
        use gix::remote::Name;
        // fail early if the remote is not persistented to disk
        let name = self
            .name()
            .and_then(Name::as_symbol)
            .ok_or(Error::NoRemote(Box::new(
                gix::remote::find::existing::Error::NotFound {
                    name: "<unamed>".into(),
                },
            )))?;

        let head = self.sync()?;

        use crate::CalculateRoot;
        let repo = self.repo();
        let root = *repo.find_commit(head).map_err(Box::new)?.calculate_root()?;

        use gix::refs::transaction::PreviousValue;
        let root_ref = repo
            .reference(V1_ROOT, root, PreviousValue::MustNotExist, "init: root")
            .map_err(Box::new)?
            .name()
            .as_bstr()
            .to_string();

        // FIXME: use gix for push once it supports it
        run_git_command(&[
            "-C",
            repo.git_dir().to_string_lossy().as_ref(),
            "push",
            name,
            format!("{}:{}", root_ref, root_ref).as_str(),
        ])?;
        tracing::info!(remote = name, message = "Successfully initialized");
        Ok(())
    }
}

pub type ProgressRange = std::ops::RangeInclusive<prodash::progress::key::Level>;
pub const STANDARD_RANGE: ProgressRange = 2..=2;

pub fn setup_line_renderer(
    progress: &std::sync::Arc<prodash::tree::Root>,
) -> prodash::render::line::JoinHandle {
    prodash::render::line(
        std::io::stderr(),
        std::sync::Arc::downgrade(progress),
        prodash::render::line::Options {
            level_filter: Some(STANDARD_RANGE),
            initial_delay: Some(std::time::Duration::from_millis(500)),
            throughput: true,
            ..prodash::render::line::Options::default()
        }
        .auto_configure(prodash::render::line::StreamKind::Stderr),
    )
}

impl<'repo> super::QueryStore<ObjectId> for gix::Remote<'repo> {
    type Error = Error;
    /// returns the git object ids for the given references
    fn get_refs<Spec>(
        &self,
        references: impl IntoIterator<Item = Spec>,
    ) -> Result<impl IntoIterator<Item = gix::ObjectId>, Self::Error>
    where
        Spec: AsRef<BStr>,
    {
        use gix::remote::{fetch::Tags, Direction};
        use std::collections::HashSet;

        use gix::progress::tree::Root;

        let tree = Root::new();
        let sync_progress = tree.add_child("sync");
        let init_progress = tree.add_child("init");
        let handle = setup_line_renderer(&tree);

        let mut remote = self.clone().with_fetch_tags(Tags::None);

        remote
            .replace_refspecs(references, Direction::Fetch)
            .map_err(Box::new)?;

        let requested: HashSet<_> = remote
            .refspecs(Direction::Fetch)
            .iter()
            .filter_map(|r| r.to_ref().source().map(ToOwned::to_owned))
            .collect();

        use gix::remote::ref_map::Options;
        let client = remote.connect(Direction::Fetch).map_err(Box::new)?;
        let sync = client
            .prepare_fetch(sync_progress, Options::default())
            .map_err(Box::new)?;

        use std::sync::atomic::AtomicBool;
        let outcome = sync
            .receive(init_progress, &AtomicBool::new(false))
            .map_err(Box::new)?;

        handle.shutdown_and_wait();

        let refs = outcome.ref_map.remote_refs;

        refs.iter()
            .filter_map(|r| {
                let (name, target, peeled) = r.unpack();
                requested.get(name)?;
                Some(peeled.or(target).map(ToOwned::to_owned).ok_or_else(|| {
                    Error::NoRef(
                        name.to_string(),
                        remote
                            .name()
                            .and_then(|n| n.as_symbol())
                            .unwrap_or("<unamed>")
                            .into(),
                    )
                }))
            })
            .collect::<Result<HashSet<_>, _>>()
    }

    fn get_ref<Spec>(&self, target: Spec) -> Result<ObjectId, Self::Error>
    where
        Spec: AsRef<BStr>,
    {
        let name = target.as_ref().to_string();
        self.get_refs(Some(target)).and_then(|r| {
            r.into_iter().next().ok_or(Error::NoRef(
                name,
                self.name()
                    .and_then(|n| n.as_symbol())
                    .unwrap_or("<unamed>")
                    .into(),
            ))
        })
    }
}
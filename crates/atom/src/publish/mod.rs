pub mod error;
pub mod git;

use crate::{id::Id, AtomId};

use git::GitContent;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct Record<R> {
    id: AtomId<R>,
    content: Content,
}

#[derive(Default)]
pub struct Stats {
    pub published: u32,
    pub skipped: u32,
    pub failed: u32,
}

/// A Result is used over an Option here mainly so we can report which
/// atom was skipped, but it does not represent a true failure condition
type MaybeSkipped<T> = Result<T, Id>;

/// A Record that signifies whether an atom was published or safetly skipped.
type PublishOutcome<R> = MaybeSkipped<Record<R>>;

/// A HashMap containing all valid atoms in the current store.
type ValidAtoms = HashMap<Id, PathBuf>;

pub enum Content {
    Git(GitContent),
}

pub trait Builder<'a, R, S> {
    type Error;
    type Publisher: Publish<R>;

    /// Collect all the atoms in the worktree into a set.
    ///
    /// This function is called during construction to ensure that we
    /// never allow for an inconsistent state in the final atom store.
    ///
    /// The ensure this absolute, this function will panic if any two
    /// atoms in the same worktree have the same Id. This is to ensure
    /// that atoms have dinstinct identities in a given store.
    fn validate(publisher: &Self::Publisher) -> Result<ValidAtoms, Self::Error>;

    /// Collect all the atoms in the worktree into a set.
    ///
    /// This function should be called before `Publish::publish` to ensure that there are
    /// no duplicates.
    fn build(&self) -> Result<(ValidAtoms, Self::Publisher), Self::Error>;
}

pub trait Publish<R> {
    type Error;

    /// Publishes atoms.
    ///
    /// This function processes a collection of paths, each representing an atom to be published.
    /// Internally the implementation calls `Publish::publish_atom` for each path.
    ///
    /// # Error Handling
    /// - The function aims to process all provided paths, even if some fail.
    /// - Errors and skipped atoms are collected as results but do not halt the overall process.
    /// - The function continues until all the atoms have been processed.
    ///
    /// # Return Value
    /// Returns a vector of results types (`Vec<Result<PublishOutcome<T>, Self::Error>>`), where the
    /// outter result represents whether an atom has failed, and the inner result determines whether an
    /// atom was safely skipped, e.g. because it already exists..
    fn publish<C>(&self, paths: C) -> Vec<Result<PublishOutcome<R>, Self::Error>>
    where
        C: IntoIterator<Item = PathBuf>;

    /// Publish an atom.
    ///
    /// This function takes a single path and publishes the atom located there, if possible.
    ///
    /// # Return Value
    /// - An outcome (`PublishOutcome<T>`) is either the record (`Record<R>`) of the successfully
    ///   publish atom or the atom id (`Id`) if it was safely skipped.
    /// - The function will return an error (`Self::Error`) if the atom could not be published for
    ///   any reason, e.g. invalid manifests.
    fn publish_atom<P: AsRef<Path>>(&self, path: P) -> Result<PublishOutcome<R>, Self::Error>;
}

impl<R> Record<R> {
    pub fn id(&self) -> &AtomId<R> {
        &self.id
    }
    pub fn content(&self) -> &Content {
        &self.content
    }
}

/// The current version of the atom ref format
const ATOM_FORMAT_VERSION: &str = "v1";
const EMPTY: &str = "";
/// the namespace under refs to publish atoms
const ATOM_REF_TOP_LEVEL: &str = "atoms";
const ATOM_TIP_REF: &str = "tip";
const ATOM_SPEC_REF: &str = "spec";
const ATOM_SRC_REF: &str = "src";
pub const ATOM_EXT: &str = "atom";

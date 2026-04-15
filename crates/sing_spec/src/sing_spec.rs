mod client;
mod file_system;
mod index;
mod store;
mod types;

pub use client::{DefaultSingSpecClientFactory, SingSpecClient, SingSpecClientFactory};
pub use file_system::{SpecFileSystem, SshSpecFileSystem};
pub use store::RemoteSpecStore;
pub use types::{
    OptionalValue, SpecBoardState, SpecDocumentState, SpecEntry, SpecMetadataPatch,
    SpecMutationResult, SpecOpenTarget,
};

pub use sing_bridge::{CreateSpecRequest, SpecStatus};

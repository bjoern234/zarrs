//! Zarr storage ([stores](store) and [storage transformers](storage_transformer)).
//!
//! See <https://zarr-specs.readthedocs.io/en/latest/v3/core/v3.0.html#storage>.
//!
//! A Zarr [store] is a system that can be used to store and retrieve data from a Zarr hierarchy.
//! For example: a filesystem, HTTP server, FTP server, Amazon S3 bucket, ZIP file, etc.
//!
//! A Zarr [storage transformer](storage_transformer) modifies a request to read or write data before passing that request to a following storage transformer or store.
//! A [`StorageTransformerChain`] represents a sequence of storage transformers.
//! A storage transformer chain and individual storage transformers all have the same interface as a [store].
//!
//! This module defines abstract store interfaces, includes various store and storage transformers, and has functions for performing the store operations defined at <https://zarr-specs.readthedocs.io/en/latest/v3/core/v3.0.html#operations>.

pub mod storage_adapter;
mod storage_handle;
pub mod storage_transformer;
mod storage_value_io;
pub mod store;
mod store_key;
mod store_prefix;

use std::{path::PathBuf, sync::Arc};

use itertools::Itertools;
use thiserror::Error;

use crate::{
    array::{ArrayMetadata, ChunkKeyEncoding, MaybeBytes},
    byte_range::{ByteOffset, ByteRange, InvalidByteRangeError},
    group::{GroupMetadata, GroupMetadataV3},
    node::{Node, NodeMetadata, NodeNameError, NodePath, NodePathError},
};

pub use store_key::{StoreKey, StoreKeyError, StoreKeys};
pub use store_prefix::{StorePrefix, StorePrefixError, StorePrefixes};

pub use self::storage_transformer::StorageTransformerChain;

pub use self::storage_handle::StorageHandle;

pub use storage_value_io::StorageValueIO;

/// [`Arc`] wrapped readable storage.
pub type ReadableStorage<'a> = Arc<dyn ReadableStorageTraits + 'a>;

/// [`Arc`] wrapped writable storage.
pub type WritableStorage<'a> = Arc<dyn WritableStorageTraits + 'a>;

/// [`Arc`] wrapped listable storage.
pub type ListableStorage<'a> = Arc<dyn ListableStorageTraits + 'a>;

/// [`Arc`] wrapped readable and writable storage.
pub type ReadableWritableStorage<'a> = Arc<dyn ReadableWritableStorageTraits + 'a>;

/// [`Arc`] wrapped readable and listable storage.
pub type ReadableListableStorage<'a> = Arc<dyn ReadableListableStorageTraits + 'a>;

/// Readable storage traits.
pub trait ReadableStorageTraits: Send + Sync {
    /// Retrieve the value (bytes) associated with a given [`StoreKey`].
    ///
    /// Returns [`None`] if the key is not found.
    ///
    /// # Errors
    ///
    /// Returns a [`StorageError`] if the store key does not exist or there is an error with the underlying store.
    fn get(&self, key: &StoreKey) -> Result<MaybeBytes, StorageError>;

    /// Retrieve partial bytes from a list of byte ranges for a store key.
    ///
    /// Returns [`None`] if the key is not found.
    ///
    /// # Errors
    ///
    /// Returns a [`StorageError`] if there is an underlying storage error.
    fn get_partial_values_key(
        &self,
        key: &StoreKey,
        byte_ranges: &[ByteRange],
    ) -> Result<Option<Vec<Vec<u8>>>, StorageError>;

    /// Retrieve partial bytes from a list of [`StoreKeyRange`].
    ///
    /// # Arguments
    /// * `key_ranges`: ordered set of ([`StoreKey`], [`ByteRange`]) pairs. A key may occur multiple times with different ranges.
    ///
    /// # Output
    ///
    /// A a list of values in the order of the `key_ranges`. It will be [`None`] for missing keys.
    ///
    /// # Errors
    ///
    /// Returns a [`StorageError`] if there is an underlying storage error.
    fn get_partial_values(
        &self,
        key_ranges: &[StoreKeyRange],
    ) -> Result<Vec<MaybeBytes>, StorageError>;

    /// Return the size in bytes of all keys under `prefix`.
    ///
    /// # Errors
    ///
    /// Returns a `StorageError` if the store does not support size() or there is an underlying error with the store.
    fn size_prefix(&self, prefix: &StorePrefix) -> Result<u64, StorageError>;

    /// Return the size in bytes of the value at `key`.
    ///
    /// Returns [`None`] if the key is not found.
    ///
    /// # Errors
    ///
    /// Returns a [`StorageError`] if there is an underlying storage error.
    fn size_key(&self, key: &StoreKey) -> Result<Option<u64>, StorageError>;

    /// Return the total size in bytes of the storage.
    ///
    /// # Errors
    ///
    /// Returns a `StorageError` if the store does not support size() or there is an underlying error with the store.
    fn size(&self) -> Result<u64, StorageError> {
        self.size_prefix(&StorePrefix::root())
    }

    /// A utility method with the same input and output as [`get_partial_values`](ReadableStorageTraits::get_partial_values) that internally calls [`get_partial_values_key`](ReadableStorageTraits::get_partial_values_key) with byte ranges grouped by key.
    ///
    /// Readable storage can use this function in the implementation of [`get_partial_values`](ReadableStorageTraits::get_partial_values) if that is optimal.
    ///
    /// # Errors
    ///
    /// Returns a [`StorageError`] if there is an underlying storage error.
    fn get_partial_values_batched_by_key(
        &self,
        key_ranges: &[StoreKeyRange],
    ) -> Result<Vec<MaybeBytes>, StorageError> {
        let mut out: Vec<MaybeBytes> = Vec::with_capacity(key_ranges.len());
        let mut last_key = None;
        let mut byte_ranges_key = Vec::new();
        for key_range in key_ranges {
            if last_key.is_none() {
                last_key = Some(&key_range.key);
            }
            let last_key_val = last_key.unwrap();

            if key_range.key != *last_key_val {
                // Found a new key, so do a batched get of the byte ranges of the last key
                let bytes = (self.get_partial_values_key(last_key.unwrap(), &byte_ranges_key)?)
                    .map_or_else(
                        || vec![None; byte_ranges_key.len()],
                        |partial_values| partial_values.into_iter().map(Some).collect(),
                    );
                out.extend(bytes);
                last_key = Some(&key_range.key);
                byte_ranges_key.clear();
            }

            byte_ranges_key.push(key_range.byte_range);
        }

        if !byte_ranges_key.is_empty() {
            // Get the byte ranges of the last key
            let bytes = (self.get_partial_values_key(last_key.unwrap(), &byte_ranges_key)?)
                .map_or_else(
                    || vec![None; byte_ranges_key.len()],
                    |partial_values| partial_values.into_iter().map(Some).collect(),
                );
            out.extend(bytes);
        }

        Ok(out)
    }
}

/// Listable storage traits.
pub trait ListableStorageTraits: Send + Sync {
    /// Retrieve all [`StoreKeys`] in the store.
    ///
    /// # Errors
    ///
    /// Returns a [`StorageError`] if there is an underlying error with the store.
    fn list(&self) -> Result<StoreKeys, StorageError>;

    /// Retrieve all [`StoreKeys`] with a given [`StorePrefix`].
    ///
    /// # Errors
    ///
    /// Returns a [`StorageError`] if the prefix is not a directory or there is an underlying error with the store.
    fn list_prefix(&self, prefix: &StorePrefix) -> Result<StoreKeys, StorageError>;

    /// Retrieve all [`StoreKeys`] and [`StorePrefix`] which are direct children of [`StorePrefix`].
    ///
    /// # Errors
    ///
    /// Returns a [`StorageError`] if the prefix is not a directory or there is an underlying error with the store.
    ///
    fn list_dir(&self, prefix: &StorePrefix) -> Result<StoreKeysPrefixes, StorageError>;
}

/// Writable storage traits.
pub trait WritableStorageTraits: Send + Sync + ReadableStorageTraits {
    /// Store bytes at a [`StoreKey`].
    ///
    /// # Errors
    ///
    /// Returns a [`StorageError`] on failure to store.
    fn set(&self, key: &StoreKey, value: &[u8]) -> Result<(), StorageError>;

    /// Store bytes according to a list of [`StoreKeyStartValue`].
    ///
    /// # Errors
    ///
    /// Returns a [`StorageError`] on failure to store.
    fn set_partial_values(
        &self,
        key_start_values: &[StoreKeyStartValue],
    ) -> Result<(), StorageError> {
        // Group by store key
        for (key, group) in &key_start_values
            .iter()
            .group_by(|key_start_value| &key_start_value.key)
        {
            // Read the store key
            let mut bytes = self.get(key)?.unwrap_or_default();

            // Update the store key
            for key_start_value in group {
                let start: usize = key_start_value.start.try_into().unwrap();
                let end: usize = key_start_value.end().try_into().unwrap();
                if bytes.len() < end {
                    bytes.resize(end, 0);
                }
                bytes[start..end].copy_from_slice(key_start_value.value);
            }

            // Write the store key
            self.set(key, &bytes)?;
        }
        Ok(())
    }

    /// Erase a [`StoreKey`].
    ///
    /// Returns true if the key exists and was erased, or false if the key does not exist.
    ///
    /// # Errors
    ///
    /// Returns a [`StorageError`] if there is an underlying storage error.
    fn erase(&self, key: &StoreKey) -> Result<bool, StorageError>;

    /// Erase a list of [`StoreKey`].
    ///
    /// Returns true if all keys existed and were erased, or false if any key does not exist.
    ///
    /// # Errors
    ///
    /// Returns a [`StorageError`] if there is an underlying storage error.
    fn erase_values(&self, keys: &[StoreKey]) -> Result<bool, StorageError> {
        let mut all_deleted = true;
        for key in keys {
            all_deleted = all_deleted && self.erase(key)?;
        }
        Ok(all_deleted)
    }

    /// Erase all [`StoreKey`] under [`StorePrefix`].
    ///
    /// Returns true if the prefix and all its children were removed.
    ///
    /// # Errors
    /// Returns a [`StorageError`] is the prefix is not in the store, or the erase otherwise fails.
    fn erase_prefix(&self, prefix: &StorePrefix) -> Result<bool, StorageError>;
}

/// A supertrait of [`ReadableStorageTraits`] and [`WritableStorageTraits`].
pub trait ReadableWritableStorageTraits: ReadableStorageTraits + WritableStorageTraits {}

impl<T> ReadableWritableStorageTraits for T where T: ReadableStorageTraits + WritableStorageTraits {}

/// A supertrait of [`ReadableStorageTraits`] and [`ListableStorageTraits`].
pub trait ReadableListableStorageTraits: ReadableStorageTraits + ListableStorageTraits {}

impl<T> ReadableListableStorageTraits for T where T: ReadableStorageTraits + ListableStorageTraits {}

/// A [`StoreKey`] and [`ByteRange`].
#[derive(Debug)]
pub struct StoreKeyRange {
    /// The key for the range.
    key: StoreKey,
    /// The byte range.
    byte_range: ByteRange,
}

impl StoreKeyRange {
    /// Create a new [`StoreKeyRange`].
    #[must_use]
    pub const fn new(key: StoreKey, byte_range: ByteRange) -> Self {
        Self { key, byte_range }
    }
}

/// A [`StoreKey`], [`ByteOffset`], and value (bytes).
#[derive(Debug)]
#[must_use]
pub struct StoreKeyStartValue<'a> {
    /// The key.
    key: StoreKey,
    /// The starting byte offset.
    start: ByteOffset,
    /// The store value.
    value: &'a [u8],
}

impl StoreKeyStartValue<'_> {
    /// Create a new [`StoreKeyStartValue`].
    pub const fn new(key: StoreKey, start: ByteOffset, value: &[u8]) -> StoreKeyStartValue {
        StoreKeyStartValue { key, start, value }
    }

    /// Get the offset of exclusive end of the [`StoreKeyStartValue`].
    #[must_use]
    pub const fn end(&self) -> ByteOffset {
        self.start + self.value.len() as u64
    }
}

/// [`StoreKeys`] and [`StorePrefixes`].
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
#[allow(dead_code)]
pub struct StoreKeysPrefixes {
    keys: StoreKeys,
    prefixes: StorePrefixes,
}

impl StoreKeysPrefixes {
    /// Returns the keys.
    #[must_use]
    pub const fn keys(&self) -> &StoreKeys {
        &self.keys
    }

    /// Returns the prefixes.
    #[must_use]
    pub const fn prefixes(&self) -> &StorePrefixes {
        &self.prefixes
    }
}

/// A storage error.
#[derive(Debug, Error)]
pub enum StorageError {
    /// A write operation was attempted on a read only store.
    #[error("a write operation was attempted on a read only store")]
    ReadOnly,
    /// An IO error.
    #[error(transparent)]
    IOError(#[from] std::io::Error),
    /// An error serializing or deserializing JSON.
    #[error(transparent)]
    InvalidJSON(#[from] serde_json::Error),
    /// An invalid store prefix.
    #[error("invalid store prefix {0}")]
    StorePrefixError(#[from] StorePrefixError),
    /// An invalid store key.
    #[error("invalid store key {0}")]
    InvalidStoreKey(#[from] StoreKeyError),
    /// An invalid node path.
    #[error("invalid node path {0}")]
    NodePathError(#[from] NodePathError),
    /// An invalid node name.
    #[error("invalid node name {0}")]
    NodeNameError(#[from] NodeNameError),
    /// An invalid byte range.
    #[error("invalid byte range {0}")]
    InvalidByteRangeError(#[from] InvalidByteRangeError),
    /// The requested method is not supported.
    #[error("{0}")]
    Unsupported(String),
    /// Unknown key size where the key size must be known.
    #[error("{0}")]
    UnknownKeySize(StoreKey),
    /// Any other error.
    #[error("{0}")]
    Other(String),
}

impl From<&str> for StorageError {
    fn from(err: &str) -> Self {
        Self::Other(err.to_string())
    }
}

impl From<String> for StorageError {
    fn from(err: String) -> Self {
        Self::Other(err)
    }
}

/// Return the metadata key given a node path.
#[must_use]
pub fn meta_key(path: &NodePath) -> StoreKey {
    let path = path.as_str();
    if path.eq("/") {
        unsafe { StoreKey::new_unchecked("zarr.json".to_string()) }
    } else {
        let path = path.strip_prefix('/').unwrap_or(path);
        unsafe { StoreKey::new_unchecked(path.to_string() + "/zarr.json") }
    }
}

/// Return the data key given a node path, chunk grid coordinates, and a chunk key encoding.
#[must_use]
pub fn data_key(
    path: &NodePath,
    chunk_grid_indices: &[u64],
    chunk_key_encoding: &ChunkKeyEncoding,
) -> StoreKey {
    let path = path.as_str();
    let path = path.strip_prefix('/').unwrap_or(path);
    let mut key_path = PathBuf::from(path);
    key_path.push(chunk_key_encoding.encode(chunk_grid_indices).as_str());
    unsafe { StoreKey::new_unchecked(key_path.to_string_lossy().to_string()) }
}

/// Get the child nodes.
///
/// # Errors
///
/// Returns a [`StorageError`] if there is an underlying error with the store.
pub fn get_child_nodes<TStorage: ?Sized + ReadableStorageTraits + ListableStorageTraits>(
    storage: &TStorage,
    path: &NodePath,
) -> Result<Vec<Node>, StorageError> {
    let prefixes = discover_children(storage, path)?;
    let mut nodes: Vec<Node> = Vec::new();
    for prefix in &prefixes {
        let child_metadata = match storage.get(&meta_key(&prefix.try_into()?))? {
            Some(child_metadata) => {
                let metadata: NodeMetadata = serde_json::from_slice(child_metadata.as_slice())?;
                metadata
            }
            None => NodeMetadata::Group(GroupMetadataV3::default().into()),
        };
        let path: NodePath = prefix.try_into()?;
        let children = match child_metadata {
            NodeMetadata::Array(_) => Vec::default(),
            NodeMetadata::Group(_) => get_child_nodes(storage, &path)?,
        };
        nodes.push(Node::new(path, child_metadata, children));
    }
    Ok(nodes)
}

// /// Create a new [`Hierarchy`].
// ///
// /// # Errors
// ///
// /// Returns a [`StorageError`] if there is an underlying error with the store.
// pub fn create_hierarchy<TStorage: ?Sized + ReadableStorageTraits + ListableStorageTraits>(
//     storage: &TStorage,
// ) -> Result<Hierarchy, StorageError> {
//     let root_path: NodePath = NodePath::new("/")?;
//     let root_metadata = storage.get(&meta_key(&root_path));
//     let root_metadata: NodeMetadata = match root_metadata {
//         Ok(root_metadata) => serde_json::from_slice(root_metadata.as_slice())?,
//         Err(..) => NodeMetadata::Group(GroupMetadata::default()), // root metadata does not exist, assume implicit group
//     };

//     let children = get_child_nodes(storage, &root_path)?;
//     let root_node = Node {
//         name: NodeName::root(),
//         path: root_path,
//         children,
//         metadata: root_metadata,
//     };
//     Ok(Hierarchy { root: root_node })
// }

/// Create a group.
///
/// # Errors
///
/// Returns a [`StorageError`] if there is an underlying error with the store.
pub fn create_group(
    storage: &dyn WritableStorageTraits,
    path: &NodePath,
    group: &GroupMetadata,
) -> Result<(), StorageError> {
    let json = serde_json::to_vec_pretty(group)?;
    storage.set(&meta_key(path), &json)?;
    Ok(())
}

/// Create an array.
///
/// # Errors
///
/// Returns a [`StorageError`] if there is an underlying error with the store.
pub fn create_array(
    storage: &dyn WritableStorageTraits,
    path: &NodePath,
    array: &ArrayMetadata,
) -> Result<(), StorageError> {
    let json = serde_json::to_vec_pretty(array)?;
    storage.set(&meta_key(path), &json)?;
    Ok(())
}

/// Store a chunk.
///
/// # Errors
///
/// Returns a [`StorageError`] if there is an underlying error with the store.
pub fn store_chunk(
    storage: &dyn WritableStorageTraits,
    array_path: &NodePath,
    chunk_grid_indices: &[u64],
    chunk_key_encoding: &ChunkKeyEncoding,
    chunk_serialised: &[u8],
) -> Result<(), StorageError> {
    storage.set(
        &data_key(array_path, chunk_grid_indices, chunk_key_encoding),
        chunk_serialised,
    )?;
    Ok(())
}

/// Retrieve a chunk.
///
/// # Errors
///
/// Returns a [`StorageError`] if there is an underlying error with the store.
pub fn retrieve_chunk(
    storage: &dyn ReadableStorageTraits,
    array_path: &NodePath,
    chunk_grid_indices: &[u64],
    chunk_key_encoding: &ChunkKeyEncoding,
) -> Result<MaybeBytes, StorageError> {
    storage.get(&data_key(
        array_path,
        chunk_grid_indices,
        chunk_key_encoding,
    ))
}

/// Erase a chunk.
///
/// # Errors
///
/// Returns a [`StorageError`] if there is an underlying error with the store.
pub fn erase_chunk(
    storage: &dyn WritableStorageTraits,
    array_path: &NodePath,
    chunk_grid_indices: &[u64],
    chunk_key_encoding: &ChunkKeyEncoding,
) -> Result<bool, StorageError> {
    storage.erase(&data_key(
        array_path,
        chunk_grid_indices,
        chunk_key_encoding,
    ))
}

/// Retrieve byte ranges from a chunk.
///
/// Returns [`None`] where keys are not found.
///
/// # Errors
///
/// Returns a [`StorageError`] if there is an underlying error with the store.
pub fn retrieve_partial_values(
    storage: &dyn ReadableStorageTraits,
    array_path: &NodePath,
    chunk_grid_indices: &[u64],
    chunk_key_encoding: &ChunkKeyEncoding,
    bytes_ranges: &[ByteRange],
) -> Result<Vec<MaybeBytes>, StorageError> {
    let key = data_key(array_path, chunk_grid_indices, chunk_key_encoding);
    let key_ranges: Vec<StoreKeyRange> = bytes_ranges
        .iter()
        .map(|byte_range| StoreKeyRange::new(key.clone(), *byte_range))
        .collect();
    storage.get_partial_values(&key_ranges)
}

/// Discover the children of a node.
///
/// # Errors
///
/// Returns a [`StorageError`] if there is an underlying error with the store.
pub fn discover_children<TStorage: ?Sized + ReadableStorageTraits + ListableStorageTraits>(
    storage: &TStorage,
    path: &NodePath,
) -> Result<StorePrefixes, StorageError> {
    let prefix: StorePrefix = path.try_into()?;
    let children: Result<Vec<_>, _> = storage
        .list_dir(&prefix)?
        .prefixes()
        .iter()
        .filter(|v| !v.as_str().starts_with("__"))
        .map(|v| StorePrefix::new(v.as_str()))
        .collect();
    Ok(children?)
}

/// Discover all nodes.
///
/// # Errors
///
/// Returns a [`StorageError`] if there is an underlying error with the store.
///
pub fn discover_nodes(storage: &dyn ListableStorageTraits) -> Result<StoreKeys, StorageError> {
    storage.list_prefix(&"/".try_into()?)
}

/// Erase a node (group or array) and all of its children.
///
/// Returns true if the node existed and was removed.
///
/// # Errors
///
/// Returns a [`StorageError`] if there is an underlying error with the store.
pub fn erase_node(
    storage: &dyn WritableStorageTraits,
    path: &NodePath,
) -> Result<bool, StorageError> {
    let prefix = path.try_into()?;
    storage.erase_prefix(&prefix)
}

/// Check if a node exists.
///
/// # Errors
///
/// Returns a [`StorageError`] if there is an underlying error with the store.
pub fn node_exists<TStorage: ?Sized + ReadableStorageTraits + ListableStorageTraits>(
    storage: &TStorage,
    path: &NodePath,
) -> Result<bool, StorageError> {
    Ok(storage
        .get(&meta_key(path))
        .map_or(storage.list_dir(&path.try_into()?).is_ok(), |_| true))
}

/// Check if a node exists.
///
/// # Errors
///
/// Returns a [`StorageError`] if there is an underlying error with the store.
pub fn node_exists_listable<TStorage: ?Sized + ListableStorageTraits>(
    storage: &TStorage,
    path: &NodePath,
) -> Result<bool, StorageError> {
    let prefix: StorePrefix = path.try_into()?;
    prefix.parent().map_or_else(
        || Ok(false),
        |parent| {
            storage.list_dir(&parent).map(|keys_prefixes| {
                !keys_prefixes.keys().is_empty() || !keys_prefixes.prefixes().is_empty()
            })
        },
    )
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use self::store::MemoryStore;

    use super::*;

    #[test]
    fn transformers_multithreaded() {
        use rayon::prelude::*;

        let store = Arc::new(MemoryStore::default());

        let log_writer = Arc::new(std::sync::Mutex::new(std::io::BufWriter::new(
            std::io::stdout(),
        )));

        // let storage_transformer_usage_log = Arc::new(self::storage_transformer::UsageLogStorageTransformer::new(
        //     || "mt_log: ".to_string(),
        //     log_writer.clone(),
        // ));
        let storage_transformer_performance_metrics =
            Arc::new(self::storage_transformer::PerformanceMetricsStorageTransformer::new());
        let storage_transformer_chain = StorageTransformerChain::new(vec![
            // storage_transformer_usage_log.clone(),
            storage_transformer_performance_metrics.clone(),
        ]);
        let transformer =
            storage_transformer_chain.create_readable_writable_transformer(store.clone());
        let transformer_listable = storage_transformer_chain.create_listable_transformer(store);

        (0..10).into_par_iter().for_each(|_| {
            transformer_listable.list().unwrap();
        });

        (0..10).into_par_iter().for_each(|i| {
            transformer
                .set(&StoreKey::new(&i.to_string()).unwrap(), &[i; 5])
                .unwrap();
        });

        for i in 0..10 {
            let _ = transformer.get(&StoreKey::new(&i.to_string()).unwrap());
        }

        log_writer.lock().unwrap().flush().unwrap();

        println!(
            "stats\n\t{}\n\t{}\n\t{}\n\t{}",
            storage_transformer_performance_metrics.bytes_written(),
            storage_transformer_performance_metrics.bytes_read(),
            storage_transformer_performance_metrics.writes(),
            storage_transformer_performance_metrics.reads()
        );
    }
}

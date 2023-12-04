use itertools::Itertools;

use crate::{array::{MaybeBytes, ArrayMetadata, ChunkKeyEncoding}, byte_range::ByteRange, node::{NodePath, Node, NodeMetadata}, group::{GroupMetadataV3, GroupMetadata}};

use super::{
    StorageError, StoreKey, StoreKeyRange, StoreKeyStartValue, StoreKeys, StoreKeysPrefixes,
    StorePrefix, meta_key, data_key, StorePrefixes,
};

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

/// Get the child nodes.
///
/// # Errors
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


/// Create a group.
///
/// # Errors
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

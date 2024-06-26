//! Zarr group metadata.
//!
//! See <https://zarr-specs.readthedocs.io/en/latest/v3/core/v3.0.html#group-metadata>.

use derive_more::{Display, From};
use serde::{Deserialize, Serialize};

use crate::metadata::AdditionalFields;

/// A wrapper to handle various versions of Zarr group metadata.
#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Debug, Display, From)]
#[serde(untagged)]
pub enum GroupMetadata {
    /// Version 3.0.
    V3(GroupMetadataV3),
}

impl TryFrom<&str> for GroupMetadata {
    type Error = serde_json::Error;
    fn try_from(metadata_json: &str) -> Result<Self, Self::Error> {
        serde_json::from_str::<Self>(metadata_json)
    }
}

/// Zarr group metadata (storage specification v3).
///
/// See <https://zarr-specs.readthedocs.io/en/latest/v3/core/v3.0.html#group-metadata>.
///
/// An example `JSON` document for an explicit v3 group:
/// ```json
/// {
///     "zarr_format": 3,
///     "node_type": "group",
///     "attributes": {
///         "spam": "ham",
///         "eggs": 42,
///     }
/// }
#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Debug, Display)]
#[serde(tag = "node_type", rename = "group")]
#[display(fmt = "{}", "serde_json::to_string(self).unwrap_or_default()")]
pub struct GroupMetadataV3 {
    /// An integer defining the version of the storage specification to which the array store adheres. Must be `3`.
    pub zarr_format: usize,
    /// A string defining the type of hierarchy node element, must be `group` here.
    #[serde(skip_serializing)]
    #[allow(dead_code)]
    pub node_type: String, // Ideally this is serialized after zarr format, and tag serialization is skipped
    /// Optional user metadata.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub attributes: serde_json::Map<String, serde_json::Value>,
    /// Additional fields.
    #[serde(flatten)]
    pub additional_fields: AdditionalFields,
}

impl Default for GroupMetadataV3 {
    fn default() -> Self {
        Self::new(serde_json::Map::new(), AdditionalFields::default())
    }
}

impl GroupMetadataV3 {
    /// Create group metadata.
    #[must_use]
    pub fn new(
        attributes: serde_json::Map<String, serde_json::Value>,
        additional_fields: AdditionalFields,
    ) -> Self {
        Self {
            zarr_format: 3,
            node_type: "group".to_string(),
            attributes,
            additional_fields,
        }
    }

    /// Validates that the `zarr_format` field is `3`.
    #[must_use]
    pub const fn validate_format(&self) -> bool {
        self.zarr_format == 3
    }

    /// Validates that the `node_type` is `"group"`.
    #[must_use]
    pub fn validate_node_type(&self) -> bool {
        self.node_type == "group"
    }
}

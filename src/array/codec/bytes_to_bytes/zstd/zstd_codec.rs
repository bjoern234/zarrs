use zstd::zstd_safe;

use crate::{
    array::{
        codec::{
            BytesPartialDecoderTraits, BytesToBytesCodecTraits, Codec, CodecError, CodecPlugin,
            CodecTraits,
        },
        BytesRepresentation,
    },
    metadata::Metadata,
    plugin::PluginCreateError,
};

#[cfg(feature = "async")]
use crate::array::codec::AsyncBytesPartialDecoderTraits;

use super::{zstd_partial_decoder, ZstdCodecConfiguration, ZstdCodecConfigurationV1};

const IDENTIFIER: &str = "zstd";

// Register the codec.
inventory::submit! {
    CodecPlugin::new(IDENTIFIER, is_name_zstd, create_codec_zstd)
}

fn is_name_zstd(name: &str) -> bool {
    name.eq(IDENTIFIER)
}

fn create_codec_zstd(metadata: &Metadata) -> Result<Codec, PluginCreateError> {
    let configuration: ZstdCodecConfiguration = metadata.to_configuration()?;
    let codec = Box::new(ZstdCodec::new_with_configuration(&configuration));
    Ok(Codec::BytesToBytes(codec))
}

/// A Zstd codec implementation.
#[derive(Clone, Debug)]
pub struct ZstdCodec {
    compression: zstd_safe::CompressionLevel,
    checksum: bool,
}

impl ZstdCodec {
    /// Create a new `Zstd` codec.
    #[must_use]
    pub const fn new(compression: zstd_safe::CompressionLevel, checksum: bool) -> Self {
        Self {
            compression,
            checksum,
        }
    }

    /// Create a new `Zstd` codec from configuration.
    #[must_use]
    pub fn new_with_configuration(configuration: &ZstdCodecConfiguration) -> Self {
        let ZstdCodecConfiguration::V1(configuration) = configuration;
        Self {
            compression: configuration.level.clone().into(),
            checksum: configuration.checksum,
        }
    }
}

impl CodecTraits for ZstdCodec {
    fn create_metadata(&self) -> Option<Metadata> {
        let configuration = ZstdCodecConfigurationV1 {
            level: self.compression.into(),
            checksum: self.checksum,
        };
        Some(Metadata::new_with_serializable_configuration(IDENTIFIER, &configuration).unwrap())
    }

    fn partial_decoder_should_cache_input(&self) -> bool {
        false
    }

    fn partial_decoder_decodes_all(&self) -> bool {
        true
    }
}

#[cfg_attr(feature = "async", async_trait::async_trait)]
impl BytesToBytesCodecTraits for ZstdCodec {
    fn encode_opt(&self, decoded_value: Vec<u8>, parallel: bool) -> Result<Vec<u8>, CodecError> {
        let mut result = Vec::<u8>::new();
        let mut encoder = zstd::Encoder::new(&mut result, self.compression)?;
        encoder.include_checksum(self.checksum)?;
        if parallel {
            let n_threads = std::thread::available_parallelism().unwrap().get();
            encoder.multithread(u32::try_from(n_threads).unwrap())?; // TODO: Check overhead of zstd par_encode
        }
        std::io::copy(&mut decoded_value.as_slice(), &mut encoder)?;
        encoder.finish()?;
        Ok(result)
    }

    fn decode_opt(
        &self,
        encoded_value: Vec<u8>,
        _decoded_representation: &BytesRepresentation,
        _parallel: bool,
    ) -> Result<Vec<u8>, CodecError> {
        zstd::decode_all(encoded_value.as_slice()).map_err(CodecError::IOError)
    }

    #[cfg(feature = "async")]
    async fn async_encode_opt(
        &self,
        decoded_value: Vec<u8>,
        parallel: bool,
    ) -> Result<Vec<u8>, CodecError> {
        self.encode_opt(decoded_value, parallel)
    }

    #[cfg(feature = "async")]
    async fn async_decode_opt(
        &self,
        encoded_value: Vec<u8>,
        decoded_representation: &BytesRepresentation,
        parallel: bool,
    ) -> Result<Vec<u8>, CodecError> {
        // FIXME: Remove
        self.decode_opt(encoded_value, decoded_representation, parallel)
    }

    fn partial_decoder_opt<'a>(
        &self,
        r: Box<dyn BytesPartialDecoderTraits + 'a>,
        _decoded_representation: &BytesRepresentation,
        _parallel: bool,
    ) -> Result<Box<dyn BytesPartialDecoderTraits + 'a>, CodecError> {
        Ok(Box::new(zstd_partial_decoder::ZstdPartialDecoder::new(r)))
    }

    #[cfg(feature = "async")]
    async fn async_partial_decoder_opt<'a>(
        &'a self,
        r: Box<dyn AsyncBytesPartialDecoderTraits + 'a>,
        _decoded_representation: &BytesRepresentation,
        _parallel: bool,
    ) -> Result<Box<dyn AsyncBytesPartialDecoderTraits + 'a>, CodecError> {
        Ok(Box::new(
            zstd_partial_decoder::AsyncZstdPartialDecoder::new(r),
        ))
    }

    fn compute_encoded_size(
        &self,
        decoded_representation: &BytesRepresentation,
    ) -> BytesRepresentation {
        decoded_representation
            .size()
            .map_or(BytesRepresentation::UnboundedSize, |size| {
                // https://github.com/facebook/zstd/blob/dev/doc/zstd_compression_format.md
                // TODO: Validate the window/block relationship
                const HEADER_TRAILER_OVERHEAD: u64 = 4 + 14 + 4;
                const MIN_WINDOW_SIZE: u64 = 1000; // 1KB
                const BLOCK_OVERHEAD: u64 = 3;
                let blocks_overhead =
                    BLOCK_OVERHEAD * ((size + MIN_WINDOW_SIZE - 1) / MIN_WINDOW_SIZE);
                BytesRepresentation::BoundedSize(size + HEADER_TRAILER_OVERHEAD + blocks_overhead)
            })
    }
}

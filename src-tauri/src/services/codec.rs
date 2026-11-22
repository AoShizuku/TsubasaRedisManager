use crate::error::{AppError, AppResult};
use crate::models::RedisText;
use crate::services::redis_values;
use base64::Engine;
use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};
use flate2::write::{DeflateEncoder, GzEncoder, ZlibEncoder};
use flate2::Compression;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};

const MAX_CODEC_PAYLOAD_BYTES: usize = 20 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CodecName {
    Gzip,
    Deflate,
    DeflateRaw,
    Brotli,
    Msgpack,
    Pickle,
    JavaSerialize,
    PhpSerialize,
    Protobuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CodecConclusion {
    RustRoundTrip,
    RendererRoundTrip,
    Readonly,
    DeferredUnsupported,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodecDecodeOptions {
    pub codec: CodecName,
    pub input_base64: String,
}

/// 输入是 **base64 字节**而不是文本，与 `CodecDecodeOptions` 对称。
/// 编码的输入未必是文本 —— msgpack 编完再 gzip 是常见组合，
/// 中间那一步的产物是二进制。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodecEncodeOptions {
    pub codec: CodecName,
    pub input_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodecDetectOptions {
    pub input_base64: String,
    pub candidates: Vec<CodecName>,
}

/// 解码的产物是**字节**，不是文本。
///
/// 产物要是 `text: String`，`gzip → msgpack` 这种组合就走不通 ——
/// 解压完拿到 msgpack 二进制，转 String 直接失败。而 gzip 套一层序列化格式
/// 在 PHP / Python 应用里非常常见，是要害用例不是边缘情况。
/// 见 `doc/backend/codec.md`「归属切分」。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodecDecodeResult {
    pub value: RedisText,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodecEncodeResult {
    pub output_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CodecDetectResult {
    pub codec: Option<CodecName>,
}

pub fn codec_support_matrix() -> BTreeMap<CodecName, CodecConclusion> {
    BTreeMap::from([
        (CodecName::Gzip, CodecConclusion::RustRoundTrip),
        (CodecName::Deflate, CodecConclusion::RustRoundTrip),
        (CodecName::DeflateRaw, CodecConclusion::RustRoundTrip),
        (CodecName::Brotli, CodecConclusion::RustRoundTrip),
        (CodecName::Msgpack, CodecConclusion::RendererRoundTrip),
        (CodecName::PhpSerialize, CodecConclusion::RendererRoundTrip),
        (CodecName::Pickle, CodecConclusion::Readonly),
        (CodecName::JavaSerialize, CodecConclusion::Readonly),
        (CodecName::Protobuf, CodecConclusion::RendererRoundTrip),
    ])
}

pub fn codec_error(code: &str, message: &str, detail: impl Into<Option<String>>) -> AppError {
    AppError::CodecFailed {
        code: code.into(),
        message: message.into(),
        detail: detail.into(),
    }
}

pub fn decode(options: CodecDecodeOptions) -> AppResult<CodecDecodeResult> {
    if !is_rust_compression_codec(options.codec) {
        return Err(codec_error(
            "codec.unsupported",
            "Codec is not supported by Rust",
            None,
        ));
    }

    let input = decode_base64(&options.input_base64)?;
    let output = match options.codec {
        CodecName::Gzip => read_limited(GzDecoder::new(input.as_slice())),
        CodecName::Deflate => read_limited(ZlibDecoder::new(input.as_slice())),
        CodecName::DeflateRaw => read_limited(DeflateDecoder::new(input.as_slice())),
        CodecName::Brotli => read_limited(brotli::Decompressor::new(input.as_slice(), 4096)),
        _ => unreachable!("unsupported codecs are rejected before payload validation"),
    }?;

    // 这里不截断：截断由取值命令负责，解码是用户显式发起的一次性动作，
    // 两处都截会让 totalBytes 的含义变得含糊。
    Ok(CodecDecodeResult {
        value: redis_values::text_from_bytes(&output),
    })
}

pub fn encode(options: CodecEncodeOptions) -> AppResult<CodecEncodeResult> {
    if !is_rust_compression_codec(options.codec) {
        return Err(codec_error(
            "codec.unsupported",
            "Codec is not supported by Rust",
            None,
        ));
    }

    let input = decode_base64(&options.input_base64)?;
    let input = input.as_slice();

    let output = match options.codec {
        CodecName::Gzip => encode_gzip(input),
        CodecName::Deflate => encode_zlib(input),
        CodecName::DeflateRaw => encode_deflate_raw(input),
        CodecName::Brotli => encode_brotli(input),
        _ => unreachable!("unsupported codecs are rejected before payload validation"),
    }
    .map_err(|error| {
        codec_error(
            "codec.encode_failed",
            "Codec encode failed",
            Some(error.to_string()),
        )
    })?;

    Ok(CodecEncodeResult {
        output_base64: base64::engine::general_purpose::STANDARD.encode(output),
    })
}

pub fn detect(options: CodecDetectOptions) -> AppResult<CodecDetectResult> {
    for codec in options.candidates {
        if decode(CodecDecodeOptions {
            codec,
            input_base64: options.input_base64.clone(),
        })
        .is_ok()
        {
            return Ok(CodecDetectResult { codec: Some(codec) });
        }
    }
    Ok(CodecDetectResult { codec: None })
}

fn is_rust_compression_codec(codec: CodecName) -> bool {
    matches!(
        codec,
        CodecName::Gzip | CodecName::Deflate | CodecName::DeflateRaw | CodecName::Brotli
    )
}

fn decode_base64(input: &str) -> AppResult<Vec<u8>> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(input)
        .map_err(|error| {
            codec_error(
                "codec.decode_failed",
                "Codec payload is not valid base64",
                Some(error.to_string()),
            )
        })?;
    if bytes.len() > MAX_CODEC_PAYLOAD_BYTES {
        return Err(payload_too_large_error(bytes.len()));
    }
    Ok(bytes)
}

fn payload_too_large_error(size: usize) -> AppError {
    codec_error(
        "codec.payload_too_large",
        "Codec payload is too large",
        Some(size.to_string()),
    )
}

fn read_limited<R: Read>(reader: R) -> AppResult<Vec<u8>> {
    let mut reader = reader.take((MAX_CODEC_PAYLOAD_BYTES + 1) as u64);
    let mut output = Vec::new();
    reader.read_to_end(&mut output).map_err(|error| {
        codec_error(
            "codec.decode_failed",
            "Codec decode failed",
            Some(error.to_string()),
        )
    })?;
    if output.len() > MAX_CODEC_PAYLOAD_BYTES {
        return Err(payload_too_large_error(output.len()));
    }
    Ok(output)
}

fn encode_gzip(input: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(input)?;
    encoder.finish()
}

fn encode_zlib(input: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(input)?;
    encoder.finish()
}

fn encode_deflate_raw(input: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(input)?;
    encoder.finish()
}

fn encode_brotli(input: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    {
        let mut compressor = brotli::CompressorWriter::new(&mut output, 4096, 5, 22);
        compressor.write_all(input)?;
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn assert_compression_round_trip(codec: CodecName) {
        let encoded = encode(CodecEncodeOptions {
            codec,
            input_base64: b64(b"{\"hello\":\"world\"}"),
        })
        .expect("encode should succeed");
        let decoded = decode(CodecDecodeOptions {
            codec,
            input_base64: encoded.output_base64.clone(),
        })
        .expect("decode should succeed");

        assert_eq!(decoded.value.text.as_deref(), Some("{\"hello\":\"world\"}"));
        assert_eq!(
            detect(CodecDetectOptions {
                input_base64: encoded.output_base64,
                candidates: vec![codec],
            })
            .unwrap()
            .codec,
            Some(codec)
        );
    }

    /// 链式解码：gzip 里裹的是二进制（msgpack / protobuf 之类）而不是文本。
    ///
    /// 产物要是 `text: String`，这里就会直接失败 —— 解压产物转 `String` 报错。
    /// 而 gzip 套一层序列化格式在 PHP / Python 应用里非常常见，
    /// 见 `doc/backend/codec.md`「归属切分」。
    #[test]
    fn gzip_wrapping_non_utf8_payload_decodes_to_bytes() {
        // 0x81 0xa3 … 是一段合法 msgpack，但不是合法 UTF-8
        let payload: &[u8] = &[0x81, 0xa3, b'k', b'e', b'y', 0xc3, 0xff, 0xfe];
        let encoded = encode(CodecEncodeOptions {
            codec: CodecName::Gzip,
            input_base64: b64(payload),
        })
        .expect("encode should succeed");

        let decoded = decode(CodecDecodeOptions {
            codec: CodecName::Gzip,
            input_base64: encoded.output_base64,
        })
        .expect("decoding a gzip-wrapped binary payload must not fail");

        assert_eq!(decoded.value.text, None);
        assert_eq!(decoded.value.base64.as_deref(), Some(b64(payload).as_str()));
    }

    fn assert_codec_error(error: AppError, expected_code: &str) {
        assert!(matches!(
            error,
            AppError::CodecFailed { ref code, .. } if code == expected_code
        ));
    }

    fn encode_compressed_payload(codec: CodecName, input: &[u8]) -> Vec<u8> {
        match codec {
            CodecName::Gzip => encode_gzip(input).unwrap(),
            CodecName::Deflate => encode_zlib(input).unwrap(),
            CodecName::DeflateRaw => encode_deflate_raw(input).unwrap(),
            CodecName::Brotli => encode_brotli(input).unwrap(),
            _ => panic!("unsupported test codec"),
        }
    }

    #[test]
    fn serializes_codec_options_as_camel_case() {
        let options = CodecDecodeOptions {
            codec: CodecName::Gzip,
            input_base64: "aGVsbG8=".into(),
        };

        assert_eq!(
            serde_json::to_value(options).unwrap(),
            serde_json::json!({
                "codec": "gzip",
                "inputBase64": "aGVsbG8="
            })
        );
    }

    #[test]
    fn codec_support_matrix_has_explicit_phase4c_conclusions() {
        let support = codec_support_matrix();
        assert_eq!(support.len(), 9);
        assert_eq!(
            support.get(&CodecName::Gzip),
            Some(&CodecConclusion::RustRoundTrip)
        );
        assert_eq!(
            support.get(&CodecName::Deflate),
            Some(&CodecConclusion::RustRoundTrip)
        );
        assert_eq!(
            support.get(&CodecName::DeflateRaw),
            Some(&CodecConclusion::RustRoundTrip)
        );
        assert_eq!(
            support.get(&CodecName::Brotli),
            Some(&CodecConclusion::RustRoundTrip)
        );
        assert_eq!(
            support.get(&CodecName::Msgpack),
            Some(&CodecConclusion::RendererRoundTrip)
        );
        assert_eq!(
            support.get(&CodecName::PhpSerialize),
            Some(&CodecConclusion::RendererRoundTrip)
        );
        assert_eq!(
            support.get(&CodecName::Pickle),
            Some(&CodecConclusion::Readonly)
        );
        assert_eq!(
            support.get(&CodecName::JavaSerialize),
            Some(&CodecConclusion::Readonly)
        );
        assert_eq!(
            support.get(&CodecName::Protobuf),
            Some(&CodecConclusion::RendererRoundTrip)
        );
    }

    #[test]
    fn gzip_round_trips_in_rust() {
        assert_compression_round_trip(CodecName::Gzip);
    }

    #[test]
    fn deflate_round_trips_in_rust() {
        assert_compression_round_trip(CodecName::Deflate);
    }

    #[test]
    fn deflate_raw_round_trips_in_rust() {
        assert_compression_round_trip(CodecName::DeflateRaw);
    }

    #[test]
    fn brotli_round_trips_in_rust() {
        assert_compression_round_trip(CodecName::Brotli);
    }

    #[test]
    fn invalid_compression_payload_returns_decode_failed() {
        let error = decode(CodecDecodeOptions {
            codec: CodecName::Gzip,
            input_base64: b64(b"not gzip"),
        })
        .expect_err("invalid gzip should fail");

        assert_codec_error(error, "codec.decode_failed");
    }

    #[test]
    fn invalid_base64_payload_returns_decode_failed() {
        let error = decode(CodecDecodeOptions {
            codec: CodecName::Deflate,
            input_base64: "not base64@@".into(),
        })
        .expect_err("invalid base64 should fail");

        assert_codec_error(error, "codec.decode_failed");
    }

    #[test]
    fn compressed_input_larger_than_limit_returns_payload_too_large() {
        let oversized_input = vec![0; MAX_CODEC_PAYLOAD_BYTES + 1];
        let error = decode(CodecDecodeOptions {
            codec: CodecName::Gzip,
            input_base64: b64(&oversized_input),
        })
        .expect_err("oversized compressed input should fail");

        assert_codec_error(error, "codec.payload_too_large");
    }

    #[test]
    fn encode_input_larger_than_limit_returns_payload_too_large() {
        let error = encode(CodecEncodeOptions {
            codec: CodecName::Gzip,
            input_base64: b64(&vec![b'x'; MAX_CODEC_PAYLOAD_BYTES + 1]),
        })
        .expect_err("oversized encode input should fail");

        assert_codec_error(error, "codec.payload_too_large");
    }

    #[test]
    fn decompressed_output_larger_than_limit_returns_payload_too_large() {
        let oversized_output = "x".repeat(MAX_CODEC_PAYLOAD_BYTES + 1);
        for codec in [
            CodecName::Gzip,
            CodecName::Deflate,
            CodecName::DeflateRaw,
            CodecName::Brotli,
        ] {
            let compressed = encode_compressed_payload(codec, oversized_output.as_bytes());
            let error = decode(CodecDecodeOptions {
                codec,
                input_base64: b64(&compressed),
            })
            .expect_err("oversized decompressed output should fail");

            assert_codec_error(error, "codec.payload_too_large");
        }
    }

    #[test]
    fn unsupported_codec_encode_returns_unsupported() {
        let error = encode(CodecEncodeOptions {
            codec: CodecName::Msgpack,
            input_base64: b64(b"value"),
        })
        .expect_err("unsupported encode should fail");

        assert_codec_error(error, "codec.unsupported");
    }

    #[test]
    fn detect_ignores_unsupported_candidates_and_returns_first_supported_match() {
        let encoded = encode(CodecEncodeOptions {
            codec: CodecName::Gzip,
            input_base64: b64(b"{\"hello\":\"world\"}"),
        })
        .expect("encode should succeed");

        let detected = detect(CodecDetectOptions {
            input_base64: encoded.output_base64,
            candidates: vec![CodecName::Msgpack, CodecName::Gzip],
        })
        .expect("detect should succeed");

        assert_eq!(detected.codec, Some(CodecName::Gzip));
    }

    #[test]
    fn unsupported_codec_decode_returns_unsupported_before_payload_validation() {
        let error = decode(CodecDecodeOptions {
            codec: CodecName::Msgpack,
            input_base64: "not base64@@".into(),
        })
        .expect_err("unsupported codec should fail before payload validation");

        assert_codec_error(error, "codec.unsupported");
    }
}

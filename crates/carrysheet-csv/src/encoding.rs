//! Finding the encoding of a file, decoding it and encoding text back.

use encoding_rs::{DecoderResult, EncoderResult, Encoding, UTF_8, UTF_16BE, UTF_16LE};

use crate::error::CsvError;

/// How sure [`detect_encoding`] is about its guess.
///
/// The order runs from least to most sure, so `confidence >= Confidence::High`
/// works.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Confidence {
    /// A guess from statistics that lost to no other candidate, or a guess
    /// from the pattern of zero bytes. Show it to the person and let them
    /// choose.
    Low,
    /// A guess from statistics that beat the other candidates.
    Medium,
    /// The bytes are valid UTF-8 (or plain ASCII, which reads the same in
    /// every supported encoding).
    High,
    /// The file starts with a byte order mark, or the caller chose the
    /// encoding.
    Certain,
}

/// The result of [`detect_encoding`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncodingGuess {
    /// The encoding to decode the file with.
    pub encoding: &'static Encoding,
    /// How sure the guess is.
    pub confidence: Confidence,
    /// The length of the byte order mark at the start of the file, or 0.
    pub bom_len: usize,
}

/// How many bytes the statistical detector looks at. Enough for a sure
/// guess, small enough to stay fast on big files.
const SNIFF_BYTES: usize = 1 << 20;

/// Guesses the encoding of `bytes`.
///
/// In order: a byte order mark (UTF-8, UTF-16 LE, UTF-16 BE) is
/// [`Confidence::Certain`]; zero bytes in every other position suggest
/// UTF-16 without a byte order mark ([`Confidence::Low`]); valid UTF-8 is
/// [`Confidence::High`]; everything else goes to `chardetng`, the detector
/// Firefox uses, which knows GBK/GB18030, Big5, Shift_JIS, EUC-KR,
/// Windows-1252 and the other legacy encodings of the Web.
///
/// GB18030 files are reported as GBK: both decode with the same decoder in
/// `encoding_rs`, so the text comes out the same. Choose GB18030 on write if
/// you need its four-byte characters.
pub fn detect_encoding(bytes: &[u8]) -> EncodingGuess {
    if let Some((encoding, bom_len)) = Encoding::for_bom(bytes) {
        return EncodingGuess {
            encoding,
            confidence: Confidence::Certain,
            bom_len,
        };
    }
    let sample = bytes.get(..SNIFF_BYTES).unwrap_or(bytes);
    // Before the UTF-8 check: zero bytes are valid UTF-8, so ASCII text in
    // UTF-16 passes it.
    if let Some(encoding) = utf16_by_zero_bytes(sample) {
        return EncodingGuess {
            encoding,
            confidence: Confidence::Low,
            bom_len: 0,
        };
    }
    if std::str::from_utf8(bytes).is_ok() {
        return EncodingGuess {
            encoding: UTF_8,
            confidence: Confidence::High,
            bom_len: 0,
        };
    }
    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(sample, sample.len() == bytes.len());
    let (encoding, sure) = detector.guess_assess(None, false);
    let confidence = if sure {
        Confidence::Medium
    } else {
        Confidence::Low
    };
    EncodingGuess {
        encoding,
        confidence,
        bom_len: 0,
    }
}

/// Looks up an encoding by a label such as `utf-8`, `gbk`, `big5`,
/// `shift_jis`, `euc-kr` or `windows-1252` (the WHATWG labels, case
/// insensitive). Returns `None` for an unknown label.
pub fn encoding_for_label(label: &str) -> Option<&'static Encoding> {
    Encoding::for_label(label.trim().as_bytes())
}

/// Text in the legacy encodings never contains zero bytes; mostly-ASCII
/// UTF-16 has one in every other position.
fn utf16_by_zero_bytes(sample: &[u8]) -> Option<&'static Encoding> {
    let sample = sample.get(..4096).unwrap_or(sample);
    if sample.len() < 4 {
        return None;
    }
    let mut zero_even = 0usize;
    let mut zero_odd = 0usize;
    for (i, &b) in sample.iter().enumerate() {
        if b == 0 {
            if i % 2 == 0 {
                zero_even += 1;
            } else {
                zero_odd += 1;
            }
        }
    }
    let pairs = sample.len() / 2;
    // At least a quarter of the characters are ASCII, and the zero bytes sit
    // on one side only.
    if zero_odd * 4 >= pairs && zero_even * 10 <= zero_odd {
        Some(UTF_16LE)
    } else if zero_even * 4 >= pairs && zero_odd * 10 <= zero_even {
        Some(UTF_16BE)
    } else {
        None
    }
}

/// Decodes `bytes` (without its byte order mark). With `replace`, bad bytes
/// become U+FFFD and are counted; without, the first one is an error.
pub(crate) fn decode(
    bytes: &[u8],
    encoding: &'static Encoding,
    replace: bool,
) -> Result<(String, usize), CsvError> {
    let mut decoder = encoding.new_decoder_without_bom_handling();
    let too_large = || CsvError::InputTooLarge { limit: bytes.len() };
    let capacity = decoder
        .max_utf8_buffer_length_without_replacement(bytes.len())
        .ok_or_else(too_large)?;
    let mut out = String::with_capacity(capacity);
    let mut consumed = 0usize;
    let mut replaced = 0usize;
    loop {
        let rest = bytes.get(consumed..).unwrap_or_default();
        let (result, read) = decoder.decode_to_string_without_replacement(rest, &mut out, true);
        consumed += read;
        match result {
            DecoderResult::InputEmpty => return Ok((out, replaced)),
            DecoderResult::OutputFull => {
                let remaining = bytes.len().saturating_sub(consumed);
                let more = decoder
                    .max_utf8_buffer_length_without_replacement(remaining)
                    .ok_or_else(too_large)?;
                out.reserve(more.max(4));
            }
            DecoderResult::Malformed(bad, after) => {
                if !replace {
                    let offset = consumed.saturating_sub(usize::from(bad) + usize::from(after));
                    return Err(CsvError::MalformedText {
                        encoding: encoding.name(),
                        offset,
                    });
                }
                out.push('\u{FFFD}');
                replaced += 1;
            }
        }
    }
}

/// Why [`encode`] failed.
pub(crate) enum EncodeError {
    /// `character` at byte `offset` of the text has no code.
    Unmappable { character: char, offset: usize },
    /// Any other problem, ready to return.
    Other(CsvError),
}

/// Encodes `text`, with a byte order mark when `bom` is set.
pub(crate) fn encode(
    text: &str,
    encoding: &'static Encoding,
    bom: bool,
) -> Result<Vec<u8>, EncodeError> {
    if encoding == UTF_8 {
        let mut out = Vec::with_capacity(text.len() + 3);
        if bom {
            out.extend_from_slice(b"\xEF\xBB\xBF");
        }
        out.extend_from_slice(text.as_bytes());
        return Ok(out);
    }
    if encoding == UTF_16LE || encoding == UTF_16BE {
        let little = encoding == UTF_16LE;
        let mut out = Vec::with_capacity(text.len().saturating_mul(2).saturating_add(2));
        let units = bom
            .then_some(0xFEFF_u16)
            .into_iter()
            .chain(text.encode_utf16());
        for unit in units {
            out.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        return Ok(out);
    }
    if encoding.output_encoding() != encoding {
        return Err(EncodeError::Other(CsvError::CannotWriteEncoding {
            encoding: encoding.name(),
        }));
    }
    if bom {
        return Err(EncodeError::Other(CsvError::BomNotAvailable {
            encoding: encoding.name(),
        }));
    }
    let mut encoder = encoding.new_encoder();
    let capacity = encoder
        .max_buffer_length_from_utf8_without_replacement(text.len())
        .unwrap_or(text.len());
    let mut out = Vec::with_capacity(capacity);
    let mut consumed = 0usize;
    loop {
        let rest = text.get(consumed..).unwrap_or_default();
        let (result, read) =
            encoder.encode_from_utf8_to_vec_without_replacement(rest, &mut out, true);
        consumed += read;
        match result {
            EncoderResult::InputEmpty => return Ok(out),
            EncoderResult::OutputFull => {
                let remaining = text.len().saturating_sub(consumed);
                let more = encoder
                    .max_buffer_length_from_utf8_without_replacement(remaining)
                    .unwrap_or(remaining);
                out.reserve(more.max(16));
            }
            EncoderResult::Unmappable(character) => {
                let offset = consumed.saturating_sub(character.len_utf8());
                return Err(EncodeError::Unmappable { character, offset });
            }
        }
    }
}

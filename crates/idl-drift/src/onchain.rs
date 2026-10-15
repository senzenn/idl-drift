//! The IDL that a program publishes on chain: in its canonical Program Metadata `idl`
//! account, or in its legacy Anchor IDL account.
//!
//! The account data is untrusted. Each offset and length is checked, decompression stops at
//! [`MAX_IDL_BYTES`], and an IDL with a control character in a string is refused, because
//! the report prints its names to a terminal.

use std::fmt;
use std::io::Read;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use curve25519_dalek::edwards::CompressedEdwardsY;
use flate2::read::{GzDecoder, ZlibDecoder};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::model::Idl;

/// The Program Metadata program.
pub const METADATA_PROGRAM: &str = "ProgM6JCCvbYkfKqJYHePx4xxSUSqJp7rh8Lyv7nk7S";

/// The most bytes that an IDL can decompress to. A small account can expand to gigabytes.
pub const MAX_IDL_BYTES: usize = 16 << 20;

/// `sha256("internal:IdlAccount")[..8]`, from `#[account("internal")]` in Anchor.
const ANCHOR_DISCRIMINATOR: [u8; 8] = [24, 70, 98, 191, 58, 144, 123, 158];
/// The discriminator, the authority, and the `u32` length of the zlib data that follows.
const ANCHOR_HEADER_LEN: usize = 44;

/// `idl`, padded with zeros to the 16-byte seed of a metadata account.
const METADATA_SEED: [u8; 16] = *b"idl\0\0\0\0\0\0\0\0\0\0\0\0\0";
const METADATA_HEADER_LEN: usize = 96;
/// An initialized metadata account. An empty account is 0 and a buffer is 1.
const METADATA_DISCRIMINATOR: u8 = 2;

type Key = [u8; 32];

/// The account that holds a program's IDL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The canonical `idl` account of the Program Metadata program. Anchor 1.0 and later
    /// write only this one.
    Metadata,
    /// The account that `anchor idl init` writes before Anchor 1.0.
    Anchor,
}

impl Source {
    /// The name of the source in `--source` and in JSON output.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Anchor => "anchor",
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Metadata => "Program Metadata account",
            Self::Anchor => "Anchor IDL account",
        })
    }
}

/// An IDL read from chain.
#[derive(Debug)]
pub struct OnchainIdl {
    pub idl: Idl,
    pub source: Source,
    /// The address of the account that holds the IDL.
    pub account: String,
}

/// Why an on-chain IDL cannot be read.
#[derive(Debug)]
pub enum Error {
    /// The program id is not a base58 32-byte address.
    Program(String),
    /// The RPC request failed.
    Rpc(String),
    /// The RPC answer does not have the shape that the RPC documents.
    RpcResponse(String),
    /// The program has no IDL account of the sources that were searched, by address.
    NotFound {
        program: String,
        searched: Vec<(Source, String)>,
    },
    /// The program has both IDL accounts, by address, and no source was given.
    BothSources {
        program: String,
        metadata: String,
        anchor: String,
    },
    /// An IDL account exists, but its data does not hold an IDL that can be read.
    Decode {
        source: Source,
        account: String,
        error: DecodeError,
    },
    /// The IDL holds a control character in a string.
    ControlCharacter { source: Source, account: String },
    /// The IDL is not valid JSON, or not an IDL.
    Idl {
        source: Source,
        account: String,
        error: serde_json::Error,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Program(program) => write!(f, "`{program}` is not a base58 program address"),
            Self::Rpc(detail) => write!(f, "RPC request failed: {detail}"),
            Self::RpcResponse(detail) => write!(f, "unexpected RPC response: {detail}"),
            Self::NotFound { program, searched } => {
                let searched: Vec<String> = searched
                    .iter()
                    .map(|(source, account)| format!("no {source} at {account}"))
                    .collect();
                write!(
                    f,
                    "program {program} has no on-chain IDL: {}",
                    searched.join(", ")
                )
            }
            Self::BothSources {
                program,
                metadata,
                anchor,
            } => write!(
                f,
                "program {program} has two on-chain IDLs, which can differ: {} {metadata} and {} {anchor}; pick one with --source {} or --source {}",
                Source::Metadata,
                Source::Anchor,
                Source::Metadata.key(),
                Source::Anchor.key()
            ),
            Self::Decode {
                source,
                account,
                error,
            } => write!(f, "{source} {account}: {error}"),
            Self::ControlCharacter { source, account } => write!(
                f,
                "{source} {account}: the IDL holds control characters, which can change a terminal; idl-drift does not print it"
            ),
            Self::Idl {
                source,
                account,
                error,
            } => write!(f, "{source} {account}: cannot parse the IDL: {error}"),
        }
    }
}

impl std::error::Error for Error {}

/// Why the data of an IDL account cannot be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The data ends at `len`, before byte `need` that the header needs.
    Truncated {
        need: usize,
        len: usize,
    },
    /// The data does not start with the discriminator of an IDL account.
    Discriminator,
    /// A metadata account of another program.
    Program,
    /// A metadata account that the program's upgrade authority did not write.
    NotCanonical,
    /// A metadata account with a seed other than `idl`.
    Seed,
    /// The IDL is at a URL (1) or in another account (2), not in this account.
    DataSource(u8),
    /// The data format is YAML (2), TOML (3) or unknown, not JSON (1) or unset (0).
    Format(u8),
    /// The data encoding is not UTF-8 (1), the only one that holds JSON text.
    Encoding(u8),
    Compression(u8),
    Decompress(String),
    /// The IDL decompresses to more than [`MAX_IDL_BYTES`].
    TooLarge,
    NotUtf8,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { need, len } => write!(
                f,
                "the data is {len} bytes, but the header needs {need} bytes"
            ),
            Self::Discriminator => {
                f.write_str("the data does not start with the discriminator of an IDL account")
            }
            Self::Program => f.write_str("the metadata account is for another program"),
            Self::NotCanonical => f.write_str(
                "the metadata account is not canonical, so the program's upgrade authority did not write it",
            ),
            Self::Seed => f.write_str("the metadata account seed is not `idl`"),
            Self::DataSource(1) => f.write_str(
                "the IDL is at a URL (data source 1); idl-drift reads only an IDL stored in the account",
            ),
            Self::DataSource(2) => f.write_str(
                "the IDL is in another account (data source 2); idl-drift reads only an IDL stored in the account",
            ),
            Self::DataSource(n) => write!(f, "unknown data source {n}"),
            Self::Format(n) => write!(f, "the data format is {n}, not JSON (1)"),
            Self::Encoding(n) => write!(
                f,
                "the data encoding is {n}; only UTF-8 (1) holds JSON text"
            ),
            Self::Compression(n) => write!(f, "unknown compression {n}"),
            Self::Decompress(e) => write!(f, "cannot decompress the IDL: {e}"),
            Self::TooLarge => write!(
                f,
                "the IDL is larger than {MAX_IDL_BYTES} bytes after decompression"
            ),
            Self::NotUtf8 => f.write_str("the IDL is not UTF-8 text"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Read the IDL that `program` publishes on chain, from the account of `source`. With no
/// `source`, read the one IDL account that the program has. `rpc` sends one JSON-RPC
/// request and returns its `result`.
///
/// # Errors
///
/// When `program` is not an address, the request fails, the program has no IDL account of
/// `source`, it has both and `source` is `None`, or the account does not hold an IDL that
/// can be read.
pub fn fetch(
    mut rpc: impl FnMut(&str, Value) -> Result<Value, Error>,
    program: &str,
    source: Option<Source>,
) -> Result<OnchainIdl, Error> {
    let key = parse_key(program).ok_or_else(|| Error::Program(program.to_string()))?;
    let program = bs58::encode(key).into_string();
    let candidates: Vec<(Source, String, &str)> = [
        (Source::Metadata, metadata_address(&key), METADATA_PROGRAM),
        (Source::Anchor, anchor_idl_address(&key), program.as_str()),
    ]
    .into_iter()
    .filter(|(s, _, _)| source.is_none_or(|asked| asked == *s))
    .filter_map(|(s, address, owner)| Some((s, bs58::encode(address?).into_string(), owner)))
    .collect();

    let keys: Vec<&str> = candidates.iter().map(|(_, a, _)| a.as_str()).collect();
    let result = rpc(
        "getMultipleAccounts",
        json!([keys, { "encoding": "base64" }]),
    )?;
    let page = AccountsPage::deserialize(&result)
        .map_err(|e| Error::RpcResponse(format!("getMultipleAccounts: {e}")))?;
    if page.value.len() != candidates.len() {
        return Err(Error::RpcResponse(format!(
            "getMultipleAccounts: {} accounts for {} keys",
            page.value.len(),
            candidates.len()
        )));
    }

    let mut found = Vec::new();
    for ((s, address, owner), value) in candidates.iter().zip(page.value) {
        // Anyone can send lamports to the address, which makes a system account with no data.
        let Some(account) = value.filter(|a| a.owner == *owner) else {
            continue;
        };
        if account.data.1 != "base64" {
            return Err(Error::RpcResponse(format!(
                "account {address}: the data is not base64"
            )));
        }
        let data = STANDARD
            .decode(&account.data.0)
            .map_err(|e| Error::RpcResponse(format!("account {address}: data: {e}")))?;
        found.push((*s, address.clone(), data));
    }
    // The two can differ a lot: Drift's metadata IDL has 3 of the 241 instructions in its
    // Anchor IDL account. So the user picks one.
    if let [(_, metadata, _), (_, anchor, _)] = &found[..] {
        return Err(Error::BothSources {
            program,
            metadata: metadata.clone(),
            anchor: anchor.clone(),
        });
    }
    let Some((source, account, data)) = found.pop() else {
        let searched = candidates.into_iter().map(|(s, a, _)| (s, a)).collect();
        return Err(Error::NotFound { program, searched });
    };

    let text = match source {
        Source::Metadata => decode_metadata(&data, &key),
        Source::Anchor => decode_anchor(&data),
    }
    .map_err(|error| Error::Decode {
        source,
        account: account.clone(),
        error,
    })?;
    let idl_error = |error| Error::Idl {
        source,
        account: account.clone(),
        error,
    };
    let value: Value = serde_json::from_str(&text).map_err(idl_error)?;
    // The report prints names, and a terminal runs the escape sequences in them.
    if has_control(&value) {
        return Err(Error::ControlCharacter {
            source,
            account: account.clone(),
        });
    }
    let mut idl = Idl::from_json(&text).map_err(idl_error)?;
    // A legacy IDL has no address. The program asked for is its address.
    if idl.address.is_empty() {
        idl.address = program;
    }
    Ok(OnchainIdl {
        idl,
        source,
        account,
    })
}

/// Whether a key or string holds a control character. `docs` are skipped: the model drops
/// them, and a doc comment can hold a tab.
fn has_control(value: &Value) -> bool {
    let control = |s: &str| s.chars().any(char::is_control);
    match value {
        Value::String(s) => control(s),
        Value::Array(items) => items.iter().any(has_control),
        Value::Object(map) => map
            .iter()
            .any(|(k, v)| control(k) || (k != "docs" && has_control(v))),
        _ => false,
    }
}

#[derive(Deserialize)]
struct AccountsPage {
    value: Vec<Option<RpcAccount>>,
}

/// An account from `getMultipleAccounts`; `data` is `[<base64>, "base64"]`.
#[derive(Deserialize)]
struct RpcAccount {
    owner: String,
    data: (String, String),
}

/// The canonical Program Metadata `idl` account of `program`: the address that the seeds
/// `[program, seed]` derive under the Program Metadata program.
#[must_use]
pub fn metadata_address(program: &[u8; 32]) -> Option<[u8; 32]> {
    find_program_address(&[program, &METADATA_SEED], &parse_key(METADATA_PROGRAM)?)
}

/// The Anchor IDL account of `program`, as `IdlAccount::address` derives it: `create_with_seed`
/// from the program's address of no seeds, with the seed `anchor:idl` and the program as owner.
#[must_use]
pub fn anchor_idl_address(program: &[u8; 32]) -> Option<[u8; 32]> {
    let base = find_program_address(&[], program)?;
    Some(sha256(&[&base, b"anchor:idl", program]))
}

/// The IDL JSON text in the data of a legacy Anchor IDL account: the header, then
/// zlib data of the length that the header gives.
///
/// # Errors
///
/// When the data is short, has another discriminator, or does not decompress to UTF-8 text
/// of at most [`MAX_IDL_BYTES`].
pub fn decode_anchor(data: &[u8]) -> Result<String, DecodeError> {
    let header = header(data, ANCHOR_HEADER_LEN)?;
    if header[..8] != ANCHOR_DISCRIMINATOR {
        return Err(DecodeError::Discriminator);
    }
    let len = u32::from_le_bytes([header[40], header[41], header[42], header[43]]);
    let body = body(data, ANCHOR_HEADER_LEN, len)?;
    text(ZlibDecoder::new(body))
}

/// The IDL JSON text in the data of the canonical Program Metadata `idl` account of
/// `program`: a 96-byte header, then the data that the header describes.
///
/// # Errors
///
/// When the data is short, the header is not that of a canonical JSON IDL of `program`
/// stored in the account, or the data does not decompress to UTF-8 text of at most
/// [`MAX_IDL_BYTES`].
pub fn decode_metadata(data: &[u8], program: &[u8; 32]) -> Result<String, DecodeError> {
    let header = header(data, METADATA_HEADER_LEN)?;
    if header[0] != METADATA_DISCRIMINATOR {
        return Err(DecodeError::Discriminator);
    }
    if header[1..33] != program[..] {
        return Err(DecodeError::Program);
    }
    // Only the upgrade authority can write a canonical account; anyone can write the others.
    if header[66] == 0 {
        return Err(DecodeError::NotCanonical);
    }
    if header[67..83] != METADATA_SEED {
        return Err(DecodeError::Seed);
    }
    // The data source decides what the other fields describe, so it goes first.
    if header[86] != 0 {
        return Err(DecodeError::DataSource(header[86]));
    }
    // The metadata CLI leaves the format unset (0) for a file without a `.json` extension,
    // as in Whirlpool's IDL, so the JSON parse decides.
    if header[85] > 1 {
        return Err(DecodeError::Format(header[85]));
    }
    if header[83] != 1 {
        return Err(DecodeError::Encoding(header[83]));
    }
    let len = u32::from_le_bytes([header[87], header[88], header[89], header[90]]);
    let body = body(data, METADATA_HEADER_LEN, len)?;
    match header[84] {
        0 => text(body),
        1 => text(GzDecoder::new(body)),
        2 => text(ZlibDecoder::new(body)),
        n => Err(DecodeError::Compression(n)),
    }
}

fn header(data: &[u8], len: usize) -> Result<&[u8], DecodeError> {
    data.get(..len).ok_or(DecodeError::Truncated {
        need: len,
        len: data.len(),
    })
}

/// The `len` bytes after the header.
fn body(data: &[u8], start: usize, len: u32) -> Result<&[u8], DecodeError> {
    let end = usize::try_from(len)
        .ok()
        .and_then(|len| start.checked_add(len))
        .unwrap_or(usize::MAX);
    data.get(start..end).ok_or(DecodeError::Truncated {
        need: end,
        len: data.len(),
    })
}

/// Read `reader` to its end as UTF-8, up to [`MAX_IDL_BYTES`].
fn text(reader: impl Read) -> Result<String, DecodeError> {
    let mut out = Vec::new();
    reader
        .take(MAX_IDL_BYTES as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| DecodeError::Decompress(e.to_string()))?;
    if out.len() > MAX_IDL_BYTES {
        return Err(DecodeError::TooLarge);
    }
    String::from_utf8(out).map_err(|_| DecodeError::NotUtf8)
}

fn parse_key(key: &str) -> Option<Key> {
    bs58::decode(key).into_vec().ok()?.try_into().ok()
}

fn sha256(parts: &[&[u8]]) -> Key {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update(part);
    }
    hash.finalize().into()
}

/// `Pubkey::find_program_address`: the hash of the seeds, a bump, the program and a marker,
/// for the first bump from 255 down that gives an address off the ed25519 curve.
fn find_program_address(seeds: &[&[u8]], program: &Key) -> Option<Key> {
    (0..=u8::MAX).rev().find_map(|bump| {
        let bump = [bump];
        let mut parts = seeds.to_vec();
        parts.extend_from_slice(&[&bump, program, b"ProgramDerivedAddress"]);
        let address = sha256(&parts);
        (!on_curve(&address)).then_some(address)
    })
}

/// The curve check of Solana's `bytes_are_curve_point`.
fn on_curve(bytes: &Key) -> bool {
    CompressedEdwardsY(*bytes).decompress().is_some()
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::write::{GzEncoder, ZlibEncoder};

    use super::*;

    const PROGRAM: &str = "whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc";

    fn key(s: &str) -> Key {
        parse_key(s).unwrap()
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut z = ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(data).unwrap();
        z.finish().unwrap()
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut z = GzEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(data).unwrap();
        z.finish().unwrap()
    }

    /// An Anchor IDL account as `anchor idl init` writes it, with spare room at the end.
    fn anchor_account(idl: &[u8]) -> Vec<u8> {
        let body = zlib(idl);
        let mut data = ANCHOR_DISCRIMINATOR.to_vec();
        data.extend([7; 32]);
        data.extend(u32::try_from(body.len()).unwrap().to_le_bytes());
        data.extend(body);
        data.extend([0; 64]);
        data
    }

    /// A canonical metadata account of `PROGRAM` that stores `body` in it.
    fn metadata_account(compression: u8, body: &[u8]) -> Vec<u8> {
        let mut data = vec![METADATA_DISCRIMINATOR];
        data.extend(key(PROGRAM));
        data.extend([0; 32]);
        data.extend([1, 1]);
        data.extend(METADATA_SEED);
        data.extend([1, compression, 1, 0]);
        data.extend(u32::try_from(body.len()).unwrap().to_le_bytes());
        data.extend([0; 5]);
        data.extend(body);
        data
    }

    const IDL: &str = r#"{"address":"whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc",
        "metadata":{"name":"whirlpool","version":"0.1.0","spec":"0.1.0"},
        "instructions":[{"name":"swap","discriminator":[1],"accounts":[],"args":[],
        "docs":["Swap\ttokens."]}]}"#;

    const LEGACY_IDL: &str = r#"{"version":"0.1.0","name":"whirlpool","instructions":[]}"#;

    /// Addresses from `solana find-program-derived-address` and `solana
    /// create-address-with-seed` (Agave 4.0.3); each account holds an IDL on mainnet.
    #[test]
    fn derives_the_idl_accounts_of_mainnet_programs() {
        // Whirlpool's metadata bump is 247 and Drift's are 254 and 253, so the curve check
        // rejects some hashes.
        let cases = [
            (
                "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P",
                "A6Y3keWVgXiJcWXYhGYYEauYaepb9NzrF6PChVeKdDoj",
                "AYgC53tU5BbP2NAnv5nConJxAdpQZctvmZK88pu69xRs",
            ),
            (
                PROGRAM,
                "9fQgUeE2BpGXLHopvZhDKTskXV2druezgX6hpmscbya5",
                "2KFqE4RWoPVbvodo8vbggCFeHPS8TDvgpwp79ALMrcyn",
            ),
            (
                "dRiftyHA39MWEi3m9aunc5MzRF1JYuBsbn6VPcn33UH",
                "7DuBKBbixzCJEFgvAxpt7MCUuSwuY854iYJ4BLpzPEVt",
                "8BKPjRu7Hvd6Y2J67EdLx9MsrZFQwEMVwYg7NeVbAPF8",
            ),
        ];
        for (program, metadata, anchor) in cases {
            let program = key(program);
            assert_eq!(metadata_address(&program), Some(key(metadata)));
            assert_eq!(anchor_idl_address(&program), Some(key(anchor)));
        }
    }

    #[test]
    fn reads_an_anchor_idl_account() {
        assert_eq!(decode_anchor(&anchor_account(IDL.as_bytes())).unwrap(), IDL);
    }

    #[test]
    fn reads_a_metadata_account_in_each_compression() {
        let program = key(PROGRAM);
        for (compression, body) in [
            (0, IDL.as_bytes().to_vec()),
            (1, gzip(IDL.as_bytes())),
            (2, zlib(IDL.as_bytes())),
        ] {
            let data = metadata_account(compression, &body);
            assert_eq!(decode_metadata(&data, &program).unwrap(), IDL);
        }
    }

    #[test]
    fn decompression_stops_at_the_size_cap() {
        let fits = anchor_account(&vec![b' '; MAX_IDL_BYTES]);
        assert_eq!(decode_anchor(&fits).unwrap().len(), MAX_IDL_BYTES);
        let bomb = anchor_account(&vec![b' '; MAX_IDL_BYTES + 1]);
        assert!(bomb.len() < 64 * 1024, "{}", bomb.len());
        assert_eq!(decode_anchor(&bomb), Err(DecodeError::TooLarge));
    }

    #[test]
    fn rejects_truncated_accounts() {
        let anchor = anchor_account(IDL.as_bytes());
        let body_len = anchor.len() - 64 - ANCHOR_HEADER_LEN;
        let cut = |data: &[u8], len: usize| data[..len].to_vec();
        // A header that is short, and a length past the end of the data.
        assert_eq!(
            decode_anchor(&cut(&anchor, 43)),
            Err(DecodeError::Truncated { need: 44, len: 43 })
        );
        let short = cut(&anchor, ANCHOR_HEADER_LEN + body_len - 1);
        assert!(matches!(
            decode_anchor(&short),
            Err(DecodeError::Truncated { .. })
        ));
        let mut huge = anchor.clone();
        huge[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            decode_anchor(&huge),
            Err(DecodeError::Truncated { .. })
        ));
        // A zlib stream cut short inside a length that fits.
        let mut stream = anchor.clone();
        let half = u32::try_from(body_len / 2).unwrap();
        stream[40..44].copy_from_slice(&half.to_le_bytes());
        assert!(matches!(
            decode_anchor(&stream),
            Err(DecodeError::Decompress(_))
        ));

        let metadata = metadata_account(2, &zlib(IDL.as_bytes()));
        assert_eq!(
            decode_metadata(&cut(&metadata, 95), &key(PROGRAM)),
            Err(DecodeError::Truncated { need: 96, len: 95 })
        );
        assert!(matches!(
            decode_metadata(&cut(&metadata, metadata.len() - 1), &key(PROGRAM)),
            Err(DecodeError::Truncated { .. })
        ));
    }

    #[test]
    fn rejects_metadata_headers_it_cannot_trust() {
        let good = metadata_account(2, &zlib(IDL.as_bytes()));
        let cases = [
            (0, 1, DecodeError::Discriminator),
            (1, 9, DecodeError::Program),
            (66, 0, DecodeError::NotCanonical),
            (68, b'x', DecodeError::Seed),
            (86, 1, DecodeError::DataSource(1)),
            (86, 2, DecodeError::DataSource(2)),
            (85, 2, DecodeError::Format(2)),
            (83, 3, DecodeError::Encoding(3)),
            (84, 3, DecodeError::Compression(3)),
        ];
        for (offset, value, want) in cases {
            let mut data = good.clone();
            data[offset] = value;
            assert_eq!(decode_metadata(&data, &key(PROGRAM)), Err(want));
        }
        assert!(decode_metadata(&good, &key(PROGRAM)).is_ok());
        let mut unset = good.clone();
        unset[85] = 0;
        assert!(decode_metadata(&unset, &key(PROGRAM)).is_ok());
    }

    const METADATA_ACCOUNT: &str = "9fQgUeE2BpGXLHopvZhDKTskXV2druezgX6hpmscbya5";
    const ANCHOR_ACCOUNT: &str = "2KFqE4RWoPVbvodo8vbggCFeHPS8TDvgpwp79ALMrcyn";

    /// An RPC that answers `getMultipleAccounts` from `accounts`, each `(address, owner, data)`.
    fn rpc(
        accounts: Vec<(&'static str, &'static str, Vec<u8>)>,
    ) -> impl FnMut(&str, Value) -> Result<Value, Error> {
        move |method, params| {
            assert_eq!(method, "getMultipleAccounts");
            let value: Vec<Value> = params[0]
                .as_array()
                .unwrap()
                .iter()
                .map(|key| match accounts.iter().find(|(a, _, _)| key == *a) {
                    Some((_, owner, data)) => json!({
                        "owner": owner, "lamports": 1, "executable": false,
                        "data": [STANDARD.encode(data), "base64"],
                    }),
                    None => Value::Null,
                })
                .collect();
            Ok(json!({ "context": { "slot": 1 }, "value": value }))
        }
    }

    #[test]
    fn fetch_needs_a_source_when_the_program_has_both_accounts() {
        let both = || {
            rpc(vec![
                (
                    METADATA_ACCOUNT,
                    METADATA_PROGRAM,
                    metadata_account(2, &zlib(IDL.as_bytes())),
                ),
                (
                    ANCHOR_ACCOUNT,
                    PROGRAM,
                    anchor_account(LEGACY_IDL.as_bytes()),
                ),
            ])
        };
        let err = fetch(both(), PROGRAM, None).unwrap_err();
        assert!(matches!(err, Error::BothSources { .. }), "{err}");
        let text = err.to_string();
        assert!(
            text.contains(METADATA_ACCOUNT) && text.contains(ANCHOR_ACCOUNT),
            "{text}"
        );

        let found = fetch(both(), PROGRAM, Some(Source::Metadata)).unwrap();
        assert_eq!(
            (found.source, found.account.as_str()),
            (Source::Metadata, METADATA_ACCOUNT)
        );
        assert_eq!(found.idl.instructions.len(), 1);
        let found = fetch(both(), PROGRAM, Some(Source::Anchor)).unwrap();
        assert_eq!(
            (found.source, found.account.as_str()),
            (Source::Anchor, ANCHOR_ACCOUNT)
        );
    }

    #[test]
    fn fetch_reads_the_only_account_and_gives_a_legacy_idl_the_program_address() {
        let anchor = rpc(vec![(
            ANCHOR_ACCOUNT,
            PROGRAM,
            anchor_account(LEGACY_IDL.as_bytes()),
        )]);
        let found = fetch(anchor, PROGRAM, None).unwrap();
        assert_eq!(found.source, Source::Anchor);
        assert_eq!(found.idl.address, PROGRAM);
    }

    #[test]
    fn accounts_of_another_owner_are_not_idl_accounts() {
        let system = "11111111111111111111111111111111";
        let accounts = rpc(vec![
            (
                METADATA_ACCOUNT,
                system,
                metadata_account(2, &zlib(IDL.as_bytes())),
            ),
            (ANCHOR_ACCOUNT, system, anchor_account(IDL.as_bytes())),
        ]);
        let err = fetch(accounts, PROGRAM, None).unwrap_err();
        assert!(matches!(err, Error::NotFound { .. }), "{err}");
        assert!(
            err.to_string().contains("no Anchor IDL account at 2KFq"),
            "{err}"
        );
    }

    #[test]
    fn refuses_an_idl_with_control_characters_in_a_name() {
        let hostile = IDL.replace(r#""swap""#, r#""swap\u001b[2J""#);
        let accounts = rpc(vec![(
            ANCHOR_ACCOUNT,
            PROGRAM,
            anchor_account(hostile.as_bytes()),
        )]);
        let err = fetch(accounts, PROGRAM, Some(Source::Anchor)).unwrap_err();
        assert!(matches!(err, Error::ControlCharacter { .. }), "{err}");
        assert!(!err.to_string().contains('\u{1b}'), "{err:?}");
    }
}

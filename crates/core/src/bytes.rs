//! Encodings shared by the format: hex, base64, Crockford base32, raw DEFLATE.

use std::io::{Read, Write};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use sha2::{Digest, Sha256};

use crate::{Error, Result};

pub fn to_base64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

pub fn from_base64(text: &str) -> Result<Vec<u8>> {
    STANDARD.decode(text).map_err(|e| Error::Format(format!("invalid base64: {e}")))
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Raw DEFLATE (encoding 1 in the format spec).
pub fn deflate(data: &[u8]) -> Vec<u8> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).expect("writing to a Vec cannot fail");
    encoder.finish().expect("writing to a Vec cannot fail")
}

pub fn inflate(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    DeflateDecoder::new(data).read_to_end(&mut out).map_err(|e| Error::Format(format!("invalid DEFLATE data: {e}")))?;
    Ok(out)
}

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Crockford base32, in hyphen-separated groups of four.
pub fn to_base32_groups(bytes: &[u8]) -> String {
    let mut out = String::new();
    let (mut value, mut bits) = (0u32, 0u32);
    for &b in bytes {
        value = (value << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            out.push(CROCKFORD[((value >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
        value &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(CROCKFORD[((value << (5 - bits)) & 31) as usize] as char);
    }
    out.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).expect("ASCII")).collect::<Vec<_>>().join("-")
}

/// Reads Crockford base32 leniently: ignores case, spaces and hyphens, and
/// maps O to 0 and I/L to 1. Returns `None` if anything else is wrong.
pub fn from_base32_groups(text: &str, length: usize) -> Option<Vec<u8>> {
    let clean: Vec<u8> = text
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            other => other,
        })
        .map(|c| c as u32)
        .map(|c| u8::try_from(c).unwrap_or(0))
        .collect();
    if clean.len() != (length * 8).div_ceil(5) {
        return None;
    }
    let mut out = Vec::with_capacity(length);
    let (mut value, mut bits) = (0u32, 0u32);
    for c in clean {
        let digit = CROCKFORD.iter().position(|&d| d == c)? as u32;
        value = (value << 5) | digit;
        bits += 5;
        if bits >= 8 {
            if out.len() < length {
                out.push(((value >> (bits - 8)) & 255) as u8);
            }
            bits -= 8;
        }
        value &= (1 << bits) - 1;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_round_trip_and_leniency() {
        let bytes: Vec<u8> = (0u8..32).map(|i| i.wrapping_mul(37)).collect();
        let text = to_base32_groups(&bytes);
        assert_eq!(text.replace('-', "").len(), 52);
        assert_eq!(from_base32_groups(&text, 32).unwrap(), bytes);
        let sloppy = text.to_lowercase().replace('-', " ").replace('0', "o");
        assert_eq!(from_base32_groups(&sloppy, 32).unwrap(), bytes);
        assert!(from_base32_groups("U", 32).is_none());
    }

    #[test]
    fn deflate_round_trip() {
        let data = b"hello hello hello hello".repeat(10);
        assert_eq!(inflate(&deflate(&data)).unwrap(), data);
    }
}

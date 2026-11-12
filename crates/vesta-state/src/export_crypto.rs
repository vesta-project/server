//! Fixed, versioned Argon2id / ChaCha20-Poly1305 STREAM envelope.
use crate::{Result, StateError};
use argon2::{Algorithm, Argon2, Block, Params, Version};
use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey};
use std::io::{Read, Write};
use zeroize::{Zeroize, Zeroizing};

pub(super) const MAGIC: &[u8; 8] = b"VESTAEX1";
const HEADER_LEN: usize = 48;
const SEGMENT: usize = 1024 * 1024;
const TAG_LEN: usize = 16;
const MEMORY_KIB: u32 = 65_536;
const ITERATIONS: u32 = 3;
const PARALLELISM: u32 = 1;
pub(super) const MAX_BYTES: usize = 256 * 1024 * 1024;
pub(super) const MAX_PASSPHRASE: usize = 4096;

fn invalid() -> StateError {
    StateError::new("state_export_decrypt_failed")
}
fn derive(passphrase: &[u8], salt: &[u8]) -> Result<LessSafeKey> {
    if passphrase.is_empty() || passphrase.len() > MAX_PASSPHRASE {
        return Err(StateError::new("state_export_passphrase_invalid"));
    }
    let params = Params::new(MEMORY_KIB, ITERATIONS, PARALLELISM, Some(32))
        .map_err(|_| StateError::new("state_export_crypto_failed"))?;
    let mut memory = Zeroizing::new(Vec::<Block>::new());
    memory
        .try_reserve_exact(params.block_count())
        .map_err(|_| StateError::new("state_export_crypto_failed"))?;
    memory.resize_with(params.block_count(), Block::default);
    let mut key = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into_with_memory(passphrase, salt, &mut key[..], &mut memory[..])
        .map_err(|_| StateError::new("state_export_crypto_failed"))?;
    let key = UnboundKey::new(&aead::CHACHA20_POLY1305, &key[..])
        .map_err(|_| StateError::new("state_export_crypto_failed"))?;
    Ok(LessSafeKey::new(key))
}
fn nonce(header: &[u8; HEADER_LEN], counter: u32, last: bool) -> Nonce {
    let mut bytes = [0u8; 12];
    bytes[..7].copy_from_slice(&header[37..44]);
    bytes[7..11].copy_from_slice(&counter.to_be_bytes());
    bytes[11] = u8::from(last);
    Nonce::assume_unique_for_key(bytes)
}
fn header() -> Result<[u8; HEADER_LEN]> {
    let mut header = [0; HEADER_LEN];
    header[..8].copy_from_slice(MAGIC);
    header[8] = 1;
    header[9..13].copy_from_slice(&MEMORY_KIB.to_be_bytes());
    header[13..17].copy_from_slice(&ITERATIONS.to_be_bytes());
    header[17..21].copy_from_slice(&PARALLELISM.to_be_bytes());
    getrandom::fill(&mut header[21..44])
        .map_err(|_| StateError::new("state_export_crypto_failed"))?;
    header[44..48].copy_from_slice(&(SEGMENT as u32).to_be_bytes());
    Ok(header)
}
fn check_header(header: &[u8; HEADER_LEN]) -> Result<()> {
    if &header[..8] != MAGIC
        || header[8] != 1
        || header[9..13] != MEMORY_KIB.to_be_bytes()
        || header[13..17] != ITERATIONS.to_be_bytes()
        || header[17..21] != PARALLELISM.to_be_bytes()
        || header[44..48] != (SEGMENT as u32).to_be_bytes()
    {
        return Err(invalid());
    }
    Ok(())
}

pub(super) struct EncryptWriter<'a, W: Write> {
    out: &'a mut W,
    header: [u8; HEADER_LEN],
    key: LessSafeKey,
    segment: Zeroizing<Vec<u8>>,
    counter: u32,
    total: usize,
}
impl<'a, W: Write> EncryptWriter<'a, W> {
    pub(super) fn new(out: &'a mut W, passphrase: &[u8]) -> Result<Self> {
        let header = header()?;
        let key = derive(passphrase, &header[21..37])?;
        out.write_all(&header)?;
        let mut segment = Zeroizing::new(Vec::new());
        segment
            .try_reserve_exact(SEGMENT + TAG_LEN)
            .map_err(|_| StateError::new("state_export_crypto_failed"))?;
        Ok(Self {
            out,
            header,
            key,
            segment,
            counter: 0,
            total: 0,
        })
    }
    fn emit(&mut self, last: bool) -> std::io::Result<()> {
        let length = self.segment.len() as u32;
        self.key
            .seal_in_place_append_tag(
                nonce(&self.header, self.counter, last),
                Aad::from(&self.header),
                &mut *self.segment,
            )
            .map_err(|_| std::io::Error::other("export encryption failed"))?;
        self.out.write_all(&length.to_be_bytes())?;
        self.out.write_all(&[u8::from(last)])?;
        self.out.write_all(&self.segment)?;
        self.segment.zeroize();
        self.segment.clear();
        self.counter = self
            .counter
            .checked_add(1)
            .ok_or_else(|| std::io::Error::other("export segment limit"))?;
        Ok(())
    }
    pub(super) fn finish(mut self) -> Result<()> {
        self.emit(true)?;
        self.out.flush()?;
        Ok(())
    }
}
impl<W: Write> Write for EncryptWriter<'_, W> {
    fn write(&mut self, mut bytes: &[u8]) -> std::io::Result<usize> {
        let count = bytes.len();
        self.total = self
            .total
            .checked_add(count)
            .filter(|n| *n <= MAX_BYTES)
            .ok_or_else(|| std::io::Error::other("export size limit"))?;
        while !bytes.is_empty() {
            if self.segment.len() == SEGMENT {
                self.emit(false)?;
            }
            let length = bytes.len().min(SEGMENT - self.segment.len());
            self.segment.extend_from_slice(&bytes[..length]);
            bytes = &bytes[length..];
        }
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.out.flush()
    }
}

pub(super) fn decrypt(
    input: &mut impl Read,
    prefix: &[u8; 8],
    passphrase: Option<&[u8]>,
) -> Result<Zeroizing<Vec<u8>>> {
    let passphrase =
        passphrase.ok_or_else(|| StateError::new("state_export_passphrase_required"))?;
    let mut header = [0u8; HEADER_LEN];
    header[..8].copy_from_slice(prefix);
    input.read_exact(&mut header[8..]).map_err(|_| invalid())?;
    check_header(&header)?;
    let key = derive(passphrase, &header[21..37])?;
    let mut output = Zeroizing::new(Vec::new());
    let mut counter = 0u32;
    loop {
        let mut frame = [0u8; 5];
        input.read_exact(&mut frame).map_err(|_| invalid())?;
        let length = u32::from_be_bytes(frame[..4].try_into().map_err(|_| invalid())?) as usize;
        let last = match frame[4] {
            0 => false,
            1 => true,
            _ => return Err(invalid()),
        };
        if length > SEGMENT
            || (!last && length != SEGMENT)
            || (length == 0 && counter != 0)
            || output
                .len()
                .checked_add(length)
                .is_none_or(|n| n > MAX_BYTES)
        {
            return Err(invalid());
        }
        let mut segment = Zeroizing::new(vec![0u8; length + TAG_LEN]);
        input.read_exact(&mut segment).map_err(|_| invalid())?;
        let plaintext = key
            .open_in_place(
                nonce(&header, counter, last),
                Aad::from(&header),
                &mut segment,
            )
            .map_err(|_| invalid())?;
        output
            .try_reserve_exact(plaintext.len())
            .map_err(|_| StateError::new("state_export_size_limit"))?;
        output.extend_from_slice(plaintext);
        if last {
            let mut trailing = [0];
            if input.read(&mut trailing).map_err(|_| invalid())? != 0 {
                return Err(invalid());
            }
            return Ok(output);
        }
        counter = counter.checked_add(1).ok_or_else(invalid)?;
    }
}

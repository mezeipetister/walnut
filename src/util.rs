use std::time::{self, SystemTime};

use crc32fast::Hasher;

use crate::BLOCK_SIZE;

/// Create 32bit checksums
/// Wrapper struct around crc32fast hasher
pub struct Checksum {
    hasher: Hasher,
}

impl Checksum {
    #[inline]
    pub fn new() -> Self {
        Self {
            hasher: crc32fast::Hasher::new(),
        }
    }

    #[inline]
    pub fn update(&mut self, bytes: &[u8]) {
        self.hasher.update(bytes);
    }

    #[inline]
    pub fn finalize(self) -> u32 {
        self.hasher.finalize()
    }
}

// Calculate checksum for small objects
// For large objects please use Checksum directly
#[inline]
pub fn calculate_checksum<S>(s: &S) -> u32
where
    S: serde::Serialize,
{
    let mut hasher = Checksum::new();
    hasher.update(&bincode::serialize(&s).unwrap());
    hasher.finalize()
}

#[inline]
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[inline]
pub fn block_seek_position(block_index: u32) -> u32 {
    block_index * BLOCK_SIZE
}

#[inline]
pub fn encrypt(bytes: &mut [u8], lookup_table: &Vec<u8>) {
    // let len = secret.len();
    // for (index, byte) in bytes.iter_mut().enumerate() {
    //     let i = index & (len - 1);
    //     // byte.bitxor_assign(secret[i]);
    //     unsafe {
    //         *byte ^= secret.get_unchecked(i);
    //     }
    // }
    bytes
        .iter_mut()
        .zip(lookup_table)
        .for_each(|(byte, secret)| *byte ^= secret);
}

#[inline]
pub fn create_lookup_table(secret: &[u8], block_size: u32) -> Vec<u8> {
    // let mut res: Vec<u8> = Vec::with_capacity(block_size as usize);
    // unsafe { res.set_len(block_size as usize) };

    (0..block_size)
        .into_iter()
        .map(|i| secret[i as usize & (secret.len() - 1)])
        .collect()
}

//! Storage

use crc::Digest;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use esp_mmu::{DBUS_BASE, IBUS_BASE, Mmu, Region, VirtualAddress};
use esp_storage::{FlashStorage, FlashStorageError};

use crate::metadata::{CRC, MAGIC, Metadata, VERSION};
use crate::partitions::PartitionLayout;

/// A loaded WASM module
#[derive(Debug)]
pub struct WasmModule<'a, R: RawMutex + 'static> {
    /// Region storing the WASM module
    storage_region: Region<'a, R>,
    /// Length in bytes of the stored WASM module
    len: usize,
}

impl<R: RawMutex> WasmModule<'_, R> {
    /// Return the WASM module as an AOT executable slice
    ///
    /// This can be fed into a WASM runtime as a slice and will thus avoid to use any RAM
    #[expect(
        clippy::missing_panics_doc,
        reason = "The only panic is when the logic faults, not user"
    )]
    #[must_use]
    pub fn slice(&self) -> &[u8] {
        #[expect(
            clippy::unwrap_used,
            reason = "Safe to do so, because we already validated it when we loaded it"
        )]
        self.storage_region.as_slice().get(..self.len).unwrap()
    }
}

/// WASM Module Storage
pub struct WasmStorage<R: RawMutex> {
    /// Access to the MMU peripheral in order to perform map/unmap operations
    mmu: Mutex<R, Mmu>,
    /// Access to the FLASH peripheral in order to write/erase WASM modules
    flash: Mutex<R, FlashStorage<'static>>,
    /// Physical/virtual placement of the AOT metadata and XIP regions in Flash
    layout: PartitionLayout,
}

impl<R: RawMutex> core::fmt::Debug for WasmStorage<R> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("WasmStorage").finish_non_exhaustive()
    }
}

impl<R: RawMutex> WasmStorage<R> {
    /// Creates a WASM storage with the given AOT flash [`PartitionLayout`].
    #[must_use]
    pub fn new(mmu: Mmu, flash: FlashStorage<'static>, layout: PartitionLayout) -> Self {
        Self {
            mmu: Mutex::new(mmu),
            flash: Mutex::new(flash),
            layout,
        }
    }

    /// Mounts regions if unmounted
    ///
    /// If already mounted, this operation is NOP
    pub(crate) fn mount_regions(&self) -> (Region<'_, R>, Region<'_, R>) {
        // Mount metadata
        #[expect(clippy::expect_used, reason = "Unrecoverable")]
        let metadata = Region::new(
            &self.mmu,
            VirtualAddress::Data(DBUS_BASE | self.layout.meta_vaddr_offset()),
            self.layout.meta_paddr,
            self.layout.meta_len,
        )
        .expect("Faulty partition logic");

        // Mount storage
        #[expect(clippy::expect_used, reason = "Unrecoverable")]
        let storage = Region::new(
            &self.mmu,
            VirtualAddress::Instruction(IBUS_BASE | self.layout.xip_vaddr_offset()),
            self.layout.xip_paddr,
            self.layout.xip_len,
        )
        .expect("Faulty partition logic");

        (metadata, storage)
    }

    /// Loads the WASM module if one is stored
    #[must_use]
    pub fn load(&mut self) -> Option<(Metadata, WasmModule<'_, R>)> {
        // Map Metadata so that we can parse the metadata without needing RAM loading
        let (metadata_region, storage_region) = self.mount_regions();

        // Try to find Metadata. If found parse it and validate it. Any failure will erase both
        // metadata and storage partitions if needed.

        // Parse Metadata
        let Ok(meta) = postcard::from_bytes::<Metadata>(metadata_region.as_slice()) else {
            // Failed to parse metadata (maybe not found)
            self.erase(metadata_region, storage_region);

            return None;
        };

        // Validate Metadata
        log::info!("Found WASM Module metadata: {meta:?}");
        if meta.magic != MAGIC || meta.version != VERSION || meta.len as usize > self.layout.xip_len
        {
            // Invalid Metadata
            self.erase(metadata_region, storage_region);

            return None;
        }
        log::info!("WASM metadata valid");

        let crc = CRC.checksum(storage_region.as_slice().get(..meta.len as usize)?);
        if meta.crc != crc {
            log::warn!(
                "WASM module crc check failed. Expected {crc:#0X}, found {:#0X}",
                meta.crc
            );

            self.erase(metadata_region, storage_region);

            return None;
        }

        let len = meta.len as usize;
        Some((
            meta,
            WasmModule {
                storage_region,
                len,
            },
        ))
    }

    /// Erases a WASM Module
    #[expect(
        clippy::cast_possible_truncation,
        reason = "We are running only on 32-bit targets"
    )]
    pub(crate) fn erase(&self, metadata_region: Region<'_, R>, storage_region: Region<'_, R>) {
        // Blank check and erase if necessary
        // Metadata is missing or invalid. If invalid (non-blank), erase so it can be written in
        // the future
        let blank = !metadata_region.as_slice().iter().any(|b| *b != 0xFF);
        if !blank {
            let (start, end) = (
                metadata_region.paddr,
                metadata_region.paddr + metadata_region.size,
            );
            // Discard region (ensures that any held slice is unused)
            core::mem::drop(metadata_region);
            log::trace!("erase from:{start:#0X} to:{end:#0X}");
            // safety: This is not called re-entrantly
            unsafe {
                #[expect(clippy::expect_used, reason = "Unrecoverable")]
                self.flash
                    .lock_mut(|flash| flash.erase(start as u32, end as u32))
                    .expect("Flawed logic in Region");
            }
        }

        // Make sure that the storage is erased too
        let blank = !storage_region.as_slice().iter().any(|b| *b != 0xFF);
        if !blank {
            let (start, end) = (
                storage_region.paddr,
                storage_region.paddr + storage_region.size,
            );
            // Discard region (ensures that any held slice is unused)
            core::mem::drop(storage_region);
            log::trace!("erase from:{start:#0X} to:{end:#0X}");
            // safety: This is not called re-entrantly
            unsafe {
                #[expect(clippy::expect_used, reason = "Unrecoverable")]
                self.flash
                    .lock_mut(|flash| flash.erase(start as u32, end as u32))
                    .expect("Flawed logic in Region");
            }
        }
    }

    /// Create a writer that can be used to write a WASM module into Flash
    pub fn writer(&mut self, expected_metadata: Metadata) -> WasmWriter<'_, R> {
        // Make sure first that everything is erased
        let (metadata_region, storage_region) = self.mount_regions();
        self.erase(metadata_region, storage_region);

        // Then create the writer (passing a reference to self so we are sure that in the meantime
        // the borrow checker will block any concurrent load attempts)
        let current_offset = self.layout.xip_paddr;
        WasmWriter {
            storage: self,
            crc_digest: CRC.digest(),
            current_offset,
            finished: false,
            expected_metadata,
        }
    }
}

/// A writer that can write a WASM module to Flash
///
/// First write all the module data
/// ```no_run
/// writer.write(&[..]).unwrap();
/// writer.write(&[..]).unwrap();
/// writer.write(&[..]).unwrap();
/// ...
/// ```
/// Then finalize by calling `done`
/// ```no_run
/// ...
/// writer.write(&[..]).unwrap();
/// writer.done().unwrap();
/// // Now the WASM module is stored in Flash
/// ```
pub struct WasmWriter<'a, R: RawMutex + 'static> {
    /// Reference to the WASM storage that can be used for erasing if writer is dropped, or to
    /// prevent concurrent conflicting storage operations while this writer is being used
    storage: &'a mut WasmStorage<R>,
    /// Running digest used to calculate final checksum for the metadata
    crc_digest: Digest<'a, u32>,
    /// Current physical address offset of the writer
    current_offset: usize,
    /// Whether the writer has been finalized
    finished: bool,
    /// Metadata that we expect to be valid at the end of the writing process. This is used to ensure
    /// that the written data matches what's expected (e.g. in terms of length and checksum)
    expected_metadata: Metadata,
}

impl<R: RawMutex> core::fmt::Debug for WasmWriter<'_, R> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("WasmWriter")
            .field("current_offset", &self.current_offset)
            .field("finished", &self.finished)
            .field("expected_metadata", &self.expected_metadata)
            .finish_non_exhaustive()
    }
}

impl<R: RawMutex> WasmWriter<'_, R> {
    /// Writes a chunk of the WASM module to the WASM storage
    ///
    /// It's important that the data is aligned to the Flash [`WORD_SIZE`] or the function will
    /// error out.
    ///
    /// Call `final_write()` after writing all chunks to commit the WASM module to storage.
    ///
    /// # Errors
    ///
    /// Returns an error if writing to Flash failed.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "We are running only on 32-bit targets"
    )]
    pub fn write(&mut self, data: &[u8]) -> Result<(), FlashStorageError> {
        // safety: Not calling re-entrantly
        unsafe {
            self.storage
                .flash
                .lock_mut(|flash| flash.write(self.current_offset as u32, data))?;
        }
        self.crc_digest.update(data);
        self.current_offset += data.len();

        Ok(())
    }

    /// Writes a chunk of the WASM module to the WASM storage, but take into consideration padding
    /// (if needed), for checksum purposes.
    ///
    /// # Note
    ///
    /// This should be called only from [`final_write`] as using this in a write that is not final,
    /// would mess up the data because of the potentially added padding.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "We are running only on 32-bit targets"
    )]
    fn write_with_padding(&mut self, data: &[u8]) -> Result<(), FlashStorageError> {
        // First calculate CRC (or we may accidentally include the padding in the CRC calculation
        self.crc_digest.update(data);

        let mut data = data.to_vec();
        let unpadded_len = data.len();
        if !data.len().is_multiple_of(FlashStorage::WORD_SIZE as usize) {
            // Needs padding
            let to_pad = FlashStorage::WORD_SIZE - (data.len() as u32 % FlashStorage::WORD_SIZE);
            data.extend(core::iter::repeat_n(0xFF, to_pad as usize));
        }
        // safety: Not calling re-entrantly
        unsafe {
            self.storage
                .flash
                .lock_mut(|flash| flash.write(self.current_offset as u32, data.as_slice()))?;
        }
        self.current_offset += unpadded_len;

        Ok(())
    }

    /// Commit the data that was so far written to the Flash storage
    ///
    /// # Errors
    ///
    /// Returns a [`FlashStorageError`] if the committing to flash failed.
    #[expect(
        clippy::missing_panics_doc,
        reason = "Panics here are logic faults, not user facing"
    )]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "We are running only on 32-bit targets"
    )]
    pub fn final_write(mut self, data: &[u8]) -> Result<(), FlashStorageError> {
        self.write_with_padding(data)?;

        // Check that the generated metadata for what we have written matches what we expected
        let metadata = Metadata {
            magic: MAGIC,
            version: VERSION,
            len: (self.current_offset - self.storage.layout.xip_paddr) as u32,
            crc: self.crc_digest.clone().finalize(),
            hash: [0u8; 32], // Not to be compared
        };
        if metadata.magic != self.expected_metadata.magic
            || metadata.version != self.expected_metadata.version
            || metadata.len != self.expected_metadata.len
            || metadata.crc != self.expected_metadata.crc
        {
            log::error!(
                "Metadata mismatch. Expected {:?}, got {metadata:?}",
                self.expected_metadata
            );

            return Err(FlashStorageError::IoError);
        }

        #[expect(
            clippy::unwrap_in_result,
            clippy::expect_used,
            reason = "If we can't serialize the metadata, then we can't operate the flash driver at \
            all because of a programming logic error. Better to panic and let the developer know"
        )]
        let mut serialized =
            postcard::to_allocvec(&self.expected_metadata).expect("Failed to serialize metadata");
        // Pad serialized metadata if necessary
        if !serialized
            .len()
            .is_multiple_of(FlashStorage::WORD_SIZE as usize)
        {
            let to_pad =
                FlashStorage::WORD_SIZE - (serialized.len() as u32 % FlashStorage::WORD_SIZE);
            serialized.extend(core::iter::repeat_n(0xFF, to_pad as usize));
        }

        let meta_paddr = self.storage.layout.meta_paddr;
        // safety: Not calling re-entrantly
        unsafe {
            self.storage.flash.lock_mut(|flash| {
                flash.write(meta_paddr as u32, serialized.as_slice())?;

                Ok(())
            })?;
        }
        self.finished = true;

        Ok(())
    }
}

// Ensures clean slate if user forgets to call final_write()
impl<R: RawMutex> Drop for WasmWriter<'_, R> {
    fn drop(&mut self) {
        // Make sure we keep a clean slate if something goes wrong
        if !self.finished {
            let (metadata, storage) = self.storage.mount_regions();
            self.storage.erase(metadata, storage);
        }
    }
}

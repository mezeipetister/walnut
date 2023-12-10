use anyhow::anyhow;
use bitvec::{order::Lsb0, vec::BitVec};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Cursor, Seek, SeekFrom};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{Read, Write},
    path::Path,
};

use util::*;

const MAGIC: [u8; 7] = *b"*bitfs*";
// const TEST_BYTES: [u8; 20] = *b"canureadthistextbro?";
const FS_VERSION: u32 = 1;
const ROOT_INODE_INDEX: u32 = 2;
const BLOCK_SIZE: u32 = 4096;
const BLOCKS_PER_GROUP: u32 = BLOCK_SIZE * 8;
const INODE_CAPACITY: usize = 4047;
const INODE_MAX_REGION: usize = 500;

pub mod util;

#[derive(Debug)]
pub struct FS {
    pub superblock: Superblock,
    pub file: File,
    pub groups: Vec<Group>,
    pub lookup_table: Vec<u8>,
}

impl FS {
    /// Init FS to a given path
    pub fn init<P>(path: P, secret: &str) -> anyhow::Result<Self>
    where
        P: AsRef<Path>,
    {
        // Create path if it has not exist (yet)
        // Fails if path does exist
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path.as_ref())?;

        // Create mmap from file
        // let mmap = unsafe { MmapMut::map_mut(&file)? };

        let superblock = Superblock::new();

        let mut fs = Self {
            superblock,
            file,
            groups: vec![],
            lookup_table: create_lookup_table(secret.as_bytes(), BLOCK_SIZE),
        };

        // Create group
        let mut group = Group::init();

        // Set root inode index as allocated
        group.force_allocate_at(0);

        // Add to superblock
        fs.add_group(group)?;

        // Create directory_index
        fs.init_directory_index()?;

        Ok(fs)
    }

    /// Open FS from a given path
    pub fn new<P>(path: P, secret: &str) -> anyhow::Result<Self>
    where
        P: AsRef<Path>,
    {
        // Open image path as read & write
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path.as_ref())?;

        let mut r = BufReader::new(&mut file);

        r.seek(SeekFrom::Start(0))?;

        // Deserialize superblock from cursor
        let superblock: Superblock = Superblock::deserialize_from(&mut r)?;

        let mut groups = vec![];

        // Deserialize groups based on superblock group count
        for group_index in 0..superblock.group_count {
            let group = Group::deserialize_from(&mut r, group_index)?;
            groups.push(group);
        }

        let fs = Self {
            superblock,
            groups,
            file,
            lookup_table: create_lookup_table(secret.as_bytes(), BLOCK_SIZE),
        };

        // Return FS
        Ok(fs)
    }

    #[inline]
    pub fn get_directory_index(&self) -> anyhow::Result<DirectoryIndex> {
        // Get inode
        let mut inode = self.get_inode(ROOT_INODE_INDEX)?;

        // Read inode data
        let mut data = vec![];

        {
            let mut w = BufWriter::new(&mut data);
            self.read_inode_data(&mut inode, &mut w)?;
        }

        // Deserialize
        let mut directory_index: DirectoryIndex = bincode::deserialize(&data)?;

        if !directory_index.verify_checksum() {
            return Err(anyhow!("Directory index checksum error"));
        }

        Ok(directory_index)
    }

    fn save_directory_index(&mut self, mut directory_index: DirectoryIndex) -> anyhow::Result<()> {
        let mut inode = self.get_inode(ROOT_INODE_INDEX)?;

        // Set checksum
        directory_index.checksum();

        let data = bincode::serialize(&directory_index)?;
        let mut w = Cursor::new(&data);

        // Save directory
        self.write_inode_data(&mut inode, &mut w, data.len() as u64)?;

        Ok(())
    }

    fn init_directory_index(&mut self) -> anyhow::Result<()> {
        let di = DirectoryIndex::init();

        let di_data = bincode::serialize(&di)?;
        let mut r = Cursor::new(&di_data);

        let mut directory_index_inode = Inode::new(ROOT_INODE_INDEX);

        self.save_inode(&mut directory_index_inode)?;

        self.write_inode_data(&mut directory_index_inode, &mut r, di_data.len() as u64)?;

        Ok(())
    }

    /// Find directory
    /// returns directory and its inode index
    #[inline]
    pub fn find_directory<P>(&self, dir: P) -> anyhow::Result<(Directory, u32)>
    where
        P: AsRef<Path>,
    {
        // First get directory index
        let directory_index = self.get_directory_index()?;

        // Find directory in dir.index
        if let Some(directory_inode_index) = directory_index.find_dir(dir) {
            // Get directory inode
            let mut directory_inode = self.get_inode(*directory_inode_index)?;

            let mut data = Vec::new();

            // Read inode data
            {
                let mut w = BufWriter::new(&mut data);
                self.read_inode_data(&mut directory_inode, &mut w)?;
            }

            // Deserialize directory
            let directory: Directory = bincode::deserialize(&data)?;

            // Return it
            return Ok((directory, *directory_inode_index));
        } else {
            return Err(anyhow!("Directory not found"));
        }
    }

    #[inline]
    fn save_directory(
        &mut self,
        directory: Directory,
        directory_inode_index: u32,
    ) -> anyhow::Result<Directory> {
        // Get directory inode
        let mut directory_inode = self.get_inode(directory_inode_index)?;

        // Serialize directory
        let data = bincode::serialize(&directory)?;
        let mut reader = Cursor::new(&data);

        self.write_inode_data(&mut directory_inode, &mut reader, data.len() as u64)?;

        Ok(directory)
    }

    /// Create directory
    /// returns created directory
    #[inline]
    pub fn create_directory<P>(&mut self, dir: P) -> anyhow::Result<Directory>
    where
        P: AsRef<Path>,
    {
        // First get directory index
        let mut directory_index = self.get_directory_index()?;

        // Then allocate dir inode index
        let directory_inode = if let Some(i) = self.allocate_inode() {
            i
        } else {
            return Err(anyhow!("Could not allocate inode block"));
        };

        // Then try to add directory to dir index
        // If it fails, then free up allocated block
        if let None = directory_index.create_dir(dir, directory_inode.block_index) {
            self.release_inode(directory_inode.block_index)?;
        }

        // Save directory index
        self.save_directory_index(directory_index)?;

        // Create empty directory
        let directory = Directory::init();

        // Try to save directory
        self.save_directory(directory, directory_inode.block_index)
    }

    /// Get file by dir and filename
    /// returns found file inode
    #[inline]
    pub fn get_file_info<P>(&mut self, dir: P, file_name: &str) -> anyhow::Result<Inode>
    where
        P: AsRef<Path>,
    {
        // Check if dir exist
        let (dir, _dir_inode_index) = self.find_directory(dir)?;

        // Find file
        if let Some(inode_block_index) = dir.get_file(file_name) {
            return self.get_inode(inode_block_index);
        } else {
            return Err(anyhow!("File not found"));
        }
    }

    /// Create a file at a given dir
    /// with a given name
    /// Copy data to the given file
    /// data_len (bytes) must be correct
    #[inline]
    pub fn add_file<P, R>(
        &mut self,
        dir: P,
        file_name: &str,
        data: &mut R,
        data_len: u64,
    ) -> anyhow::Result<()>
    where
        P: AsRef<Path>,
        R: BufRead,
    {
        // Check if dir exist
        let (mut dir, dir_inode_index) = self.find_directory(dir)?;

        // Find file
        let mut file_inode = if let Some(inode_block_index) = dir.get_file(file_name) {
            self.get_inode(inode_block_index)?
        } else {
            let file_inode = self.allocate_inode().unwrap();
            dir.add_file(file_name, file_inode.block_index)?;
            self.save_directory(dir, dir_inode_index)?;

            // Inc. file count
            self.superblock_mut().file_count += 1;

            file_inode
        };

        self.write_inode_data(&mut file_inode, data, data_len)?;

        // Save superblock
        self.save_superblock()?;

        Ok(())
    }

    #[inline]
    pub fn remove_file(&mut self, dir: &str, file_name: &str) -> anyhow::Result<()> {
        // Check if dir exist
        let (mut dir, dir_inode_index) = self.find_directory(dir)?;

        // Find file
        let file_inode = if let Some(inode_block_index) = dir.get_file(file_name) {
            if let Ok(inode) = self.get_inode(inode_block_index) {
                inode
            } else {
                return Err(anyhow!("No file found in dir!"));
            }
        } else {
            return Err(anyhow!("Unknown directory!"));
        };

        // Release inode
        self.release_inode(file_inode.block_index)?;

        // Remove file from directory
        dir.remove_file(file_name)?;

        // Save directory
        self.save_directory(dir, dir_inode_index)?;

        // Save superblock
        self.save_superblock()?;

        Ok(())
    }

    /// Read file data
    /// Finds file by dir and filename
    /// And writes its content to the given writer
    #[inline]
    pub fn get_file_data<P, W>(&mut self, dir: P, file_name: &str, w: &mut W) -> anyhow::Result<u32>
    where
        P: AsRef<Path>,
        W: Write,
    {
        // First find directory
        let (directory, _) = self.find_directory(dir)?;

        // Then find file
        let mut file_inode = if let Some(file_inode_index) = directory.get_file(file_name) {
            self.get_inode(file_inode_index)?
        } else {
            // Else return error
            return Err(anyhow!("File not found"));
        };

        self.read_inode_data(&mut file_inode, w)
    }

    #[inline]
    fn superblock_check(&mut self) {
        // Set group count
        self.superblock.group_count = self.groups.len() as u32;
        // Set free blocks
        self.superblock.free_blocks = self
            .groups
            .iter()
            .map(|g| g.block_bitmap.count_zeros() as u32)
            .sum();
        // Set block count
        self.superblock.block_count = self
            .groups
            .iter()
            .map(|g| g.total_data_blocks() as u32)
            .sum();
        // Set last modified time
        self.superblock.modified = now();
        // Set checksum
        self.superblock.checksum();
    }

    #[inline]
    fn save_superblock(&mut self) -> anyhow::Result<()> {
        // Create superblock checks
        self.superblock_check();

        let mut w = BufWriter::new(&self.file);
        let mut data = bincode::serialize(&self.superblock)?;
        w.seek(SeekFrom::Start(0))?;
        w.write_all(&mut data)?;
        Ok(())
    }

    #[inline]
    fn get_inode(&self, inode_block_index: u32) -> anyhow::Result<Inode> {
        let mut r = BufReader::new(&self.file);

        r.seek(SeekFrom::Start(
            block_seek_position(inode_block_index) as u64
        ))?;

        // Deserialize by bincode
        let inode: Inode = Inode::deserialize_from(r)?;

        // Return inode
        Ok(inode)
    }

    #[inline]
    fn save_inode(&mut self, inode: &mut Inode) -> anyhow::Result<()> {
        let mut w = BufWriter::new(&self.file);

        w.seek(SeekFrom::Start(
            block_seek_position(inode.block_index) as u64
        ))?;
        inode.set_last_modified();
        inode.serialize_into(w)?;
        Ok(())
    }

    #[inline]
    fn save_group(&mut self, group: Group, group_index: u32) -> anyhow::Result<()> {
        // Update group at FS
        self.groups[group_index as usize] = group.clone();

        // Write group to disk
        let mut w = BufWriter::new(&self.file);

        w.seek(SeekFrom::Start(Group::seek_position(group_index) as u64))?;
        group.serialize_into(w)?;
        Ok(())
    }

    #[inline]
    fn read_inode_data<W>(&self, inode: &mut Inode, w: &mut W) -> anyhow::Result<u32>
    where
        W: Write,
    {
        let mut checksum = Checksum::new();
        let mut r = BufReader::new(&self.file);

        match &mut inode.data {
            Data::Raw(data) => {
                // Decrypt raw data
                encrypt(data, &self.lookup_table);

                // Update checksum
                checksum.update(&data);

                // Write data into writer
                w.write_all(&data)?;
            }
            Data::DirectPointers(pointers) => {
                // Counting data left to read
                let mut data_left = inode.size;

                let mut block_buffer: Vec<u8> = Vec::with_capacity(BLOCK_SIZE as usize);
                unsafe { block_buffer.set_len(BLOCK_SIZE as usize) };

                for (block_index, range) in pointers {
                    // Seek start position
                    r.seek(SeekFrom::Start(block_seek_position(*block_index) as u64))?;

                    for _ in *block_index..(*block_index + *range) {
                        // Determine if last block
                        if data_left < BLOCK_SIZE as u64 {
                            block_buffer = Vec::with_capacity(data_left as usize);
                            unsafe { block_buffer.set_len(data_left as usize) };
                        };

                        // Read range bytes
                        r.read_exact(&mut block_buffer)?;

                        // Decrypt chunk
                        encrypt(&mut block_buffer, &self.lookup_table);

                        // Update checksum
                        checksum.update(&block_buffer);

                        // Write buffer to writer
                        w.write_all(&mut block_buffer)?;
                        // std::io::copy(&mut BufReader::new(Cursor::new(&block_buffer)), &mut w)?;

                        // Decrease data_left
                        data_left -= block_buffer.capacity() as u64;
                    }
                }
            }
        }

        Ok(checksum.finalize())
    }

    #[inline]
    fn write_inode_data<R>(
        &mut self,
        inode: &mut Inode,
        data: &mut R,
        data_len: u64,
    ) -> anyhow::Result<()>
    where
        R: BufRead,
    {
        // Release inode data
        match &inode.data {
            Data::Raw(_) => (),
            Data::DirectPointers(pointers) => self.release_inode_data(pointers.clone())?,
        }

        // If data length fits inside inode
        if data_len as usize <= INODE_CAPACITY {
            // Create buffer
            let mut buffer = vec![];

            // and read data into it
            data.read_to_end(&mut buffer)?;

            // Encrypt buffer
            encrypt(&mut buffer, &self.lookup_table);

            // Create reader from buffer
            let mut data = Cursor::new(&buffer);

            // Set data inside inode
            inode.set_raw_data(&mut data, data_len)?;

            // Save inode
            self.save_inode(inode)?;

            // Return ok
            return Ok(());
        }

        // If data does not fit inside Inode as raw data

        // Set inode data size
        inode.size = data_len;
        // And save it
        self.save_inode(inode)?;

        // Define empty ranges
        let mut ranges: Vec<(u32, u32)> = vec![];

        // Define block_to_allocate
        let blocks_to_allocate = |data_size| {
            data_size / BLOCK_SIZE as u64 + u64::from(data_size % BLOCK_SIZE as u64 != 0)
        };

        // Determine how many block we need
        let mut block_to_allocate = blocks_to_allocate(data_len);

        // Check if we have enough space for file
        while self.superblock().free_blocks < block_to_allocate as u32 {
            // Add new group
            self.add_group(Group::init())?;
        }

        let groups = self.groups.clone();

        for (group_index, mut group) in groups.into_iter().enumerate() {
            // Check if we need any blocks?
            if block_to_allocate > 0 {
                // Allocate regions from group
                let (mut range, left) = group.allocate_region(
                    group_index as u32,
                    block_to_allocate as usize,
                    INODE_MAX_REGION,
                );

                // Save group
                self.save_group(group, group_index as u32)?;

                ranges.append(&mut range);

                // Decrease block wanted
                block_to_allocate = left as u64;
            }
        }

        // Save ranges
        inode.set_direct_pointers(ranges.clone(), data_len);
        self.save_inode(inode)?;

        // Write data into ranges
        let mut data_left = data_len;

        let mut w = BufWriter::new(&self.file);

        let mut block_buffer: Vec<u8> = Vec::with_capacity(BLOCK_SIZE as usize);
        unsafe { block_buffer.set_len(BLOCK_SIZE as usize) };

        for (block_index, range) in ranges {
            // Seek position
            w.seek(SeekFrom::Start(block_seek_position(block_index) as u64))?;

            // Iter over rage
            for _ in block_index..(block_index + range) {
                // Determine if last block
                if data_left < BLOCK_SIZE as u64 {
                    block_buffer = Vec::with_capacity(data_left as usize);
                    unsafe { block_buffer.set_len(data_left as usize) };
                };

                // Read data into chunk buffer
                data.read_exact(&mut block_buffer)?;

                // Encrypt chunk
                encrypt(&mut block_buffer, &self.lookup_table);

                // Write chunk buffer to disk
                w.write_all(&mut block_buffer)?;

                // Decrease data left
                data_left -= block_buffer.capacity() as u64;
            }
        }

        // Check all data has written
        assert!(data_left == 0);

        // Flush disk
        w.flush()?;

        Ok(())
    }

    #[inline]
    fn truncate(&mut self) -> anyhow::Result<()> {
        // Superblock + GroupCount * (Group bitmap + group data inodes)
        let size =
            BLOCK_SIZE + (self.groups.len() as u32) * (BLOCK_SIZE + BLOCKS_PER_GROUP * BLOCK_SIZE);
        // Set file size
        self.file.set_len(size as u64)?;
        // Return ok
        Ok(())
    }

    #[inline]
    fn allocate_inode(&mut self) -> Option<Inode> {
        // Check if we need more space
        // while self.superblock().free_blocks < 3 {
        //     self.add_group(Group::init()).unwrap();
        // }

        let mut res = None;
        for (group_index, group) in self.groups_mut().iter_mut().enumerate() {
            if let Some(inode_block_index) = group.allocate_one(group_index as u32) {
                let inode = Inode::new(inode_block_index);
                res = Some(inode);
                break;
            }
        }
        if let Some(inode) = &mut res {
            self.save_inode(inode).unwrap();
        }
        // TODO! Shoud handle the case when inode fails to save
        res
    }

    #[inline]
    fn add_group(&mut self, group: Group) -> anyhow::Result<()> {
        // Insert new group to FS groups
        self.groups.push(group.clone());
        // Save group to disk
        self.save_group(group, self.groups.len() as u32 - 1)?;
        // Increment group count
        self.superblock.group_count += 1;
        // Truncate itself
        self.truncate()?;
        // Save superblock
        self.save_superblock()?;
        // Return ok
        Ok(())
    }

    #[inline]
    fn groups_mut(&mut self) -> &mut [Group] {
        &mut self.groups
    }

    #[inline]
    fn superblock(&self) -> &Superblock {
        &self.superblock
    }

    #[inline]
    fn superblock_mut(&mut self) -> &mut Superblock {
        &mut self.superblock
    }

    #[inline]
    fn release_inode_data(&mut self, data_pointers: Vec<(u32, u32)>) -> anyhow::Result<()> {
        let mut groups = self.groups_mut().as_mut().to_owned();

        // Check each data region
        for (block_index, range) in data_pointers {
            // Translate public address
            let (group_index, bitmap_index) = Group::translate_public_address(block_index);
            // Release data region
            groups[group_index as usize].release_data_region(bitmap_index, range);
        }
        // Iter groups
        for (group_index, group) in groups.into_iter().enumerate() {
            {
                // And save each group to disk
                self.save_group(group, group_index as u32)?;
            }
        }
        Ok(())
    }

    #[inline]
    fn release_inode(&mut self, inode_block_index: u32) -> anyhow::Result<()> {
        // Check if inode exist
        let inode = self.get_inode(inode_block_index)?;

        // Translate block index
        let (group_index, bitmap_index) = Group::translate_public_address(inode_block_index);

        // Release data
        match inode.data {
            // Dont do anything when it has raw data
            Data::Raw(_) => (),
            // Release all direct pointers
            Data::DirectPointers(direct_pointers) => self.release_inode_data(direct_pointers)?,
        }

        let mut group = self.groups[group_index as usize].to_owned();

        {
            // Release index bitmap
            group.release_one(bitmap_index);
        }

        // Save group
        self.save_group(group, group_index)?;

        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Superblock {
    magic: [u8; 7],  // Magic number to check
    fs_version: u32, // FS Version
    // test_bytes: [u8; 20], // Secret test bytes
    block_size: u32,  // Block size in bytes
    group_count: u32, // Total groups count
    block_count: u32, // Total blocks count
    free_blocks: u32, // Available blocks
    file_count: u32,  // File count in fs
    created: u64,     // FS creation time
    modified: u64,    // FS last modification time
    checksum: u32,    // Superblock checksum
}

impl Superblock {
    fn new() -> Self {
        Self {
            magic: MAGIC,
            fs_version: FS_VERSION,
            block_size: BLOCK_SIZE,
            group_count: 0,
            block_count: 1,
            free_blocks: 0,
            file_count: 0,
            created: now(),
            modified: now(),
            checksum: 0,
        }
    }

    pub fn update_modified(&mut self) {
        self.modified = now();
    }

    #[allow(dead_code)]
    pub fn serialize(&mut self) -> anyhow::Result<Vec<u8>> {
        self.checksum();
        bincode::serialize(self).map_err(|e| e.into())
    }

    #[inline]
    pub fn serialize_into<W>(&mut self, w: W) -> anyhow::Result<()>
    where
        W: Write,
    {
        self.checksum();
        bincode::serialize_into(w, self).map_err(|e| e.into())
    }

    #[inline]
    pub fn deserialize_from<R>(r: R) -> anyhow::Result<Self>
    where
        R: Read,
    {
        let mut sb: Self = bincode::deserialize_from(r)?;
        if !sb.verify_checksum() {
            return Err(anyhow!("Superblock checksum verification failed"));
        }

        Ok(sb)
    }

    #[inline]
    fn checksum(&mut self) {
        self.checksum = 0;
        self.checksum = calculate_checksum(&self);
    }

    #[inline]
    fn verify_checksum(&mut self) -> bool {
        let checksum = self.checksum;
        self.checksum = 0;
        let ok = checksum == calculate_checksum(&self);
        self.checksum = checksum;

        ok
    }
}

#[derive(Debug, Default, Clone)]
pub struct Group {
    pub block_bitmap: BitVec<u8, Lsb0>,
}

impl Group {
    fn new(block_bitmap: BitVec<u8, Lsb0>) -> Self {
        Self { block_bitmap }
    }

    pub fn init() -> Self {
        let mut block_bitmap = BitVec::<u8, Lsb0>::with_capacity(BLOCK_SIZE as usize * 8);
        block_bitmap.resize(BLOCK_SIZE as usize * 8, false);
        Self { block_bitmap }
    }

    #[inline]
    fn seek_position(group_index: u32) -> u32 {
        // Superblock BLOCK_SIZE (4kib)
        // + Group ID * (BLOCK_SIZE + BLOCKS_PER_GROUP * BLOCK_SIZE)
        BLOCK_SIZE + (group_index * (BLOCK_SIZE + BLOCKS_PER_GROUP * BLOCK_SIZE))
    }

    #[inline]
    pub fn create_public_address(group_index: u32, bitmap_index: u32) -> u32 {
        // Maybe +1?
        Self::seek_position(group_index) / BLOCK_SIZE + bitmap_index + 1
    }

    /// Returns (group_index, bitmap_index)
    #[inline]
    pub fn translate_public_address(mut block_index: u32) -> (u32, u32) {
        block_index -= 1;
        let n = BLOCKS_PER_GROUP + 1;
        let group_index = (block_index as u32) / n;
        let bitmap_index = if group_index == 0 {
            block_index - 1
        } else {
            block_index % (group_index * n) - 1
        };
        (group_index, bitmap_index)
    }

    #[inline]
    pub fn serialize_into<W>(&self, mut w: W) -> anyhow::Result<()>
    where
        W: Write + Seek,
    {
        w.write_all(self.block_bitmap.as_raw_slice())?;

        Ok(())
    }

    #[inline]
    pub fn deserialize_from<R>(mut r: R, group_index: u32) -> anyhow::Result<Group>
    where
        R: Read + Seek,
    {
        let mut buf = Vec::with_capacity(BLOCK_SIZE as usize);
        unsafe {
            buf.set_len(BLOCK_SIZE as usize);
        }

        let offset = Self::seek_position(group_index);
        r.seek(SeekFrom::Start(offset as u64))?;
        r.read_exact(&mut buf)?;
        let data_bitmap = BitVec::<u8, Lsb0>::from_slice(&buf);

        Ok(Group::new(data_bitmap))
    }

    // #[inline]
    // pub fn has_data_block(&self, i: usize) -> bool {
    //     self.block_bitmap.get(i - 1).as_deref().unwrap_or(&false) == &true
    // }

    #[inline]
    pub fn free_data_blocks(&self) -> usize {
        self.block_bitmap.count_zeros()
    }

    #[inline]
    pub fn total_data_blocks(&self) -> usize {
        self.block_bitmap.len()
    }

    #[inline]
    fn release_one(&mut self, bitmap_index: u32) {
        self.block_bitmap.set(bitmap_index as usize, false);
    }

    #[inline]
    pub fn release_data_region(&mut self, bitmap_index: u32, length: u32) {
        for i in bitmap_index..(bitmap_index + length) {
            self.block_bitmap.set(i as usize, false);
        }
    }

    /// Set bitmap index by force
    #[inline]
    fn force_allocate_at(&mut self, bitmap_index: u32) {
        // Set it to be taken
        self.block_bitmap.set(bitmap_index as usize, true);
    }

    /// Allocate one block
    #[inline]
    fn allocate_one(&mut self, group_index: u32) -> Option<u32> {
        // If we have at least one free block index
        if let Some(bitmap_index) = self.block_bitmap.iter_zeros().next() {
            // Set it to be taken
            self.block_bitmap.set(bitmap_index, true);
            // Return index as public address
            return Some(Self::create_public_address(
                group_index,
                bitmap_index as u32,
            ));
        }
        None
    }

    /// Allocate data region
    #[inline]
    fn allocate_region(
        &mut self,
        // to translate internal ID into public address
        group_index: u32,
        // Blocks to allocate
        mut blocks_to_allocate: usize,
        // Maximum number of region to allocate
        max_regions: usize,
    ) -> (Vec<(u32, u32)>, usize) {
        let mut regions = Vec::new();
        let mut region: Option<(u32, u32)> = None;

        let mut iter = self.block_bitmap.iter_mut().enumerate().peekable();

        while let Some((bitmap_index, mut i)) = iter.next() {
            // Break loop if we dont need more blocks
            // to allocate
            if blocks_to_allocate == 0 {
                // Add opened region to regions if we have one opened
                if let Some(r) = region.take() {
                    regions.push(r);
                }
                break;
            }

            // If current block index is free
            if !*i {
                // If we have opened region
                if let Some((_block_index, region_length)) = region.as_mut() {
                    // Then increment region_length
                    *region_length += 1;
                } else {
                    // Else we need to create a new opened region
                    region = Some((
                        Self::create_public_address(group_index, bitmap_index as u32),
                        1,
                    ));
                }

                // Decrease blocks number to allocate by one
                // As we allocate on in this if block
                blocks_to_allocate -= 1;

                // Set block index as taken
                i.set(true);

                // If i is taken
            } else {
                // Check if we have opened region
                // and close it
                if let Some(r) = region.take() {
                    regions.push(r);

                    // Break loop if we reached the maximum region number
                    // we dont have room to allocate more regions
                    if regions.len() == max_regions {
                        break;
                    }
                }
            }

            // If last item, then clean up
            if let None = iter.peek() {
                // If we have opened region
                // then close it
                if let Some(r) = region.take() {
                    regions.push(r);
                }
            }
        }

        // allocated regions
        //  |                  remaining blocks to allocate
        //  |                     |
        (regions, blocks_to_allocate)
    }

    // #[inline]
    // fn next_free_data_region(&self, size: u32) -> Option<(usize, usize)> {
    //     self.block_bitmap
    //         .windows(size as usize)
    //         .position(|p| p.not_any())
    //         .map(|p| (p + 1, p + size as usize + 1))
    // }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Inode {
    pub block_index: u32,
    pub created: u64,
    pub last_modified: u64,
    pub size: u64,
    pub data_checksum: u32,
    pub data: Data,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Data {
    Raw(Vec<u8>),
    DirectPointers(Vec<(u32, u32)>),
}

impl Default for Data {
    fn default() -> Self {
        Self::Raw(vec![])
    }
}

impl Inode {
    pub fn new(block_index: u32) -> Self {
        Self {
            block_index,
            created: now(),
            last_modified: now(),
            size: 0,
            data_checksum: calculate_checksum(&()),
            data: Data::Raw(vec![]),
        }
    }

    #[inline]
    pub fn serialize_into<W>(&self, mut w: W) -> anyhow::Result<()>
    where
        W: Write + Seek,
    {
        // Serialize inode bytes array
        let serialized = bincode::serialize(&self)?;

        // Check if serialized inode size is correct
        assert!(serialized.len() as u32 <= BLOCK_SIZE);

        // Write serialized inode
        w.write_all(&serialized)?;

        // Flush buffer
        w.flush()?;

        Ok(())
    }

    #[inline]
    pub fn deserialize_from<R>(mut r: R) -> anyhow::Result<Self>
    where
        R: Read + Seek,
    {
        let inode: Inode = bincode::deserialize_from(&mut r)?;
        Ok(inode)
    }

    #[inline]
    fn set_last_modified(&mut self) {
        self.last_modified = now();
    }

    #[inline]
    fn set_raw_data<R>(&mut self, data: &mut R, data_size: u64) -> anyhow::Result<()>
    where
        R: Read,
    {
        let mut buffer = vec![];
        let data_len = data.read_to_end(&mut buffer)?;

        if data_len != data_size as usize {
            return Err(anyhow!("Data read and given data size are not the same"));
        }

        if data_len > INODE_CAPACITY as usize {
            return Err(anyhow!(
                "Data is too big to be raw data. Does not fit inside inode"
            ));
        }

        self.size = data_size;
        self.data = Data::Raw(buffer);
        Ok(())
    }

    #[inline]
    fn set_direct_pointers(&mut self, pointers: Vec<(u32, u32)>, data_size: u64) {
        self.data = Data::DirectPointers(pointers);
        self.size = data_size;
    }
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct DirectoryIndex {
    directories: BTreeMap<OsString, u32>,
    checksum: u32,
}

impl DirectoryIndex {
    pub fn init() -> Self {
        let mut r = Self {
            directories: BTreeMap::new(),
            checksum: 0,
        };
        r.checksum();
        r
    }
    pub fn find_dir<P>(&self, dir: P) -> Option<&u32>
    where
        P: AsRef<Path>,
    {
        self.directories.get(dir.as_ref().as_os_str())
    }
    pub fn create_dir<P>(&mut self, dir: P, inode_index: u32) -> Option<&u32>
    where
        P: AsRef<Path>,
    {
        if self.find_dir(&dir).is_some() {
            return None;
        }
        self.directories
            .insert(dir.as_ref().as_os_str().to_os_string(), inode_index);
        self.find_dir(dir)
    }
    pub fn move_dir<P>(&mut self, from: P, to: P) -> anyhow::Result<()>
    where
        P: AsRef<Path>,
    {
        if self.find_dir(&from).is_none() {
            return Err(anyhow!("From directory not found"));
        }
        if self.find_dir(&to).is_some() {
            return Err(anyhow!("Target directory has already exist"));
        }

        let dir_inode = self.directories.remove(from.as_ref().as_os_str()).unwrap();

        let _ = self
            .directories
            .insert(to.as_ref().as_os_str().to_os_string(), dir_inode);

        Ok(())
    }
    pub fn directories(&self) -> &BTreeMap<OsString, u32> {
        &self.directories
    }
    fn checksum(&mut self) {
        self.checksum = 0;
        self.checksum = calculate_checksum(&self);
    }

    fn verify_checksum(&mut self) -> bool {
        let checksum = self.checksum;
        self.checksum = 0;
        let ok = checksum == calculate_checksum(&self);
        self.checksum = checksum;

        ok
    }
}

#[derive(Serialize, Deserialize, Debug, Default)]
pub struct Directory {
    pub files: BTreeMap<String, u32>,
    checksum: u32,
}

impl Directory {
    fn init() -> Self {
        let mut dir = Directory {
            files: BTreeMap::new(),
            checksum: 0,
        };
        dir.checksum();
        dir
    }

    pub fn get_file(&self, file_name: &str) -> Option<u32> {
        self.files.get(file_name).map(|x| *x)
    }

    pub fn add_file(&mut self, file_name: &str, inode_block_index: u32) -> anyhow::Result<()> {
        match self.get_file(file_name) {
            Some(_) => Err(anyhow!("File already exist")),
            None => {
                self.files.insert(file_name.into(), inode_block_index);
                Ok(())
            }
        }
    }

    fn remove_file(&mut self, file_name: &str) -> anyhow::Result<()> {
        match self.files.remove(file_name) {
            Some(_) => Ok(()),
            None => Err(anyhow!("File not found!")),
        }
    }

    fn checksum(&mut self) {
        self.checksum = 0;
        self.checksum = calculate_checksum(&self);
    }

    fn verify_checksum(&mut self) -> bool {
        let checksum = self.checksum;
        self.checksum = 0;
        let ok = checksum == calculate_checksum(&self);
        self.checksum = checksum;

        ok
    }
}

#[cfg(test)]
mod tests {
    // use super::*;
    // use std::io::Cursor;
    // use std::time::{self, SystemTime};

    #[test]
    fn test_block_bitmap_seek_position() {
        // let group = Group::new(0);
        // assert_eq!(group.bitmap_seek_position(), BLOCK_SIZE);

        // let group = Group::new(1);
        // assert_eq!(group.bitmap_seek_position(), 134_221_824);
    }
}

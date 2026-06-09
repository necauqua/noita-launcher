use std::fmt::Write as _;
use std::io::Cursor;
use std::io::Read;
use std::io::SeekFrom;
use std::io::Write;
use std::ops::Deref;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use byteorder::LittleEndian;
use byteorder::ReadBytesExt;
use eyre::Context;
use eyre::Result;
use eyre::bail;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use sha1::Digest;
use sha1::Sha1;
use steam_vent_proto::content_manifest::content_manifest_payload::FileMapping;
use steam_vent_proto::content_manifest::content_manifest_payload::file_mapping::ChunkData;
use tokio::io::AsyncSeekExt as _;
use tokio::io::AsyncWriteExt as _;
use tokio::task::JoinSet;

use crate::cdn::CdnDownloader;
use crate::steam::Depot;

pub struct Chunk {
    pub sha: [u8; 20],
    pub sha_str: String,
    pub compressed_size: usize,
    pub size: usize,
}

impl TryFrom<&ChunkData> for Chunk {
    type Error = eyre::Error;

    fn try_from(value: &ChunkData) -> Result<Self> {
        let sha = value
            .sha()
            .try_into()
            .wrap_err("chunk sha1 had invalid length")?;
        let mut sha_str = String::with_capacity(40);
        for b in &sha {
            write!(&mut sha_str, "{:02x}", b).unwrap();
        }
        Ok(Self {
            sha,
            sha_str,
            compressed_size: value.cb_compressed() as _,
            size: value.cb_original() as _,
        })
    }
}

impl Chunk {
    pub fn get_cache_path(&self, base: &Path) -> PathBuf {
        let (folder, file) = self.sha_str.split_at(2);
        base.join(folder).join(file)
    }
}

pub trait ProgressCallback: Clone + Sync + Send + 'static {
    fn track(&self, bytes: u64);
}

impl ProgressCallback for () {
    fn track(&self, _bytes: u64) {}
}

impl ProgressCallback for indicatif::ProgressBar {
    fn track(&self, bytes: u64) {
        self.inc(bytes);
    }
}

pub struct InstanceDownloaderState {
    pub depot: Depot,
    cache: PathBuf,
    cdn: CdnDownloader,
}

#[derive(Clone)]
pub struct InstanceDownloader(Arc<InstanceDownloaderState>);

impl Deref for InstanceDownloader {
    type Target = InstanceDownloaderState;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl InstanceDownloader {
    pub fn new(
        depot: Depot,
        cdn_hosts: Arc<[String]>,
        cache: PathBuf,
        http: reqwest::Client,
        parallelism: usize,
    ) -> Self {
        Self(Arc::new(InstanceDownloaderState {
            depot,
            cache,
            cdn: CdnDownloader::new(cdn_hosts, http, parallelism),
        }))
    }

    pub async fn fetch_chunk(
        &self,
        chunk: &Chunk,
        progress: impl ProgressCallback,
    ) -> Result<Vec<u8>> {
        let cache_path = chunk.get_cache_path(&self.cache);

        match tokio::fs::read(&cache_path).await {
            Ok(bytes) => {
                // we're calling compute_cached_size before instantiation,
                //  rechecking sha1 here is superfluous

                // if Sha1::digest(&bytes) == chunk.sha {
                return Ok(bytes);
                // }
                // tracing::warn!(
                //     "chunk {sha} found in cache but failed integrity check, redownloading"
                // );
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }

        let bytes = self
            .cdn
            .download(
                &format!("depot/{}/chunk/{}", self.depot.id, chunk.sha_str),
                Some(chunk.compressed_size),
                async |mut bytes| {
                    let depot = self.depot.clone();
                    let (digest, decompressed) = tokio::task::spawn_blocking(move || {
                        let decrypted = depot.decrypt(&mut bytes)?;
                        let decompressed = steam_decompress(decrypted)?;
                        eyre::Ok((Sha1::digest(&decompressed), decompressed))
                    })
                    .await??;
                    if digest != chunk.sha {
                        bail!("downloaded chunk {} failed integrity check", chunk.sha_str);
                    }
                    Ok(decompressed)
                },
            )
            .await?;

        progress.track(bytes.len() as _);

        tokio::fs::create_dir_all(&cache_path.parent().unwrap()).await?;
        tokio::fs::write(cache_path, &bytes).await?;

        Ok(bytes)
    }

    pub fn fetch_chunks(
        &self,
        mapping: &FileMapping,
        progress: impl ProgressCallback,
    ) -> JoinSet<Result<(u64, Vec<u8>)>> {
        mapping
            .chunks
            .iter()
            .map(|chunk| {
                let s = self.clone();
                let progress = progress.clone();
                let offset = chunk.offset();
                let chunk = chunk.try_into();
                async move {
                    let bytes = s.fetch_chunk(&chunk?, progress).await?;
                    eyre::Ok((offset, bytes))
                }
            })
            .collect()
    }

    pub async fn fetch_to_file(
        &self,
        mapping: &FileMapping,
        target: &Path,
        progress: impl ProgressCallback,
        write_progress: impl ProgressCallback,
    ) -> Result<()> {
        let temp_path = target.with_added_extension("~");

        let mut file = tokio::fs::File::create(&temp_path).await?;
        file.set_len(mapping.size()).await?;

        let mut fetches = self.fetch_chunks(mapping, progress);

        while let Some(result) = fetches.join_next().await {
            let (offset, bytes) = result??;
            file.seek(SeekFrom::Start(offset)).await?;
            file.write_all(&bytes).await?;
            write_progress.track(bytes.len() as _);
        }

        drop(file);

        tokio::fs::rename(&temp_path, target).await?;

        Ok(())
    }

    pub async fn compute_cached_size(
        &self,
        mappings: &[FileMapping],
        validate: bool,
        progress: impl ProgressCallback,
    ) -> Result<u64> {
        let total = Arc::new(AtomicU64::new(0));

        let mut join_set = mappings
            .iter()
            .flat_map(|m| m.chunks.iter())
            .map(|chunk| {
                let s = self.clone();
                let total = total.clone();
                let progress = progress.clone();
                let chunk = chunk.try_into();
                async move {
                    let chunk: Chunk = chunk?;
                    let cache_path = chunk.get_cache_path(&s.cache);

                    if validate {
                        let Ok(bytes) = tokio::fs::read(&cache_path).await else {
                            progress.track(chunk.size as _);
                            return eyre::Ok(());
                        };
                        if Sha1::digest(&bytes) == chunk.sha {
                            let len = bytes.len() as _;
                            progress.track(len);
                            total.fetch_add(len, Ordering::Relaxed);
                            return eyre::Ok(());
                        }

                        tracing::warn!("cached chunk {} failed integrity check", chunk.sha_str);

                        tokio::fs::remove_file(cache_path).await?;
                    } else {
                        if tokio::fs::try_exists(&cache_path).await? {
                            total.fetch_add(chunk.size as _, Ordering::Relaxed);
                        }
                        progress.track(chunk.size as _);
                    }

                    eyre::Ok(())
                }
            })
            .collect::<JoinSet<_>>();

        while let Some(res) = join_set.join_next().await {
            res??
        }

        Ok(total.load(Ordering::Relaxed))
    }

    pub async fn prefetch(
        &self,
        mappings: &[FileMapping],
        progress: impl ProgressCallback,
    ) -> Result<()> {
        let mut tasks = FuturesUnordered::new();
        for mapping in mappings {
            // skip dir entries
            if mapping.flags() & 0x40 != 0 {
                continue;
            }

            let progress = progress.clone();
            tasks.push(async move {
                let mut fetches = self.fetch_chunks(mapping, progress);
                while let Some(res) = fetches.join_next().await {
                    res??;
                }
                eyre::Ok(())
            });
        }
        while let Some(res) = tasks.next().await {
            res?;
        }
        Ok(())
    }

    pub async fn fetch(
        &self,
        mappings: Vec<FileMapping>,
        folder: &Path,
        progress: impl ProgressCallback,
        write_progress: impl ProgressCallback,
    ) -> Result<()> {
        let mut tasks = FuturesUnordered::new();
        for mapping in &mappings {
            // skip dir entries
            if mapping.flags() & 0x40 != 0 {
                continue;
            }

            let target = folder.join(mapping.filename());

            let progress = progress.clone();
            let write_progress = write_progress.clone();
            tasks.push(async move {
                // unwrap: always ok (I think?) because target is result of Path::join
                tokio::fs::create_dir_all(target.parent().unwrap()).await?;
                self.fetch_to_file(mapping, &target, progress, write_progress)
                    .await
            });
        }
        while let Some(res) = tasks.next().await {
            res?;
        }

        Ok(())
    }
}

fn steam_decompress(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut c = Cursor::new(bytes);
    let mut magic = [0; 2];
    c.read_exact(&mut magic)?;
    match &magic {
        b"VZ" => decompress_vz(c),
        b"VS" => decompress_vsz(c),
        [a, b] => bail!("unknown chunk magic 0x{a:02x}{b:02x}"),
    }
}

// VZ compressed chunk format
//
// Header (12 bytes):
//   [u8; 2] magic      = b"VZ" (0x5A56)
//   u8      version    = b'a'
//   u32le   crc        (crc for depot chunks, could be timestamp in some other cases)
//   [u8; 5] lzma_props (1 byte props + 4 bytes dict size)
//
// Body:
//   [u8; _len - 12 - 10]  lzma_payload
//
// Footer (10 bytes):
//   u32le  crc_footer        (crc again, this time likely always crc)
//   u32le  uncompressed_size
//   u16le  trailer           = b"zv" (0x767A)
fn decompress_vz(mut c: Cursor<&[u8]>) -> Result<Vec<u8>> {
    // b"VZ" already consumed by dispatch
    if c.read_u8()? != b'a' {
        bail!("bad VZ version");
    }

    let _crc = c.read_u32::<LittleEndian>()?;

    let mut lzma_props = [0u8; 5];
    c.read_exact(&mut lzma_props)?;

    let body_start = c.position() as usize;
    let body_end = c.get_ref().len() - 10;

    c.set_position(body_end as _);

    let _crc_footer = c.read_u32::<LittleEndian>()?;
    let uncompressed_size = c.read_u32::<LittleEndian>()? as usize;

    if c.read_u16::<LittleEndian>()? != 0x767A {
        bail!("bad VZ footer");
    }

    let lzma_payload = &c.get_ref()[body_start..body_end];

    (|| {
        let mut out = Vec::with_capacity(uncompressed_size);
        let mut w = lzma_rs::decompress::Stream::new(&mut out);
        // write a standard LZMA1 header (props + u64le size) then append the payload
        w.write_all(&lzma_props)?;
        w.write_all(&(uncompressed_size as u64).to_le_bytes())?;
        w.write_all(lzma_payload)?;
        w.finish()?;
        eyre::Ok(out)
    })()
    .wrap_err("lzma decompression")
}

// VSZ compressed chunk format
//
// Header (8 bytes):
//   [u8; 4] magic = b"VSZa" (0x615A5356)
//   u32le   crc
//
// Body:
//   [u8; _len - 8 - 15]  zstd_payload
//
// Footer (15 bytes):
//   u32le   crc_footer
//   u32le   uncompressed_size
//   [u8; 4] _                 (unknown/reserved)
//   [u8; 3] trailer           = b"zsv"
fn decompress_vsz(mut c: Cursor<&[u8]>) -> Result<Vec<u8>> {
    // b"VS" already consumed by dispatch
    let mut rest_magic = [0; 2];
    c.read_exact(&mut rest_magic)?;
    if &rest_magic != b"Za" {
        bail!("bad VSZ magic");
    }

    let _crc = c.read_u32::<LittleEndian>()?;

    let body_start = c.position() as usize;
    let body_end = c.get_ref().len() - 15;

    c.set_position(body_end as _);

    let _crc_footer = c.read_u32::<LittleEndian>()?;
    let _uncompressed_size = c.read_u32::<LittleEndian>()?;
    let _unknown = c.read_u32::<LittleEndian>()?;

    let mut trailer = [0u8; 3];
    c.read_exact(&mut trailer)?;
    if &trailer != b"zsv" {
        bail!("bad VSZ trailer");
    }

    zstd::decode_all(&c.get_ref()[body_start..body_end]).wrap_err("zstd decompression")
}

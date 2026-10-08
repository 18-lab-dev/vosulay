use anyhow::{Context, Result, bail};
use crc32fast::Hasher;
use flate2::{Compression, write::DeflateEncoder};
use std::{
    fs::File,
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
};

const MAX_ZIP_SIZE: u64 = 3_000_000_000;
const UTF8_AND_DESCRIPTOR: u16 = 0x0808;
const DOS_DATE_1980_01_01: u16 = 0x0021;

struct Entry {
    name: String,
    crc: u32,
    compressed_size: u32,
    size: u32,
    offset: u32,
}

pub(super) fn write_zip(output: &Path, root: &Path, files: &[PathBuf]) -> Result<()> {
    let input_size = files.iter().try_fold(0_u64, |sum, path| {
        sum.checked_add(std::fs::metadata(path)?.len())
            .context("ZIP package size overflow")
    })?;
    if input_size > MAX_ZIP_SIZE {
        bail!("ZIP package exceeds 3 GB limit");
    }
    if files.len() > u16::MAX as usize {
        bail!("ZIP32 entry count overflow");
    }
    if output.exists() {
        bail!("ZIP output already exists: {}", output.display());
    }

    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("creating temporary ZIP beside {}", output.display()))?;
    let mut entries = Vec::with_capacity(files.len());

    for path in files {
        let relative = path
            .strip_prefix(root)
            .context("package file outside package root")?;
        let relative = relative.to_str().context("ZIP path is not valid UTF-8")?;
        let mut name = String::with_capacity("vosulay/".len() + relative.len());
        name.push_str("vosulay/");
        if cfg!(windows) {
            for (index, component) in relative.split('\\').enumerate() {
                if index != 0 {
                    name.push('/');
                }
                name.push_str(component);
            }
        } else {
            name.push_str(relative);
        }
        let name_bytes = name.as_bytes();
        if name_bytes.len() > u16::MAX as usize {
            bail!("ZIP path exceeds ZIP32 limit: {name}");
        }

        let offset = u32::try_from(temp.as_file().stream_position()?)
            .context("ZIP32 local-header offset overflow")?;
        write_local_header(&mut temp, name_bytes)?;
        let mut input = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut checksum = Hasher::new();
        let mut size = 0_u64;
        let compressed_start = temp.as_file().stream_position()?;
        {
            let mut encoder = DeflateEncoder::new(temp.as_file_mut(), Compression::new(6));
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let count = input.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                size = size
                    .checked_add(count as u64)
                    .context("ZIP32 file size overflow")?;
                if size > u32::MAX as u64 {
                    bail!("ZIP32 file size overflow: {}", path.display());
                }
                checksum.update(&buffer[..count]);
                encoder.write_all(&buffer[..count])?;
            }
            encoder.finish()?;
        }
        let compressed_end = temp.as_file().stream_position()?;
        if compressed_end > MAX_ZIP_SIZE {
            bail!("ZIP package exceeds 3 GB limit");
        }
        let compressed_size = u32::try_from(compressed_end - compressed_start)
            .context("ZIP32 compressed size overflow")?;
        let size = u32::try_from(size).context("ZIP32 file size overflow")?;
        let crc = checksum.finalize();
        write_data_descriptor(&mut temp, crc, compressed_size, size)?;
        entries.push(Entry {
            name,
            crc,
            compressed_size,
            size,
            offset,
        });
    }

    let central_offset = u32::try_from(temp.as_file().stream_position()?)
        .context("ZIP32 central-directory offset overflow")?;
    for entry in &entries {
        write_central_entry(&mut temp, entry)?;
    }
    let central_end = temp.as_file().stream_position()?;
    let central_size = u32::try_from(central_end - u64::from(central_offset))
        .context("ZIP32 central-directory size overflow")?;
    write_end_record(
        &mut temp,
        entries.len() as u16,
        central_size,
        central_offset,
    )?;
    if temp.as_file().stream_position()? > MAX_ZIP_SIZE {
        bail!("ZIP package exceeds 3 GB limit");
    }
    temp.as_file_mut().flush()?;
    temp.persist_noclobber(output)
        .with_context(|| format!("publishing ZIP without overwriting {}", output.display()))?;
    Ok(())
}

fn write_local_header(writer: &mut impl Write, name: &[u8]) -> Result<()> {
    writer.write_all(&[0x50, 0x4b, 0x03, 0x04])?;
    le16(writer, 20)?;
    le16(writer, UTF8_AND_DESCRIPTOR)?;
    le16(writer, 8)?;
    le16(writer, 0)?;
    le16(writer, DOS_DATE_1980_01_01)?;
    le32(writer, 0)?;
    le32(writer, 0)?;
    le32(writer, 0)?;
    le16(writer, name.len() as u16)?;
    le16(writer, 0)?;
    writer.write_all(name)?;
    Ok(())
}

fn write_data_descriptor(
    writer: &mut impl Write,
    crc: u32,
    compressed: u32,
    size: u32,
) -> Result<()> {
    le32(writer, 0x0807_4b50)?;
    le32(writer, crc)?;
    le32(writer, compressed)?;
    le32(writer, size)?;
    Ok(())
}

fn write_central_entry(writer: &mut impl Write, entry: &Entry) -> Result<()> {
    let name = entry.name.as_bytes();
    writer.write_all(&[0x50, 0x4b, 0x01, 0x02])?;
    le16(writer, 20)?;
    le16(writer, 20)?;
    le16(writer, UTF8_AND_DESCRIPTOR)?;
    le16(writer, 8)?;
    le16(writer, 0)?;
    le16(writer, DOS_DATE_1980_01_01)?;
    le32(writer, entry.crc)?;
    le32(writer, entry.compressed_size)?;
    le32(writer, entry.size)?;
    le16(writer, name.len() as u16)?;
    le16(writer, 0)?;
    le16(writer, 0)?;
    le16(writer, 0)?;
    le16(writer, 0)?;
    le32(writer, 0)?;
    le32(writer, entry.offset)?;
    writer.write_all(name)?;
    Ok(())
}

fn write_end_record(
    writer: &mut impl Write,
    count: u16,
    central_size: u32,
    central_offset: u32,
) -> Result<()> {
    writer.write_all(&[0x50, 0x4b, 0x05, 0x06])?;
    le16(writer, 0)?;
    le16(writer, 0)?;
    le16(writer, count)?;
    le16(writer, count)?;
    le32(writer, central_size)?;
    le32(writer, central_offset)?;
    le16(writer, 0)?;
    Ok(())
}

fn le16(writer: &mut impl Write, value: u16) -> std::io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn le32(writer: &mut impl Write, value: u32) -> std::io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

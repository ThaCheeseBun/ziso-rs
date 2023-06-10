use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use clap::{Parser, Subcommand, ValueEnum};
use lz4::block::{compress, decompress, CompressionMode};
use std::fmt;
use std::fs::File;
use std::io::prelude::*;
use std::io::{Cursor, SeekFrom};
use std::path::PathBuf;
use std::vec;

const ZISO_MAGIC: u32 = 0x4F53495A; // ZISO
const COMPRESS_THREHOLD: usize = 100;
const HEADER_SIZE: u32 = 0x18; // 24
const BLOCK_SIZE: u32 = 0x800; // 2048
const VERSION: i8 = 1;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Compress ISO to ZSO
    Compress {
        /// Compression mode
        #[arg(short, long, default_value_t = Mode::Default)]
        mode: Mode,

        /// Input ISO file
        infile: PathBuf,

        /// Output ZSO file
        outfile: PathBuf,
    },
    /// Decompress ZSO to ISO
    Decompress {
        /// Input ZSO file
        infile: PathBuf,

        /// Output ISO file
        outfile: PathBuf,
    },
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum Mode {
    Default,
    Fast,
    Slow,
}
impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let printable = match *self {
            Mode::Default => "default",
            Mode::Fast => "fast",
            Mode::Slow => "slow",
        };
        write!(f, "{}", printable)
    }
}

fn lz4_decompress(compressed: Vec<u8>, block_size: i32) -> Vec<u8> {
    let mut compress = compressed;
    return loop {
        let temp = decompress(&compress, Some(block_size));
        if temp.is_err() {
            compress.remove(compress.len() - 1);
        } else {
            break temp.unwrap();
        }
    };
}

fn decompress_zso(infile: PathBuf, outfile: PathBuf) {
    let mut fin = File::open(&infile).unwrap();
    let mut fout = File::create(&outfile).unwrap();

    let fin_size = fin.metadata().unwrap().len();

    let mut header_buf = [0; HEADER_SIZE as usize];
    fin.read_exact(&mut header_buf).unwrap();
    let mut header = Cursor::new(header_buf);

    let magic = header.read_u32::<LittleEndian>().unwrap();
    let header_size = header.read_u32::<LittleEndian>().unwrap();
    let total_bytes = header.read_u64::<LittleEndian>().unwrap();
    let block_size = header.read_u32::<LittleEndian>().unwrap();
    let ver = header.read_i8().unwrap();
    let align = header.read_i8().unwrap();

    if magic != ZISO_MAGIC
        || header_size != HEADER_SIZE
        || total_bytes == 0
        || block_size == 0
        || ver != VERSION
    {
        println!("ziso file format error");
        panic!();
    }

    let total_block = total_bytes / block_size as u64;
    let mut index_buf = vec![];

    let mut index_raw = vec![0; (total_block as usize + 1) * 4];
    fin.read_exact(&mut index_raw).unwrap();
    let mut index_read = Cursor::new(index_raw);
    for _ in 0..total_block + 1 {
        index_buf.push(index_read.read_u32::<LittleEndian>().unwrap() as u64);
    }

    println!("Decompress '{:?}' to '{:?}'", infile, outfile);
    println!("Total File Size {} bytes", total_bytes);
    println!("block size      {} bytes", block_size);
    println!("total blocks    {} blocks", total_block);
    println!("index align     {}", align);

    let mut block: u64 = 0;
    let percent_period = total_block / 100;
    let mut percent_cnt = 0;

    while block < total_block {
        percent_cnt += 1;
        if percent_cnt >= percent_period && percent_period != 0 {
            percent_cnt = 0;
            eprint!("decompress {}%\r", block / percent_period);
        }

        let mut index = index_buf[block as usize];
        let plain = index & 0x80000000;
        index &= 0x7fffffff;
        let read_pos = index << (align);

        let read_size = {
            if plain > 0 {
                block_size as u64
            } else {
                let index2 = index_buf[block as usize + 1] & 0x7fffffff;
                // Have to read more bytes if align was set
                let mut read_size2 = (index2 - index) << (align);
                if block == total_block - 1 {
                    read_size2 = total_bytes - read_pos;
                }
                read_size2
            }
        };

        let current_pos = fin.seek(SeekFrom::Start(read_pos)).unwrap();
        let zso_data = {
            if current_pos + read_size > fin_size {
                let mut x = vec![];
                fin.read_to_end(&mut x).unwrap();
                x
            } else {
                let mut x = vec![0; read_size as usize];
                fin.read_exact(&mut x).unwrap();
                x
            }
        };

        let dec_data = {
            if plain > 0 {
                zso_data
            } else {
                lz4_decompress(zso_data, block_size as i32)
            }
        };

        if dec_data.len() != block_size as usize {
            println!("{} block: {:#x} {}", block, read_pos, read_size);
            panic!();
        }

        fout.write_all(&dec_data).unwrap();
        block += 1;
    }

    println!("ziso decompress completed");
}

fn compress_zso(mode: Mode, infile: PathBuf, outfile: PathBuf) {
    let mut fin = File::open(&infile).unwrap();
    let mut fout = File::create(&outfile).unwrap();

    let total_bytes = fin.metadata().unwrap().len();

    // We have to use alignment on any ZSO files which > 2GB, for MSB bit of index as the plain indicator
    // If we don't then the index can be larger than 2GB, which its plain indicator was improperly set
    let align = total_bytes / 2u64.pow(31);

    let mut header = Cursor::new([0u8; HEADER_SIZE as usize]);
    header.write_u32::<LittleEndian>(ZISO_MAGIC).unwrap();
    header.write_u32::<LittleEndian>(HEADER_SIZE).unwrap();
    header.write_u64::<LittleEndian>(total_bytes).unwrap();
    header.write_u32::<LittleEndian>(BLOCK_SIZE).unwrap();
    header.write_i8(VERSION).unwrap();
    header.write_i8(align as i8).unwrap();
    fout.write_all(&header.into_inner()).unwrap();

    let total_block = total_bytes / BLOCK_SIZE as u64;

    let mut index_buf = vec![];
    for _ in 0..(total_block as u64 + 1) {
        index_buf.push(0u64);
    }
    fout.write(&vec![0u8; (total_block as usize + 1) * 4])
        .unwrap();

    println!("Compress '{:?}' to '{:?}'", infile, outfile);
    println!("Total File Size {} bytes", total_bytes);
    println!("block size      {} bytes", BLOCK_SIZE);
    println!("index align     {}", 1 << align);

    let mut write_pos = fout.stream_position().unwrap();
    let percent_period = total_block / 100;
    let mut percent_cnt: u64 = 0;

    let mut block: usize = 0;

    while block < total_block as usize {
        percent_cnt += 1;

        if percent_cnt >= percent_period && percent_period != 0 {
            percent_cnt = 0;

            if block == 0 {
                eprint!(
                    "compress {:>3}% average rate {:>3}%\r",
                    block as u64 / percent_period,
                    0
                );
            } else {
                eprint!(
                    "compress {:>3}% average rate {:>3}%\r",
                    block as u64 / percent_period,
                    100 * write_pos / (block as u64 * 0x800)
                );
            }
        }

        let mut iso_data = vec![0; BLOCK_SIZE as usize];
        fin.read_exact(&mut iso_data).unwrap();

        let mut zso_data = compress(
            &iso_data,
            Some(match mode {
                Mode::Fast => CompressionMode::FAST(0),
                Mode::Slow => CompressionMode::HIGHCOMPRESSION(0),
                Mode::Default => CompressionMode::DEFAULT,
            }),
            false,
        )
        .unwrap();

        if write_pos % (1 << align) > 0 {
            let align_len = (1 << align) - write_pos % (1 << align);
            fout.write_all(&vec![b'X'; align_len as usize]).unwrap();
            write_pos += align_len;
        }

        index_buf[block] = write_pos >> align;

        if 100 * zso_data.len() / iso_data.len() >= COMPRESS_THREHOLD {
            zso_data = iso_data;
            index_buf[block] |= 0x80000000; // Mark as plain;
        } else if index_buf[block] & 0x80000000 > 0 {
            println!("Align error, you have to increase align by 1 or CFW won't be able to read offset above 2 ** 31 bytes");
        }

        fout.write_all(&zso_data).unwrap();
        write_pos += zso_data.len() as u64;
        block += 1;
    }

    // Last position (total size)
    index_buf[block] = write_pos >> align;

    // Update index block
    fout.seek(SeekFrom::Start(HEADER_SIZE as u64)).unwrap();
    for x in index_buf {
        fout.write_u32::<LittleEndian>(x as u32).unwrap();
    }

    println!(
        "ziso compress completed, total size = {:>8} bytes, rate {}%",
        write_pos,
        (write_pos * 100 / total_bytes)
    );
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Compress {
            mode,
            infile,
            outfile,
        }) => {
            compress_zso(mode, infile, outfile);
        }
        Some(Commands::Decompress { infile, outfile }) => {
            decompress_zso(infile, outfile);
        }
        None => {
            println!("Unknown command");
        }
    }
}

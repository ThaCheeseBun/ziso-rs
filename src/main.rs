use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use clap::Parser;
use lz4::block::{compress, decompress};
use std::fs::File;
use std::io::prelude::*;
use std::io::Cursor;
use std::path::PathBuf;
use std::vec;

const ZISO_MAGIC: u32 = 0x4F53495A;
const COMPRESS_THREHOLD: usize = 100;
const header_size: u32 = 0x18;
const block_size: u32 = 0x800;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Set compression level
    #[arg(short = 'c', long)]
    level: Option<u8>,

    /// Set Compression Threshold (1-100)
    #[arg(short, long)]
    threshold: Option<u8>,

    /// Padding alignment 0=small/slow 6=fast/large
    #[arg(short, long)]
    align: Option<u8>,

    /// Padding byte
    #[arg(short, long)]
    pad: Option<char>,

    /// Set input file
    infile: PathBuf,

    /// Set output file
    outfile: PathBuf,
}

fn lz4_decompress(compressed: Vec<u8>, block_size_: i32) -> Vec<u8> {
    let mut compress = compressed;
    let mut thing: Option<Vec<u8>> = None;
    loop {
        let temp = decompress(&compress, Some(block_size_));
        if temp.is_err() {
            compress.remove(compress.len() - 1);
        } else {
            thing = Some(temp.unwrap());
            break;
        }
    }
    return thing.unwrap();
}

fn decompress_zso(cli: Cli) {
    let mut fin = File::open(&cli.infile).unwrap();
    let mut fout = File::create(&cli.outfile).unwrap();

    let mut header_data = [0; header_size as usize];
    fin.read_exact(&mut header_data).unwrap();
    let mut rdr = Cursor::new(header_data);

    let magic = rdr.read_u32::<LittleEndian>().unwrap();
    let header_size_ = rdr.read_u32::<LittleEndian>().unwrap();
    let total_bytes = rdr.read_u64::<LittleEndian>().unwrap();
    let block_size_ = rdr.read_u32::<LittleEndian>().unwrap();
    let ver = rdr.read_i8().unwrap();
    let align = rdr.read_i8().unwrap();

    if magic != ZISO_MAGIC || block_size_ == 0 || total_bytes == 0 || header_size_ != 24 || ver > 1
    {
        println!("ziso file format error");
        panic!();
    }

    let total_block = total_bytes / block_size_ as u64;
    let mut index_buf = vec![];

    for _ in 0..total_block + 1 {
        index_buf.push(fin.read_u32::<LittleEndian>().unwrap() as u64);
    }

    println!("Decompress '{:?}' to '{:?}'", cli.infile, cli.outfile);
    println!("Total File Size {:?} bytes", total_bytes);
    println!("block size      {:?} bytes", block_size_);
    println!("total blocks    {:?} blocks", total_block);
    println!("index align     {:?}", align);

    let mut block: u64 = 0;
    let percent_period = total_block / 100;
    let mut percent_cnt = 0;

    while block < total_block {
        percent_cnt += 1;
        if percent_cnt >= percent_period && percent_period != 0 {
            percent_cnt = 0;
            eprint!("decompress {:?}\r", block / percent_period);
        }

        let mut index = index_buf[block as usize];
        let plain = index & 0x80000000;
        index &= 0x7fffffff;
        let read_pos = index << (align);

        let read_size = {
            if plain > 0 {
                block_size_ as u64
            } else {
                let index2 = index_buf[block as usize+1] & 0x7fffffff;
                let mut read_size2 = (index2-index) << (align);
                if block == total_block - 1 {
                    read_size2 = total_bytes - read_pos;
                }
                read_size2
            }
        };

        fin.seek(std::io::SeekFrom::Start(read_pos as u64)).unwrap();
        let mut zso_data = vec![0; read_size as usize];
        let read_res = fin.read_exact(&mut zso_data);
        if read_res.is_err() && read_res.unwrap_err().kind() == std::io::ErrorKind::UnexpectedEof {
            zso_data = vec![];
            fin.seek(std::io::SeekFrom::Start(read_pos as u64)).unwrap();
            fin.read_to_end(&mut zso_data).unwrap();
        }

        let dec_data = {
            if plain > 0 {
                zso_data
            } else {
                lz4_decompress(zso_data, block_size_ as i32)
            }
        };

        if dec_data.len() != block_size_ as usize {
            println!("{:?} block: 0x{:?} {:?}", block, read_pos, read_size);
            panic!();
        }

        fout.write_all(&dec_data).unwrap();
        block += 1;
    }

    println!("ziso decompress completed");
}

fn compress_zso(cli: Cli) {
    let mut fin = File::open(&cli.infile).unwrap();
    let mut fout = File::create(&cli.outfile).unwrap();

    let total_bytes = fin.seek(std::io::SeekFrom::End(0)).unwrap();
    fin.seek(std::io::SeekFrom::Start(0)).unwrap();

    let ver = 1i8;

    // We have to use alignment on any ZSO files which > 2GB, for MSB bit of index as the plain indicator
    // If we don't then the index can be larger than 2GB, which its plain indicator was improperly set
    let align = total_bytes / 2u64.pow(31);

    let mut header: Vec<u8> = vec![];
    header.write_u32::<LittleEndian>(ZISO_MAGIC).unwrap();
    header.write_u32::<LittleEndian>(header_size).unwrap();
    header.write_u64::<LittleEndian>(total_bytes).unwrap();
    header.write_u32::<LittleEndian>(block_size).unwrap();
    header.write_i8(ver).unwrap();
    header.write_i8(align as i8).unwrap();
    header.extend([0u8; 2]);
    fout.write_all(&header).unwrap();

    let total_block = (total_bytes as f64 / block_size as f64).floor();

    let mut index_buf: Vec<u64> = vec![];
    for _ in 0..(total_block as u64 + 1) {
        index_buf.push(0);
        fout.write_all(&[0u8; 4]).unwrap();
    }

    println!("Compress '{:?}' to '{:?}'", cli.infile, cli.outfile);
    println!("Total File Size {:?} bytes", total_bytes);
    println!("block size {:?} bytes", block_size);
    println!("index align {:?}", (1u64 << align));
    //println!("compress level {:?}", cli.level.unwrap());

    let mut write_pos = fout.stream_position().unwrap();
    let percent_period = total_block / 100f64;
    let mut percent_cnt: u64 = 0;

    let mut block: usize = 0;

    while block < total_block as usize {
        percent_cnt += 1;

        if percent_cnt >= percent_period as u64 && percent_period != 0f64 {
            percent_cnt = 0;

            if block == 0 {
                eprint!(
                    "compress {:?} average rate {:?}\r",
                    block as f64 / percent_period,
                    0
                );
            } else {
                eprint!(
                    "compress {:?} average rate {:?}\r",
                    block as f64 / percent_period,
                    100 * write_pos / (block as u64 * 0x800)
                );
            }
        }

        let mut iso_data = vec![0; block_size as usize];
        fin.read_exact(&mut iso_data).unwrap();

        let mut zso_data = compress(&iso_data, None, false).unwrap();

        if write_pos % (1u64 << align) > 0 {
            let align_len = (1u64 << align) - write_pos % (1u64 << align);
            let mut to_write: Vec<u8> = vec![];
            for _ in 0..align_len {
                to_write.push(b'X');
            }
            fout.write_all(&to_write).unwrap();
            write_pos += align_len;
        }

        index_buf[block as usize] = write_pos >> align;

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
    fout.seek(std::io::SeekFrom::Start(header.len() as u64))
        .unwrap();
    for x in index_buf {
        fout.write_u32::<LittleEndian>(x as u32).unwrap();
    }

    println!(
        "ziso compress completed , total size = {} bytes , rate {}",
        write_pos,
        (write_pos * 100 / total_bytes)
    );
}

fn main() {
    let cli = Cli::parse();

    if cli.level.is_some() && cli.level.unwrap() == 0 {
        decompress_zso(cli);
    } else {
        compress_zso(cli);
    }
}

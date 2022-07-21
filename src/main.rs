use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use lz4_flex::block::{compress, decompress};

use clap::{arg, command, value_parser};

const ZISO_MAGIC: u32 = 0x4F53495A;

fn open_input_output(fname_in: &PathBuf, fname_out: &PathBuf) -> (File, File) {
    let fin = File::open(&fname_in);
    if std::io::Result::is_err(&fin) {
        panic!("Can't open {:?}", &fname_in);
    }
    let fout = File::create(&fname_out);
    if std::io::Result::is_err(&fin) {
        panic!("Can't create {:?}", &fname_out);
    }
    return (fin.unwrap(), fout.unwrap());
}

fn read_zso_header(fin: &mut File) -> (u32, u32, u64, u32, i8, i8) {
    // ZSO header has 0x18 bytes
    fin.seek(SeekFrom::Current(0)).unwrap();

    let magic = fin.read_u32::<LittleEndian>().unwrap();
    let header_size = fin.read_u32::<LittleEndian>().unwrap();
    let total_bytes = fin.read_u64::<LittleEndian>().unwrap();
    let block_size = fin.read_u32::<LittleEndian>().unwrap();
    let ver = fin.read_i8().unwrap();
    let align = fin.read_i8().unwrap();

    fin.seek(SeekFrom::Current(2)).unwrap();

    return (magic, header_size, total_bytes, block_size, ver, align)
}

fn generate_zso_header(
    magic: u32,
    header_size: u32,
    total_bytes: u64,
    block_size: u32,
    ver: i8,
    align: i8,
) -> [u8; 0x18] {
    let mut data = Cursor::new([0; 0x18]);
    data.write_u32::<LittleEndian>(magic).unwrap();
    data.write_u32::<LittleEndian>(header_size).unwrap();
    data.write_u64::<LittleEndian>(total_bytes).unwrap();
    data.write_u32::<LittleEndian>(block_size).unwrap();
    data.write_i8(ver).unwrap();
    data.write_i8(align).unwrap();
    return *data.get_ref();
}

fn show_zso_info(fname_in: &PathBuf, fname_out: &PathBuf, total_bytes: &u64, block_size: &u32, total_block: &usize, align: &i8) {
    println!("Decompress '{:?}' to '{:?}'", fname_in, fname_out);
    println!("Total File Size {:?} bytes", total_bytes);
    println!("block size      {:?}  bytes", block_size);
    println!("total blocks    {:?}  blocks", total_block);
    println!("index align     {:?}", align);
}

fn decompress_zso(fname_in: &PathBuf, fname_out: &PathBuf) {
    let (mut fin, mut fout) = open_input_output(&fname_in, &fname_out);

    let reff = std::io::Read::by_ref(&mut fin);
    let (magic, header_size, total_bytes, block_size, ver, align) = read_zso_header(reff);

    if magic != ZISO_MAGIC || block_size == 0 || total_bytes == 0 || header_size != 24 || ver > 1 {
        panic!("ziso file format error");
    }
    let total_block: usize = (total_bytes / (block_size as u64)) as usize;
    let mut index_buf = vec![];

    for _ in 0..(total_block + 1) {
        index_buf.push(fin.read_u32::<LittleEndian>().unwrap());
    }

    show_zso_info(&fname_in, &fname_out, &total_bytes, &block_size, &total_block, &align);

    let mut block: usize = 0;
    let percent_period = total_block/100;
    let mut percent_cnt = 0;

    while block < total_block {
        percent_cnt += 1;
        if percent_cnt >= percent_period && percent_period != 0 {
            percent_cnt = 0;
            eprint!("decompress {:?}%\r", block / percent_period);
        }

        let mut index = index_buf[block];
        let plain = index & 0x80000000;
        index &= 0x7fffffff;
        let read_pos = index << (align);

        let mut read_size: u32 = 0;
        if plain != 0 {
            read_size = block_size;
        } else {
            let index2 = index_buf[block+1] & 0x7fffffff;
            // Have to read more bytes if align was set
            read_size = (index2-index) << (align);
            if block == total_block - 1 {
                read_size = (total_bytes as u32) - read_pos;
            }
        }

        fin.seek(SeekFrom::Start(read_pos as u64)).unwrap();

        let mut zso_data = vec![];
        std::io::Read::by_ref(&mut fin)
            .take(read_size as u64)
            .read_to_end(&mut zso_data)
            .unwrap();

        let mut dec_data: Vec<u8> = vec![];
        if plain != 0 {
            dec_data = zso_data
        } else {
            let dec = decompress(&zso_data, block_size as usize);

            if Result::is_err(&dec) {
                let e = "temp";
                panic!("{:?} block: 0x{:x} {:?} {:?}", block, read_pos, read_size, e);
            }

            dec_data = dec.unwrap();
        }

        if dec_data.len() != block_size as usize {
            panic!("{:?} block: 0x{:x} {:?}", block, read_pos, read_size);
        }
        fout.write(&dec_data).unwrap();
        block += 1;
    }

    drop(fin);
    fout.sync_all().unwrap();
    drop(fout);
    println!("ziso decompress completed");
}

fn show_comp_info(
    fname_in: &PathBuf,
    fname_out: &PathBuf,
    total_bytes: u64,
    block_size: u32,
    align: i8,
    level: u8,
) {
    println!("Compress '{:?}' to '{:?}'", fname_in, fname_out);
    println!("Total File Size {:?} bytes", total_bytes);
    println!("block size      {:?}  bytes", block_size);
    println!("index align     {:?}", 1 << align);
    println!("compress level  {:?}", level);
}

fn set_align(fout: &mut File, write_pos: u64, align: i8, padding: char) -> u64 {
    let mut write_pos2 = write_pos;
    if (write_pos2 % (1 << align)) > 1 {
        let align_len = (1 << align) - write_pos2 % (1 << align);
        for _ in 0..align_len {
            fout.write(&[padding as u8]).unwrap();
        }
        write_pos2 += align_len;
    }
    return write_pos2;
}

fn compress_zso(
    fname_in: &PathBuf,
    fname_out: &PathBuf,
    level: &u8,
    percent: &u8,
    align: &i8,
    pad: &char,
) {
    let (mut fin, mut fout) = open_input_output(&fname_in, &fname_out);
    let total_bytes = fin.seek(SeekFrom::End(0)).unwrap();
    fin.seek(SeekFrom::Start(0)).unwrap();

    let magic = ZISO_MAGIC;
    let header_size = 0x18;
    let block_size = 0x800;
    let ver = 1;
    let mut alignment = *align;

    // We have to use alignment on any ZSO files which > 2GB, for MSB bit of index as the plain indicator
    // If we don't then the index can be larger than 2GB, which its plain indicator was improperly set
    alignment = (total_bytes / u64::pow(2, 31)) as i8;

    let header = generate_zso_header(magic, header_size, total_bytes, block_size, ver, alignment);
    fout.seek(SeekFrom::Start(0)).unwrap();
    fout.write(&header).unwrap();

    let total_block = (total_bytes as u32) / block_size;
    let mut index_buf: Vec<u64> = vec![];
    index_buf.resize((total_block + 1) as usize, 0);

    fout.seek(SeekFrom::Start(0x18)).unwrap();
    for _ in 0..index_buf.len() {
        fout.write(b"\x00\x00\x00\x00").unwrap();
    }

    show_comp_info(
        &fname_in,
        &fname_out,
        total_bytes,
        block_size,
        alignment,
        *level,
    );

    let mut write_pos = fout.seek(SeekFrom::Current(0)).unwrap();
    let percent_period = total_block / 100;
    let mut percent_cnt = 0;

    let mut block = 0;
    while block < total_block {
        percent_cnt += 1;

        if percent_cnt >= percent_period && percent_period != 0 {
            percent_cnt = 0;

            if block == 0 {
                eprint!(
                    "compress {:?}% avarage rate {:?}%\r",
                    block / percent_period,
                    0
                )
            } else {
                eprint!(
                    "compress {:?}% avarage rate {:?}%\r",
                    block / percent_period,
                    100 * write_pos / ((block * 0x800) as u64)
                );
            }
        }

        let mut iso_data = vec![];
        std::io::Read::by_ref(&mut fin)
            .take(block_size as u64)
            .read_to_end(&mut iso_data)
            .unwrap();

        let mut zso_data = compress(&iso_data);

        write_pos = set_align(&mut fout, write_pos, alignment, *pad);
        index_buf[block as usize] = (write_pos >> alignment) as u64;

        if 100 * zso_data.len() / iso_data.len() >= (*percent as usize) {
            zso_data = iso_data;
            index_buf[block as usize] |= 0x80000000 // Mark as plain
        } else if (index_buf[block as usize] & 0x80000000) > 1 {
            panic!("Align error, you have to increase align by 1 or CFW won't be able to read offset above 2 ** 31 bytes");
        }

        fout.write_all(&zso_data).unwrap();
        write_pos += zso_data.len() as u64;
        block += 1;
    }

    // Last position (total size)
    index_buf[block as usize] = (write_pos >> alignment) as u64;

    // Update index block
    fout.seek(SeekFrom::Start(header.len() as u64)).unwrap();
    for i in index_buf {
        fout.write_u32::<LittleEndian>(i as u32).unwrap();
    }

    println!(
        "ziso compress completed , total size = {:?} bytes , rate {:?}",
        write_pos,
        write_pos * 100 / total_bytes
    );

    drop(fin);
    fout.sync_all().unwrap();
    drop(fout);
}

fn main() {
    let args = command!()
        .arg(
            arg!(<input> "Input file to process")
            .value_parser(value_parser!(PathBuf))
        )
        .arg(
            arg!(<output> "Final output file")
            .value_parser(value_parser!(PathBuf))
        )
        .arg(
            arg!(-c --compress <level> "1-9 compress ISO to ZSO, use any non-zero number it has no effect. 0 decompress ZSO to ISO")
            .value_parser(value_parser!(u8))
        )
        .arg(
            arg!(-t --threshold <percent> "Compression Threshold (1-100)")
            .required(false)
            .value_parser(value_parser!(u8))
            .default_value("100")
        )
        .arg(
            arg!(-a --align <align> "Padding alignment 0=small/slow 6=fast/large")
            .required(false)
            .value_parser(value_parser!(i8))
            .default_value("0")
        )
        .arg(
            arg!(-p --padding <pad> "Padding byte")
            .required(false)
            .value_parser(value_parser!(char))
            .default_value("X")
        )
        .get_matches();

    let compress = args.get_one::<u8>("compress").unwrap();
    if compress == &0u8 {
        decompress_zso(
            args.get_one::<PathBuf>("input").unwrap(),
            args.get_one::<PathBuf>("output").unwrap(),
        );
    } else {
        compress_zso(
            args.get_one::<PathBuf>("input").unwrap(),
            args.get_one::<PathBuf>("output").unwrap(),
            args.get_one::<u8>("compress").unwrap(),
            args.get_one::<u8>("threshold").unwrap(),
            args.get_one::<i8>("align").unwrap(),
            args.get_one::<char>("padding").unwrap(),
        );
    }
}

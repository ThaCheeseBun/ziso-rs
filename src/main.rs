use std::fs::File;
use std::io::{prelude::*, Cursor, SeekFrom};
use std::path::PathBuf;

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use lz4_flex::block::{compress, decompress};

use clap::{arg, command, value_parser, ArgAction, ValueHint};

// ZSO format constants
const ZSO_MAGIC: u32 = 0x4F53495A;
const ZSO_HEADER_SIZE: u32 = 0x18;
const ZSO_VER: i8 = 1;

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
    let magic = fin.read_u32::<LittleEndian>().unwrap();
    let header_size = fin.read_u32::<LittleEndian>().unwrap();
    let total_bytes = fin.read_u64::<LittleEndian>().unwrap();
    let block_size = fin.read_u32::<LittleEndian>().unwrap();
    let ver = fin.read_i8().unwrap();
    let align = fin.read_i8().unwrap();

    fin.seek(SeekFrom::Current(2)).unwrap();

    return (magic, header_size, total_bytes, block_size, ver, align);
}

fn generate_zso_header(
    total_bytes: u64,
    block_size: u32,
    align: i8,
) -> [u8; ZSO_HEADER_SIZE as usize] {
    let mut data = Cursor::new([0; ZSO_HEADER_SIZE as usize]);
    data.write_u32::<LittleEndian>(ZSO_MAGIC).unwrap();
    data.write_u32::<LittleEndian>(ZSO_HEADER_SIZE).unwrap();
    data.write_u64::<LittleEndian>(total_bytes).unwrap();
    data.write_u32::<LittleEndian>(block_size).unwrap();
    data.write_i8(ZSO_VER).unwrap();
    data.write_i8(align).unwrap();
    return *data.get_ref();
}

fn lz4_decompress(compressed: &mut Vec<u8>, block_size: u32) -> Option<Vec<u8>> {
    let mut decompressed = None;

    loop {
        let dec = decompress(&compressed, block_size as usize);
        if Result::is_err(&dec) {
            compressed.remove(compressed.len() - 1);
        } else {
            decompressed = Some(dec.unwrap());
            break;
        }
    }

    return decompressed;
}

fn decompress_zso(fname_in: &PathBuf, fname_out: &PathBuf) {
    let (mut fin, mut fout) = open_input_output(&fname_in, &fname_out);

    let total_zso_bytes = fin.seek(SeekFrom::End(0)).unwrap();
    fin.seek(SeekFrom::Start(0)).unwrap();

    let reff = std::io::Read::by_ref(&mut fin);
    let (magic, header_size, total_bytes, block_size, ver, align) = read_zso_header(reff);

    if magic != ZSO_MAGIC || block_size == 0 || total_bytes == 0 || header_size != 24 || ver > 1 {
        panic!("ziso file format error");
    }
    let total_block = (total_bytes / block_size as u64) as usize;
    let mut index_buf = vec![];

    for _ in 0..=total_block {
        index_buf.push(fin.read_u32::<LittleEndian>().unwrap());
    }

    // show_zso_info
    eprintln!("Decompress '{:?}' to '{:?}'", fname_in, fname_out);
    eprintln!("Total File Size {:?} bytes", total_bytes);
    eprintln!("block size      {:?}  bytes", block_size);
    eprintln!("total blocks    {:?}  blocks", total_block);
    eprintln!("index align     {:?}", align);

    let mut block: usize = 0;
    let percent_period = total_block / 100;
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

        let read_size = {
            if plain != 0 {
                block_size
            } else {
                let index2 = index_buf[block + 1] & 0x7fffffff;
                // Have to read more bytes if align was set
                if block == total_block - 1 {
                    (total_zso_bytes as u32) - read_pos
                } else {
                    (index2 - index) << (align)
                }
            }
        };

        fin.seek(SeekFrom::Start(read_pos as u64)).unwrap();

        let mut zso_data = vec![];
        std::io::Read::by_ref(&mut fin)
            .take(read_size as u64)
            .read_to_end(&mut zso_data)
            .unwrap();

        let dec_data = {
            if plain != 0 {
                zso_data
            } else {
                //let dec = decompress(&zso_data, block_size as usize);
                let dec = lz4_decompress(&mut zso_data, block_size);
                //let dec = decompress(&zso_data, Some(block_size as i32));
                dec.expect(&format!(
                    "{:?} block: 0x{:x} {:?}",
                    block, read_pos, read_size
                ))
            }
        };

        if dec_data.len() != block_size as usize {
            panic!("{:?} block: 0x{:x} {:?}", block, read_pos, read_size);
        }
        fout.write(&dec_data).unwrap();
        block += 1;
    }

    drop(fin);
    drop(fout);
    eprintln!("ziso decompress completed");
}

fn set_align(fout: &mut File, pos: u64, align: i8, padding: char) -> u64 {
    let mut write_pos = pos;
    if (write_pos % (1 << align)) > 0 {
        let align_len = (1 << align) - write_pos % (1 << align);
        for _ in 0..align_len {
            fout.write(&[padding as u8]).unwrap();
        }
        write_pos += align_len;
    }
    write_pos
}

fn compress_zso(fname_in: &PathBuf, fname_out: &PathBuf, percent: &u8, align_arg: &i8, pad: &char) {
    let (mut fin, mut fout) = open_input_output(&fname_in, &fname_out);
    let total_bytes = fin.seek(SeekFrom::End(0)).unwrap();
    fin.seek(SeekFrom::Start(0)).unwrap();

    let block_size = 0x800;

    // We have to use alignment on any ZSO files which > 2GB, for MSB bit of index as the plain indicator
    // If we don't then the index can be larger than 2GB, which its plain indicator was improperly set
    let align = if *align_arg == 0 {
        (total_bytes / u64::pow(2, 31)) as i8
    } else {
        *align_arg
    };

    let header = generate_zso_header(total_bytes, block_size, align);
    fout.write_all(&header).unwrap();

    let total_block = (total_bytes / block_size as u64) as usize;

    let mut index_buf: Vec<u32> = vec![];
    index_buf.resize((total_block + 1) as usize, 0u32);

    for _ in 0..index_buf.len() {
        fout.write(b"\x00\x00\x00\x00").unwrap();
    }

    eprintln!("Compress '{:?}' to '{:?}'", &fname_in, &fname_out);
    eprintln!("Total File Size {:?} bytes", total_bytes);
    eprintln!("block size      {:?}  bytes", block_size);
    eprintln!("index align     {:?}", 1 << align);

    let mut write_pos = fout.seek(SeekFrom::Current(0)).unwrap();
    let percent_period = total_block / 100;
    let mut percent_cnt = 0;

    let mut block = 0usize;
    while block < total_block {
        percent_cnt += 1;

        if percent_cnt >= percent_period && percent_period != 0 {
            percent_cnt = 0;

            if block == 0 {
                eprint!(
                    "compress {:?}% average rate {:?}%\r",
                    block / percent_period,
                    0
                );
            } else {
                eprint!(
                    "compress {:?}% average rate {:?}%\r",
                    block / percent_period,
                    100 * write_pos / (block * block_size as usize) as u64
                );
            }
        }

        let mut iso_data = vec![];
        std::io::Read::by_ref(&mut fin)
            .take(block_size as u64)
            .read_to_end(&mut iso_data)
            .unwrap();

        let mut zso_data = compress(&iso_data);

        write_pos = set_align(&mut fout, write_pos, align, *pad);
        index_buf[block] = (write_pos >> align) as u32;

        if 100 * zso_data.len() / iso_data.len() >= *percent as usize {
            zso_data = iso_data;
            index_buf[block] |= 0x80000000; // Mark as plain
        } else if index_buf[block] & 0x80000000 > 0 {
            panic!("Align error, you have to increase align by 1 or CFW won't be able to read offset above 2 ** 31 bytes");
        }

        fout.write_all(&zso_data).unwrap();
        write_pos += zso_data.len() as u64;
        block += 1;
    }

    // Last position (total size)
    index_buf[block] = (write_pos >> align) as u32;

    // Update index block
    fout.seek(SeekFrom::Start(header.len() as u64)).unwrap();
    for i in index_buf {
        fout.write_u32::<LittleEndian>(i).unwrap();
    }

    println!(
        "ziso compress completed , total size = {:?} bytes , rate {:?}%",
        write_pos,
        (write_pos * 100 / total_bytes)
    );

    drop(fin);
    drop(fout);
}

fn main() {
    let args = command!()
        // Files I/O
        .arg(
            arg!(<input> "Input file to process")
                .value_parser(value_parser!(PathBuf))
                .value_hint(ValueHint::FilePath),
        )
        .arg(
            arg!(<output> "Final output file")
                .value_parser(value_parser!(PathBuf))
                .value_hint(ValueHint::FilePath),
        )
        // Compression
        .arg(
            arg!(-c --compress "Compress ISO to ZSO")
                .action(ArgAction::SetTrue)
                .required_unless_present("decompress")
                .conflicts_with("decompress"),
        )
        .arg(
            arg!(-t --threshold <percent> "Compression Threshold (1-100)")
                .value_parser(value_parser!(u8).range(1..101))
                .default_value("100")
                .required(false),
        )
        .arg(
            arg!(-a --align <align> "Padding alignment 0=small/slow 6=fast/large")
                .value_parser(value_parser!(i8).range(0..6))
                .default_value("0")
                .required(false),
        )
        .arg(
            arg!(-p --padding <pad> "Padding byte")
                .value_parser(value_parser!(char))
                .default_value("X")
                .required(false),
        )
        // Decompression
        .arg(
            arg!(-d --decompress "Decompress ZSO to ISO")
                .action(ArgAction::SetTrue)
                .required_unless_present("compress")
                .conflicts_with("compress"),
        )
        .get_matches();

    if *args.get_one::<bool>("compress").unwrap() {
        compress_zso(
            args.get_one::<PathBuf>("input").unwrap(),
            args.get_one::<PathBuf>("output").unwrap(),
            args.get_one::<u8>("threshold").unwrap(),
            args.get_one::<i8>("align").unwrap(),
            args.get_one::<char>("padding").unwrap(),
        );
    } else if *args.get_one::<bool>("decompress").unwrap() {
        decompress_zso(
            args.get_one::<PathBuf>("input").unwrap(),
            args.get_one::<PathBuf>("output").unwrap(),
        );
    } else {
        panic!("no");
    }
}

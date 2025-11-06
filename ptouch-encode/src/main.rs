use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use png;


const ESC: u8 = 0x1B;


#[derive(Parser)]
struct Opts {
    #[arg(short = 'c', long)]
    pub auto_cut: bool,

    #[arg(short = 'm', long)]
    pub mirror_print: bool,

    #[arg(short = 'd', long)]
    pub draft: bool,

    #[arg(short = 'H', long)]
    pub half_cut: bool,

    #[arg(short = 'C', long)]
    pub no_chain: bool,

    #[arg(short = 's', long)]
    pub special_tape: bool,

    #[arg(short = 'R', long)]
    pub hi_res: bool,

    #[arg(short = 'B', long)]
    pub dont_clear_print_buffer: bool,

    #[arg(short = 'e', long, default_value = "0")]
    pub cut_every: u8,

    #[arg(short = 'f', long, default_value = "0")]
    pub feed: u16,

    #[arg(short = 'w', long)]
    pub width_mm: u8,

    #[arg(required = true)]
    pub png_paths: Vec<PathBuf>,

    pub pt_path: PathBuf,
}


fn pack_bits(bytes: &[u8]) -> Vec<u8> {
    fn take_repeated(slice: &[u8]) -> &[u8] {
        let mut i = 0;
        let b = match slice.get(i) {
            Some(bb) => bb,
            None => return &[],
        };
        i += 1;

        while let Some(b2) = slice.get(i) {
            if b2 == b {
                i += 1;
            } else {
                break;
            }
        }

        &slice[..i]
    }

    fn take_verbatim(slice: &[u8]) -> &[u8] {
        let mut i = 0;
        let mut prev_b = match slice.get(i) {
            Some(pb) => pb,
            None => return &[],
        };
        i += 1;

        while let Some(next_b) = slice.get(i) {
            if prev_b != next_b {
                i += 1;
                prev_b = next_b;
            } else {
                break;
            }
        }

        &slice[..i]
    }

    let mut ret = Vec::with_capacity(2*bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let repeated_slice = take_repeated(&bytes[i..]);
        let verbatim_slice = take_verbatim(&bytes[i..]);
        if repeated_slice.len() > verbatim_slice.len() {
            assert!(repeated_slice.len() > 1);

            i += repeated_slice.len();

            // can't do more than 128
            let repeat_count = repeated_slice.len().min(128);
            let repeat_byte_i16: i16 = 1 - i16::try_from(repeat_count).unwrap();
            let repeat_byte_i8: i8 = repeat_byte_i16.try_into().unwrap();
            let repeat_bytes = repeat_byte_i8.to_ne_bytes();

            ret.push(repeat_bytes[0]);
            ret.push(repeated_slice[0]);
        } else {
            assert!(verbatim_slice.len() > 0);

            i += verbatim_slice.len();

            let verbatim_count = verbatim_slice.len().min(128);
            let verbatim_byte_i8: i8 = (verbatim_count - 1).try_into().unwrap();
            let verbatim_bytes = verbatim_byte_i8.to_ne_bytes();

            ret.push(verbatim_bytes[0]);
            ret.extend(verbatim_slice);
        }
    }
    ret
}

fn main() -> ExitCode {
    let opts = Opts::parse();
    if opts.png_paths.len() == 0 {
        panic!("at least one PNG file must be given");
    }

    let mut pages = Vec::new();
    let mut width = None;
    for (png_index, png_path) in opts.png_paths.iter().enumerate() {
        let f = File::open(png_path)
            .expect("failed to open PNG file");
        let f_buf = BufReader::new(f);
        let dec = png::Decoder::new(f_buf);
        let mut reader = dec.read_info()
            .expect("failed to decode PNG file");
        if let Some(w) = width {
            if reader.info().width != w {
                panic!("PNG at index {} has different width {} (index 0: width {})", png_index, reader.info().width, w);
            }
        } else {
            width = Some(reader.info().width);
        }
        if reader.info().bit_depth != png::BitDepth::One {
            panic!("PNG bit depth is not 1");
        }
        let mut rows = Vec::new();
        loop {
            let ols = reader.output_line_size(width.unwrap())
                .expect("failed to obtain output line size");
            let mut buf = vec![0u8; ols];
            let row_opt = reader.read_row(&mut buf)
                .expect("failed to read row");
            if row_opt.is_none() {
                break;
            }

            // flip the bits
            // (PNG: 1 (white) = no marker, 0 (black) = marker;
            //  P-Touch: 0 = no marker, 1 = marker)
            // but flip only those that are valid
            let mut remaining_width = width.unwrap();
            for b in &mut buf {
                let this_bits = remaining_width.min(8);
                for i in 0..this_bits {
                    *b ^= 1 << ((8 - 1) - i);
                }
                remaining_width -= this_bits;
            }

            if buf.iter().all(|b| *b == 0x00) {
                rows.push(vec![]);
            } else {
                let packed = pack_bits(&buf);
                rows.push(packed);
            }
        }

        // flip the rows
        rows.reverse();

        pages.push(rows);
    }

    // let's go
    let mut out_file = File::create(&opts.pt_path)
        .expect("failed to create output file");
    let mut out_buffy = BufWriter::new(&mut out_file);

    // 200 bytes invalidate
    let buf = [0u8; 10];
    for _ in 0..(200/10) {
        out_buffy.write_all(&buf)
            .expect("failed to write invalidate bytes");
    }

    // reset
    out_buffy.write_all(&[ESC, b'@'])
        .expect("failed to write reset");

    // switch to raster mode (mode 1)
    out_buffy.write_all(&[ESC, b'i', b'a', 0x01])
        .expect("failed to write switch-to-raster-mode");

    // auto-cut? mirror print?
    let mut mode_byte = 0u8;
    if opts.auto_cut {
        mode_byte |= 0x40;
    }
    if opts.mirror_print {
        mode_byte |= 0x80;
    }
    out_buffy.write_all(&[ESC, b'i', b'M', mode_byte])
        .expect("failed to write options");

    // all the other settings
    let mut setting_byte = 0u8;
    if opts.draft {
        setting_byte |= 0x01;
    }
    if opts.half_cut {
        setting_byte |= 0x04;
    }
    if opts.no_chain {
        setting_byte |= 0x08;
    }
    if opts.special_tape {
        setting_byte |= 0x10;
    }
    if opts.hi_res {
        setting_byte |= 0x40;
    }
    if opts.dont_clear_print_buffer {
        setting_byte |= 0x80;
    }
    out_buffy.write_all(&[ESC, b'i', b'K', setting_byte])
        .expect("failed to write settings");

    out_buffy.write_all(&[ESC, b'i', b'A', opts.cut_every])
        .expect("failed to write cut-every setting");

    let feed_buf = opts.feed.to_le_bytes();
    out_buffy.write_all(&[ESC, b'i', b'd', feed_buf[0], feed_buf[1]])
        .expect("failed to write feed setting");

    const SET_PACKBITS_COMPRESSION: &[u8] = &[b'M', 0x02];
    out_buffy.write_all(&SET_PACKBITS_COMPRESSION)
        .expect("failed to write compression instruction");

    for (page_index, page_rows) in pages.iter().enumerate() {
        let page_byte = if page_index == pages.len() - 1 {
            // last (or single) page
            2
        } else if page_index == 0 {
            // first page
            0
        } else {
            // middle page
            1
        };

        let line_count_usize = page_rows.len();
        let line_count_u32: u32 = line_count_usize.try_into().unwrap();
        let line_count_bytes = line_count_u32.to_le_bytes();
        out_buffy.write_all(&[
            ESC, b'i', b'z',
            0x04 | 0x80, // media width is given, printer recovery is on
            0x00, // media type (ignored because 0x02 presence flag is missing)
            opts.width_mm,
            0x00, // length ("endless")
            line_count_bytes[0],
            line_count_bytes[1],
            line_count_bytes[2],
            line_count_bytes[3],
            page_byte,
            0, // always zero
        ])
            .expect("failed to write page info");

        for row in page_rows {
            if row.len() == 0 {
                out_buffy.write_all(&[b'Z'])
                    .expect("failed to write empty row");
            } else {
                let data_length: u16 = row.len().try_into().unwrap();
                let data_length_bytes = data_length.to_le_bytes();
                out_buffy.write_all(&[b'G', data_length_bytes[0], data_length_bytes[1]])
                    .expect("failed to write row metadata");
                out_buffy.write_all(row)
                    .expect("failed to write row data");
            }
        }

        if page_index == pages.len() - 1 {
            // print and feed
            out_buffy.write_all(&[0x1A])
                .expect("failed to write print-and-feed command");
        } else {
            // print
            out_buffy.write_all(&[0x0C])
                .expect("failed to write print command");
        };
    }

    out_buffy.flush()
        .expect("failed to flush output file");

    ExitCode::SUCCESS
}

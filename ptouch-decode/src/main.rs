use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::ExitCode;


const ESC: u8 = 0x1B;


trait BufReadExt {
    /// Skips over all bytes that equal the given byte.
    ///
    /// Returns `Ok(true)` if a byte with a different value was reached and `Ok(false)` on EOF. The
    /// byte with a different value is not consumed and is read during the next call to one of the
    /// `read*()` functions.
    fn skip_while(&mut self, byte: u8) -> Result<bool, io::Error>;
}
impl<T: BufRead> BufReadExt for T {
    fn skip_while(&mut self, byte: u8) -> Result<bool, io::Error> {
        loop {
            // fill the reader buffer
            let my_buf = self.fill_buf()?;
            if my_buf.len() == 0 {
                // EOF reached
                return Ok(false);
            }
            let until_pos = my_buf
                .iter()
                .position(|b| *b != byte)
                .unwrap_or(my_buf.len());
            if until_pos == 0 {
                break;
            }
            self.consume(until_pos);
        }
        Ok(true)
    }
}


#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum AnnouncedPage {
    #[default] BeforeFirst,
    First,
    Other,
    Last,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum CompressionMode {
    #[default] Raw,
    PackBits,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum LabelPart {
    LabelData { rows: Vec<Vec<u8>> },
    Print,
    PrintFeed,
}


fn unpack_bits(buf: &[u8]) -> Vec<u8> {
    let mut ret = Vec::new();

    let mut iter = buf.iter();
    while let Some(instruction_u8) = iter.next() {
        let instruction = i8::from_le_bytes([*instruction_u8]);
        if instruction >= 0 {
            let literal_byte_count = (1 + instruction).try_into().unwrap();
            ret.reserve(literal_byte_count);
            for _ in 0..literal_byte_count {
                let literal_byte = iter.next()
                    .expect("short read of literal bytes");
                ret.push(*literal_byte);
            }
        } else if instruction == -128 {
            // skip
        } else {
            // repeated byte
            let repeat_count = usize::try_from(1 - instruction).unwrap();
            assert!(repeat_count >= 2);
            let value = iter.next()
                .expect("repeat without repeated value");
            ret.reserve(repeat_count);
            for _ in 0..repeat_count {
                ret.push(*value);
            }
        }
    }

    ret
}


fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().collect();
    let prog_name = args
        .get(0)
        .map(|pn | pn.display().to_string())
        .unwrap_or_else(|| "ptouch-decode".to_owned());
    if args.len() != 3 {
        eprintln!("Usage: {} PRINTDATA PNGDATA", prog_name);
        return ExitCode::FAILURE;
    }
    let print_data_path = Path::new(&args[1]);
    let print_data_file = File::open(print_data_path)
        .expect("file not found");
    let mut print_data_buffering = BufReader::new(print_data_file);

    // read 200 bytes to ensure we have an invalidate command
    let mut invalidate_buf = vec![0u8; 200];
    print_data_buffering.read_exact(&mut invalidate_buf)
        .expect("failed to read invalidate command");
    if invalidate_buf.iter().any(|b| *b != 0x00) {
        panic!("print data does not start with a valid invalidate command (200 zero bytes)");
    }

    // skip over all following 0 bytes
    print_data_buffering.skip_while(0x00)
        .expect("failed to fast-forward over long invalidate command");

    // read 2 bytes to ensure we start with an initialize command
    let mut init_buf = [0u8; 2];
    print_data_buffering.read_exact(&mut init_buf)
        .expect("failed to read init command");
    if init_buf[0] != ESC || init_buf[1] != b'@' {
        panic!("first command is not init but {:#04X} {:#04X}", init_buf[0], init_buf[1]);
    }

    // parse commands
    let mut raster_mode = false;
    let mut media_type = None;
    let mut media_width = None;
    let mut media_length = None;
    let mut raster_number = None;
    let mut printer_recovery = false;
    let mut page_state = AnnouncedPage::BeforeFirst;
    let mut auto_cut = None;
    let mut mirror_print = None;
    let mut draft = None;
    let mut half_cut = None;
    let mut no_chain = None;
    let mut special_tape = None;
    let mut hi_res = None;
    let mut dont_clean_print_buffer = None;
    let mut feed_amount = None;
    let mut cut_each_n_labels = None;
    let mut compression_mode = CompressionMode::Raw;
    let mut pixel_data_width = 0;
    let mut parts = Vec::new();
    let mut rows = Vec::new();
    loop {
        let mut buf = [0u8];
        match print_data_buffering.read(&mut buf) {
            Ok(1) => {},
            Ok(0) => break, // EOF
            Ok(n) => unreachable!(".read() read {} bytes into a 1-byte buffer?!", n),
            Err(e) => panic!("failed to read next command: {}", e),
        }
        match buf[0] {
            ESC => {
                // control command
                let mut esc_buf = [0u8];
                print_data_buffering.read_exact(&mut esc_buf)
                    .expect("failed to read type of escape");
                match esc_buf[0] {
                    b'@' => {
                        // reinitialize again?

                        // raster_mode does not change
                        media_type = None;
                        media_width = None;
                        media_length = None;
                        raster_number = None;
                        page_state = AnnouncedPage::BeforeFirst;
                    },
                    b'i' => {
                        // mode settings
                        let mut set_buf = [0u8];
                        print_data_buffering.read_exact(&mut set_buf)
                            .expect("failed to read type of ESC i");
                        match set_buf[0] {
                            b'S' => {
                                // status info request
                                // nothing to do for us here
                            },
                            b'a' => {
                                // switch print data language
                                let mut lang_buf = [0u8];
                                print_data_buffering.read_exact(&mut lang_buf)
                                    .expect("failed to read print data language to which to switch");
                                match lang_buf[0] {
                                    0 => panic!("attempting to switch to ESC/P which we do not support"),
                                    1 => {
                                        raster_mode = true;
                                    },
                                    3 => panic!("attempting to switch to P-touch Template Mode which we do not support"),
                                    other => panic!("unknown print data language {:#04X}", other),
                                }
                            },
                            b'z' => {
                                // print information
                                // always followed by 10 bytes, whose validity is governed by the first byte
                                let mut info_buf = [0u8; 10];
                                print_data_buffering.read_exact(&mut info_buf)
                                    .expect("failed to read print information command data");
                                if info_buf[0] & 0x02 != 0 {
                                    media_type = Some(info_buf[1]);
                                }
                                if info_buf[0] & 0x04 != 0 {
                                    media_width = Some(info_buf[2]);
                                }
                                if info_buf[0] & 0x08 != 0 {
                                    media_length = Some(info_buf[3]);
                                }
                                raster_number = Some(u32::from_le_bytes(info_buf[4..8].try_into().unwrap()));
                                match info_buf[8] {
                                    0 => {
                                        // announcing page: first
                                        if page_state == AnnouncedPage::BeforeFirst {
                                            page_state = AnnouncedPage::First;
                                        } else {
                                            panic!("announcing first page in state {:?}", page_state);
                                        }
                                    },
                                    1 => {
                                        // announcing page: midway
                                        if page_state == AnnouncedPage::First || page_state == AnnouncedPage::Other {
                                            page_state = AnnouncedPage::Other;
                                        } else {
                                            panic!("announcing midway page in state {:?}", page_state);
                                        }
                                    },
                                    2 => {
                                        // announcing page: last
                                        // (also used if there is only one page)
                                        if page_state != AnnouncedPage::Last {
                                            page_state = AnnouncedPage::Last;
                                        } else {
                                            panic!("announcing last page in state {:?}", page_state);
                                        }
                                    },
                                    other => panic!("unknown page announcement byte {:#04X}", other),
                                }
                                // info_buf[9] is apparently always 0
                            },
                            b'M' => {
                                // mode
                                let mut mode_buf = [0u8];
                                print_data_buffering.read_exact(&mut mode_buf)
                                    .expect("failed to read print mode command data");
                                auto_cut = Some((mode_buf[0] & 0x40) != 0);
                                mirror_print = Some((mode_buf[0] & 0x80) != 0);
                            },
                            b'A' => {
                                // cut after sets of how many labels?
                                let mut count_buf = [0u8];
                                print_data_buffering.read_exact(&mut count_buf)
                                    .expect("failed to read count data");
                                cut_each_n_labels = Some(count_buf[0]);
                            },
                            b'K' => {
                                // advanced settings
                                let mut settings_buf = [0u8];
                                print_data_buffering.read_exact(&mut settings_buf)
                                    .expect("failed to read print mode command data");
                                draft = Some((settings_buf[0] & 0x01) != 0);
                                // 0x02 unused
                                half_cut = Some((settings_buf[0] & 0x04) != 0);
                                no_chain = Some((settings_buf[0] & 0x08) != 0);
                                special_tape = Some((settings_buf[0] & 0x10) != 0);
                                // 0x20 unused
                                hi_res = Some((settings_buf[0] & 0x40) != 0);
                                dont_clean_print_buffer = Some((settings_buf[0] & 0x80) != 0);
                            },
                            b'd' => {
                                // feed amount
                                let mut value_buf = [0u8; 2];
                                print_data_buffering.read_exact(&mut value_buf)
                                    .expect("failed to read feed amount");
                                feed_amount = Some(u16::from_le_bytes(value_buf));
                            },
                            b'!' => {
                                // auto status notification mode
                            },
                            other => panic!("unexpected ESC i command {:#04X}", other),
                        }
                    },
                    other => panic!("unexpected ESC command {:#04X}", other),
                }
            },
            b'M' => {
                // select compression mode
                let mut mode_buf = [0u8];
                print_data_buffering.read_exact(&mut mode_buf)
                    .expect("failed to read select compression mode data");
                match mode_buf[0] {
                    0x00 => {
                        compression_mode = CompressionMode::Raw;
                    },
                    0x02 => {
                        compression_mode = CompressionMode::PackBits;
                    },
                    other => panic!("unsupported compression mode: {:#04X}", other),
                }
            },
            b'G' => {
                // raster graphics transfer
                if !raster_mode {
                    panic!("raster graphics transfer without raster mode entered");
                }
                let mut byte_count_buf = [0u8; 2];
                print_data_buffering.read_exact(&mut byte_count_buf)
                    .expect("failed to read raster graphics transfer length");
                let byte_count = usize::from(u16::from_le_bytes(byte_count_buf));
                let mut raster_buf = vec![0u8; byte_count];
                print_data_buffering.read_exact(&mut raster_buf)
                    .expect("failed to read raster graphics data");

                // convert into pixelzzz
                let raw_buf = if compression_mode == CompressionMode::PackBits {
                    unpack_bits(&raster_buf)
                } else {
                    raster_buf
                };
                pixel_data_width = pixel_data_width.max(raw_buf.len() * 8);
                let mut row: Vec<u8> = Vec::with_capacity(pixel_data_width);
                for byte in &raw_buf {
                    for bit_index in (0..8).rev() {
                        if (*byte & (1 << bit_index)) == 0 {
                            row.push(0x00);
                        } else {
                            row.push(0x01);
                        }
                    }
                }
                rows.push(row);
            },
            b'Z' => {
                // zero raster graphics
                if !raster_mode {
                    panic!("zero raster graphics transfer without raster mode entered");
                }
                rows.push(Vec::with_capacity(0));
            },
            0x0C => {
                // form feed = print
                let old_rows = std::mem::replace(&mut rows, Vec::new());
                parts.push(LabelPart::LabelData { rows: old_rows });
                parts.push(LabelPart::Print);
            },
            0x1A => {
                // substitute = print with feeding
                let old_rows = std::mem::replace(&mut rows, Vec::new());
                parts.push(LabelPart::LabelData { rows: old_rows });
                parts.push(LabelPart::PrintFeed);
            },
            other => panic!("unexpected command byte {:#04X}", other),
        }
    }

    parts.push(LabelPart::LabelData { rows });

    // update the parts to match the image width and calculate the image height
    let mut height = 0;
    for part in &mut parts {
        match part {
            LabelPart::LabelData { rows } => {
                for row in rows.iter_mut() {
                    assert!(row.len() <= pixel_data_width);
                    row.resize(pixel_data_width, 0x00);
                }
                height += rows.len();
            },
            LabelPart::Print|LabelPart::PrintFeed => {
                height += 1;
            },
        }
    }

    // output as PNG
    let mut png_buf = Vec::new();

    {
        let mut png_enc = png::Encoder::new(
            &mut png_buf,
            pixel_data_width.try_into().unwrap(),
            height.try_into().unwrap(),
        );
        png_enc.set_color(png::ColorType::Indexed);
        png_enc.set_depth(png::BitDepth::Eight);
        png_enc.set_palette(&[
            0xFF, 0xFF, 0xFF, // 0 = white (medium)
            0x00, 0x00, 0x00, // 1 = black (marker)
            0xFF, 0x00, 0x00, // 2 = red (print)
            0x00, 0x00, 0xFF, // 3 = blue (print+feed)
        ]);
        let mut png_wr = png_enc.write_header()
            .expect("failed to write PNG header");
        let mut png_stream_wr = png_wr.stream_writer()
            .expect("failed to obtain stream writer");
        for part in &parts {
            match part {
                LabelPart::LabelData { rows } => {
                    for row in rows {
                        png_stream_wr.write_all(row)
                            .expect("failed to write into PNG stream");
                    }
                },
                LabelPart::Print => {
                    // row of 0x02
                    let row_0x02 = vec![0x02; pixel_data_width];
                    png_stream_wr.write_all(&row_0x02)
                        .expect("failed to write into PNG stream");
                },
                LabelPart::PrintFeed => {
                    // row of 0x03
                    let row_0x03 = vec![0x03; pixel_data_width];
                    png_stream_wr.write_all(&row_0x03)
                        .expect("failed to write into PNG stream");
                },
            }
        }
        // done
        png_stream_wr.finish()
            .expect("failed to finish PNG stream encoding");
        png_wr.finish()
            .expect("failed to finish PNG encoding");
    }

    std::fs::write(&args[2], &png_buf)
        .expect("failed to write PNG");

    ExitCode::SUCCESS
}

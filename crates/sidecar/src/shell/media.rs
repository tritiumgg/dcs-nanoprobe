//! The image and sound files a campaign carries into its shells
//! (spec/persistence.md, "Mission files"). Each file is read past its magic
//! bytes before it is stored: a PNG's `IHDR`, a JPEG's start of frame, an Ogg
//! stream's Vorbis identification header, a WAV file's `fmt ` chunk. The
//! Mission Editor offers PNG and JPEG for images and Ogg and WAV for sounds
//! (ED-CODE, `MissionEditor/modules/FileDialogFilters.lua`, DCS 2.9.31.29434).

/// The largest file, in bytes.
pub const FILE_MAX: usize = 8 * 1024 * 1024;

/// The largest width and height of an image, in pixels. A briefing image is
/// shown scaled to the briefing window, and a kneeboard page is a page: 8192
/// is well past any screen and keeps a decoded image under 256 MiB.
pub const IMAGE_SIDE_MAX: u32 = 8192;

/// The sample rates a sound may have, in Hz: telephone quality to studio.
pub const RATE_MIN: u32 = 8000;
/// See [`RATE_MIN`].
pub const RATE_MAX: u32 = 192_000;

/// The most channels a sound may have.
pub const CHANNELS_MAX: u32 = 8;

/// A media type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Media {
    /// PNG, `.png`.
    Png,
    /// JPEG, `.jpg` or `.jpeg`.
    Jpeg,
    /// Ogg Vorbis, `.ogg`.
    Ogg,
    /// PCM WAV, `.wav`.
    Wav,
}

impl Media {
    /// The type a file name's extension names, ignoring case; `None` for
    /// any other.
    pub fn of_name(name: &str) -> Option<Media> {
        let (_, ext) = name.rsplit_once('.')?;
        match ext.to_ascii_lowercase().as_str() {
            "png" => Some(Media::Png),
            "jpg" | "jpeg" => Some(Media::Jpeg),
            "ogg" => Some(Media::Ogg),
            "wav" => Some(Media::Wav),
            _ => None,
        }
    }

    /// Whether it is an image.
    pub fn is_image(self) -> bool {
        matches!(self, Media::Png | Media::Jpeg)
    }

    /// Whether it is a sound.
    pub fn is_sound(self) -> bool {
        !self.is_image()
    }

    /// The type's name in messages.
    pub fn label(self) -> &'static str {
        match self {
            Media::Png => "PNG",
            Media::Jpeg => "JPEG",
            Media::Ogg => "Ogg Vorbis",
            Media::Wav => "PCM WAV",
        }
    }
}

/// Check that `bytes` is a file of type `media` within the limits. The
/// error says what is wrong.
pub fn check(media: Media, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > FILE_MAX {
        return Err(format!("a file is at most {FILE_MAX} bytes"));
    }
    let r = match media {
        Media::Png => png(bytes),
        Media::Jpeg => jpeg(bytes),
        Media::Ogg => ogg(bytes),
        Media::Wav => wav(bytes),
    };
    r.map_err(|e| format!("not a {} file: {e}", media.label()))
}

fn u16_be(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_be(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u16_le(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_le(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn side(what: &str, width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err(format!("the {what} has no pixels"));
    }
    if width > IMAGE_SIDE_MAX || height > IMAGE_SIDE_MAX {
        return Err(format!(
            "{width} by {height} pixels; an image is at most {IMAGE_SIDE_MAX} on a side"
        ));
    }
    Ok(())
}

/// CRC-32 as PNG computes it (ISO 3309).
fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 == 1 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

fn png(b: &[u8]) -> Result<(), String> {
    const SIG: &[u8] = b"\x89PNG\r\n\x1a\n";
    if !b.starts_with(SIG) {
        return Err("no PNG signature".into());
    }
    // The first chunk is IHDR, 13 bytes.
    if u32_be(b, 8) != Some(13) || b.get(12..16) != Some(b"IHDR") {
        return Err("the first chunk is not IHDR".into());
    }
    let data = b.get(16..29).ok_or("IHDR is cut short")?;
    let crc = u32_be(b, 29).ok_or("IHDR is cut short")?;
    if crc32(&b[12..29]) != crc {
        return Err("the IHDR checksum is wrong".into());
    }
    let width = u32_be(data, 0).unwrap_or(0);
    let height = u32_be(data, 4).unwrap_or(0);
    let (depth, color) = (data[8], data[9]);
    let ok = match color {
        0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
        3 => matches!(depth, 1 | 2 | 4 | 8),
        2 | 4 | 6 => matches!(depth, 8 | 16),
        _ => false,
    };
    if !ok {
        return Err(format!("bit depth {depth} with color type {color}"));
    }
    if data[10] != 0 || data[11] != 0 || data[12] > 1 {
        return Err("an unknown compression, filter or interlace method".into());
    }
    side("image", width, height)
}

fn jpeg(b: &[u8]) -> Result<(), String> {
    if !b.starts_with(&[0xFF, 0xD8]) {
        return Err("no start-of-image marker".into());
    }
    let mut i = 2;
    loop {
        if b.get(i) != Some(&0xFF) {
            return Err("a segment does not start with a marker".into());
        }
        // Fill bytes before a marker.
        while b.get(i) == Some(&0xFF) {
            i += 1;
        }
        let marker = *b.get(i).ok_or("no start of frame")?;
        i += 1;
        match marker {
            // Markers with no length.
            0x01 | 0xD0..=0xD7 => continue,
            0xD8 | 0xD9 | 0xDA | 0x00 => {
                return Err("no start of frame before the image data".into());
            }
            _ => {}
        }
        let len = usize::from(u16_be(b, i).ok_or("a segment is cut short")?);
        if len < 2 || b.len() < i + len {
            return Err("a segment is cut short".into());
        }
        // SOF0 to SOF15, less DHT (C4), JPG (C8) and DAC (CC).
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            if len < 8 {
                return Err("the start of frame is cut short".into());
            }
            let height = u32::from(u16_be(b, i + 3).unwrap_or(0));
            let width = u32::from(u16_be(b, i + 5).unwrap_or(0));
            let components = b[i + 7];
            if !matches!(components, 1 | 3 | 4) {
                return Err(format!("{components} color components"));
            }
            return side("image", width, height);
        }
        i += len;
    }
}

fn ogg(b: &[u8]) -> Result<(), String> {
    // The first page: "OggS", version 0, beginning of stream.
    if !b.starts_with(b"OggS") {
        return Err("no Ogg page".into());
    }
    if b.get(4) != Some(&0) {
        return Err("an unknown Ogg version".into());
    }
    if b.get(5).is_none_or(|t| t & 0x02 == 0) {
        return Err("the first page does not begin a stream".into());
    }
    let segments = usize::from(*b.get(26).ok_or("the page is cut short")?);
    let lacing = b.get(27..27 + segments).ok_or("the page is cut short")?;
    let start = 27 + segments;
    let packet_len: usize = lacing
        .iter()
        .take_while(|&&l| l == 255)
        .map(|_| 255usize)
        .sum::<usize>()
        + lacing
            .iter()
            .find(|&&l| l != 255)
            .map_or(0, |&l| usize::from(l));
    let p = b
        .get(start..start + packet_len)
        .ok_or("the page is cut short")?;
    // The Vorbis identification header: type 1, "vorbis", version 0,
    // channels, rate, three bitrates, block sizes, framing bit.
    if p.len() < 30 || p[0] != 1 || &p[1..7] != b"vorbis" {
        return Err("the stream is not Vorbis".into());
    }
    if u32_le(p, 7) != Some(0) {
        return Err("an unknown Vorbis version".into());
    }
    let channels = u32::from(p[11]);
    let rate = u32_le(p, 12).unwrap_or(0);
    if p[29] & 1 == 0 {
        return Err("the identification header has no framing bit".into());
    }
    sound(channels, rate)
}

fn sound(channels: u32, rate: u32) -> Result<(), String> {
    if channels == 0 || channels > CHANNELS_MAX {
        return Err(format!(
            "{channels} channels; a sound has 1 to {CHANNELS_MAX}"
        ));
    }
    if !(RATE_MIN..=RATE_MAX).contains(&rate) {
        return Err(format!(
            "a sample rate of {rate} Hz; a sound has {RATE_MIN} to {RATE_MAX}"
        ));
    }
    Ok(())
}

fn wav(b: &[u8]) -> Result<(), String> {
    if b.get(0..4) != Some(b"RIFF") || b.get(8..12) != Some(b"WAVE") {
        return Err("no RIFF WAVE header".into());
    }
    let mut i = 12;
    loop {
        let id = b.get(i..i + 4).ok_or("no fmt chunk")?;
        let len = u32_le(b, i + 4).ok_or("no fmt chunk")? as usize;
        let body = i + 8;
        if id == b"fmt " {
            let f = b
                .get(body..body + len.min(16))
                .ok_or("the fmt chunk is cut short")?;
            if len < 16 || f.len() < 16 {
                return Err("the fmt chunk is cut short".into());
            }
            let format = u16_le(f, 0).unwrap_or(0);
            let channels = u32::from(u16_le(f, 2).unwrap_or(0));
            let rate = u32_le(f, 4).unwrap_or(0);
            let bits = u16_le(f, 14).unwrap_or(0);
            // 1 is PCM. WAVE_FORMAT_EXTENSIBLE (0xFFFE) is not taken: the
            // Mission Editor's own tests use plain PCM.
            if format != 1 {
                return Err(format!("format {format}, not PCM (1)"));
            }
            if !matches!(bits, 8 | 16 | 24 | 32) {
                return Err(format!("{bits} bits per sample"));
            }
            return sound(channels, rate);
        }
        // Chunks are padded to an even length.
        i = body
            .checked_add(len + (len & 1))
            .ok_or("a chunk is too long")?;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A valid 1 by 1 PNG header and an end chunk.
    pub(crate) fn png_file(w: u32, h: u32) -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut chunk = b"IHDR".to_vec();
        chunk.extend_from_slice(&w.to_be_bytes());
        chunk.extend_from_slice(&h.to_be_bytes());
        chunk.extend_from_slice(&[8, 6, 0, 0, 0]);
        b.extend_from_slice(&13u32.to_be_bytes());
        b.extend_from_slice(&chunk);
        b.extend_from_slice(&crc32(&chunk).to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes());
        b.extend_from_slice(b"IEND");
        b.extend_from_slice(&crc32(b"IEND").to_be_bytes());
        b
    }

    pub(crate) fn jpeg_file(w: u16, h: u16) -> Vec<u8> {
        let mut b = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 16];
        b.extend_from_slice(b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
        b.extend_from_slice(&[0xFF, 0xC0, 0, 17, 8]);
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&w.to_be_bytes());
        b.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        b.extend_from_slice(&[0xFF, 0xD9]);
        b
    }

    pub(crate) fn ogg_file(channels: u8, rate: u32) -> Vec<u8> {
        let mut p = vec![1];
        p.extend_from_slice(b"vorbis");
        p.extend_from_slice(&0u32.to_le_bytes());
        p.push(channels);
        p.extend_from_slice(&rate.to_le_bytes());
        p.extend_from_slice(&[0; 12]);
        p.push(0xB8);
        p.push(1);
        let mut b = b"OggS\0\x02".to_vec();
        b.extend_from_slice(&[0; 20]);
        b.push(1);
        b.push(u8::try_from(p.len()).expect("short"));
        b.extend_from_slice(&p);
        b
    }

    pub(crate) fn wav_file(format: u16, bits: u16) -> Vec<u8> {
        let mut b = b"RIFF\0\0\0\0WAVE".to_vec();
        b.extend_from_slice(b"LIST\x03\0\0\0abc\0");
        b.extend_from_slice(b"fmt \x10\0\0\0");
        b.extend_from_slice(&format.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&44_100u32.to_le_bytes());
        b.extend_from_slice(&88_200u32.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&bits.to_le_bytes());
        b.extend_from_slice(b"data\x02\0\0\0\0\0");
        b
    }

    #[test]
    fn names_give_their_type() {
        assert_eq!(Media::of_name("a.PNG"), Some(Media::Png));
        assert_eq!(Media::of_name("a.b.jpeg"), Some(Media::Jpeg));
        assert_eq!(Media::of_name("a.jpg"), Some(Media::Jpeg));
        assert_eq!(Media::of_name("x.Ogg"), Some(Media::Ogg));
        assert_eq!(Media::of_name("x.wav"), Some(Media::Wav));
        assert_eq!(Media::of_name("x.mp3"), None);
        assert_eq!(Media::of_name("png"), None);
    }

    #[test]
    fn good_files_pass() {
        check(Media::Png, &png_file(1, 1)).expect("png");
        check(Media::Png, &png_file(8192, 8192)).expect("png at the cap");
        check(Media::Jpeg, &jpeg_file(640, 480)).expect("jpeg");
        check(Media::Ogg, &ogg_file(2, 44_100)).expect("ogg");
        check(Media::Wav, &wav_file(1, 16)).expect("wav");
    }

    #[test]
    fn bad_files_are_refused_with_the_reason() {
        let cases: Vec<(Media, Vec<u8>, &str)> = vec![
            (Media::Png, png_file(8193, 1), "at most 8192"),
            (Media::Png, png_file(0, 1), "no pixels"),
            (Media::Png, b"\x89PNG\r\n\x1a\nxxxx".to_vec(), "IHDR"),
            (Media::Png, jpeg_file(1, 1), "signature"),
            (Media::Jpeg, jpeg_file(9000, 10), "at most 8192"),
            (
                Media::Jpeg,
                vec![0xFF, 0xD8, 0xFF, 0xD9],
                "no start of frame",
            ),
            (Media::Jpeg, png_file(1, 1), "start-of-image"),
            (Media::Ogg, ogg_file(0, 44_100), "0 channels"),
            (Media::Ogg, ogg_file(1, 1000), "sample rate"),
            (Media::Ogg, b"OggS\0\x02".to_vec(), "cut short"),
            (Media::Wav, wav_file(3, 32), "not PCM"),
            (Media::Wav, wav_file(1, 12), "12 bits"),
            (Media::Wav, b"RIFF\0\0\0\0WAVEdata".to_vec(), "no fmt"),
            (Media::Wav, ogg_file(1, 44_100), "RIFF"),
        ];
        for (m, b, want) in cases {
            let e = check(m, &b).expect_err(want);
            assert!(e.contains(want), "{m:?}: {e}");
        }
        let big = vec![0; FILE_MAX + 1];
        assert!(
            check(Media::Png, &big)
                .expect_err("big")
                .contains("at most")
        );
    }

    #[test]
    fn a_damaged_checksum_is_refused() {
        let mut b = png_file(4, 4);
        b[20] ^= 1;
        assert!(check(Media::Png, &b).expect_err("crc").contains("checksum"));
    }

    #[test]
    fn cut_files_never_panic() {
        for f in [
            png_file(3, 3),
            jpeg_file(3, 3),
            ogg_file(1, 8000),
            wav_file(1, 8),
        ] {
            for n in 0..f.len() {
                for m in [Media::Png, Media::Jpeg, Media::Ogg, Media::Wav] {
                    let _ = check(m, &f[..n]);
                }
            }
        }
    }
}

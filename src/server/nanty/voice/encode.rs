//! Codificação da fala do Gemini em nota de voz (spec 138 T09, C-14): o PCM de 16 bits mono (24 kHz)
//! vira Opus num OGG, o formato da nota de voz do Telegram, pelo `opus-pure` (Rust puro, sem C).
//!
//! - **Entrada:** o WAV que o Gemini devolve (`RIFF`/`WAVE`, PCM de 16 bits) ou o PCM cru, com a
//!   taxa informada. Estéreo vira mono pela média. Só as taxas do Opus (8, 12, 16, 24 e 48 kHz).
//! - **Saída:** Opus de voz ([`Application::Voip`]) a [`OPUS_BITRATE`], quadros de 20 ms, com o fim
//!   exato (RFC 7845 §4.4): o silêncio que empurra o atraso do codificador e o grânulo final no
//!   fim do áudio, como o exemplo `encode_gapless` do `opus-pure`.
//! - **Erros em código:** `format` (não é WAV de 16 bits), `rate` (taxa fora do Opus), `empty` e
//!   `encode` (falha do codificador).

use opus_pure::{Application, OggOpusWriter, OpusEncoder, OpusHead, MAX_PACKET_BYTES};

/// Taxa de bits da fala (voz, mono).
pub const OPUS_BITRATE: i32 = 32_000;
/// Taxa da fala do Gemini quando ele manda o PCM cru.
pub const GEMINI_PCM_RATE: u32 = 24_000;
/// Taxas aceitas pelo Opus.
const OPUS_RATES: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];

/// Áudio mono de 16 bits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcm {
    pub rate: u32,
    pub samples: Vec<i16>,
}

impl Pcm {
    /// Duração em segundos, arredondada para cima.
    pub fn duration_secs(&self) -> u32 {
        if self.rate == 0 {
            return 0;
        }
        let secs = (self.samples.len() as u64).div_ceil(u64::from(self.rate));
        u32::try_from(secs).unwrap_or(u32::MAX)
    }
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?]))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes([*b.get(at)?, *b.get(at + 1)?, *b.get(at + 2)?, *b.get(at + 3)?]))
}

/// Amostras de 16 bits (little endian) de `data`, com `channels` intercalados, em mono.
fn mono(data: &[u8], channels: usize) -> Vec<i16> {
    let frame = 2 * channels.max(1);
    data.chunks_exact(frame)
        .map(|f| {
            let sum: i32 = f.chunks_exact(2).map(|s| i32::from(i16::from_le_bytes([s[0], s[1]]))).sum();
            let avg = sum / i32::try_from(channels.max(1)).unwrap_or(1);
            i16::try_from(avg).unwrap_or(if avg < 0 { i16::MIN } else { i16::MAX })
        })
        .collect()
}

/// O PCM cru (16 bits, mono, little endian) na taxa `rate`.
pub fn raw_pcm(data: &[u8], rate: u32) -> Pcm {
    Pcm { rate, samples: mono(data, 1) }
}

/// Lê um WAV PCM de 16 bits. O tamanho do bloco `data` pode vir maior que o arquivo (o Gemini manda
/// o WAV de um fluxo): vale o que existe.
pub fn pcm_of_wav(bytes: &[u8]) -> Result<Pcm, &'static str> {
    if bytes.get(0..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return Err("format");
    }
    let mut at = 12;
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    while at + 8 <= bytes.len() {
        let id = bytes.get(at..at + 4).ok_or("format")?;
        let size = usize::try_from(le32(bytes, at + 4).ok_or("format")?).unwrap_or(usize::MAX);
        let body = at + 8;
        if id == b"fmt " {
            let format = le16(bytes, body).ok_or("format")?;
            let channels = le16(bytes, body + 2).ok_or("format")?;
            let rate = le32(bytes, body + 4).ok_or("format")?;
            let bits = le16(bytes, body + 14).ok_or("format")?;
            fmt = Some((format, channels, rate, bits));
        } else if id == b"data" {
            let (format, channels, rate, bits) = fmt.ok_or("format")?;
            // 1 = PCM; 0xFFFE = WAVE_FORMAT_EXTENSIBLE (o subtipo PCM, conferido pelos bits).
            if !(format == 1 || format == 0xFFFE) || bits != 16 || channels == 0 {
                return Err("format");
            }
            let end = body.saturating_add(size).min(bytes.len());
            let data = bytes.get(body..end).ok_or("format")?;
            return Ok(Pcm { rate, samples: mono(data, usize::from(channels)) });
        }
        // Blocos têm tamanho par (RFC 2361): o byte de enchimento do tamanho ímpar.
        at = body.saturating_add(size).saturating_add(size % 2);
    }
    Err("format")
}

/// O áudio do Gemini: WAV, ou o PCM cru de 24 kHz.
pub fn pcm_of_gemini(bytes: &[u8]) -> Result<Pcm, &'static str> {
    if bytes.starts_with(b"RIFF") {
        pcm_of_wav(bytes)
    } else {
        Ok(raw_pcm(bytes, GEMINI_PCM_RATE))
    }
}

/// Codifica o PCM em Opus num OGG (a nota de voz).
pub fn ogg_opus(pcm: &Pcm) -> Result<Vec<u8>, &'static str> {
    if !OPUS_RATES.contains(&pcm.rate) {
        return Err("rate");
    }
    if pcm.samples.is_empty() {
        return Err("empty");
    }
    let rate = i32::try_from(pcm.rate).map_err(|_| "rate")?;
    let frame = usize::try_from(pcm.rate / 50).map_err(|_| "rate")?;
    let mut encoder = OpusEncoder::new(rate, 1, Application::Voip).map_err(|_| "encode")?;
    encoder.bitrate_bps = OPUS_BITRATE;
    let head = OpusHead::for_encoder(&encoder, pcm.rate);
    // Toda taxa do Opus divide 48 kHz: a conta dos grânulos é exata.
    let ticks = usize::try_from(48_000 / pcm.rate).map_err(|_| "rate")?;
    let total = pcm.samples.len();
    let frames = (total + usize::from(head.pre_skip).div_ceil(ticks)).div_ceil(frame);
    let final_granule = u64::from(head.pre_skip) + (total * ticks) as u64;
    let mut writer = OggOpusWriter::new(Vec::new(), head).map_err(|_| "encode")?;
    let mut packet = vec![0u8; MAX_PACKET_BYTES];
    let mut block = vec![0i16; frame];
    for i in 0..frames {
        let start = (i * frame).min(total);
        let end = (start + frame).min(total);
        block[..end - start].copy_from_slice(&pcm.samples[start..end]);
        block[end - start..].fill(0);
        let n = encoder.encode_s16(&block, frame, &mut packet).map_err(|_| "encode")?;
        let bytes = packet.get(..n).ok_or("encode")?;
        if i + 1 == frames {
            let written = u64::try_from(writer.granule()).unwrap_or(0);
            let duration = u32::try_from(final_granule.saturating_sub(written)).map_err(|_| "encode")?;
            writer.write_packet_with_duration(bytes, duration).map_err(|_| "encode")?;
        } else {
            writer.write_packet(bytes).map_err(|_| "encode")?;
        }
    }
    writer.finish().map_err(|_| "encode")
}

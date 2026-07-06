//! Decodes committed audio blobs into the 16kHz mono f32 PCM whisper-rs
//! expects (issue #89). Pure Rust via `symphonia` - no ffmpeg dependency for
//! the formats symphonia handles (wav/mp3/aac/isomp4).
//!
//! Only formats symphonia can actually demux/decode are attempted; anything
//! else surfaces as `AudioError::UnsupportedContainer` so `route_by_mime`
//! can report an honest gap instead of a silent failure.
//!
//! OGG/Opus honesty (issue #143): Telegram voice notes are OGG/Opus, and
//! symphonia 0.5 ships NO Opus decoder (it decodes Vorbis, not Opus). Rather
//! than pretend otherwise, `decode_audio` falls back to an `ffmpeg` transcode
//! (a WAV symphonia then decodes) when an `ffmpeg` binary is supplied; with no
//! ffmpeg, an OGG/Opus note surfaces `UnsupportedContainer` - an honest gap,
//! never a silent zero-segment transcript.

use std::fmt;
use std::io::{Cursor, Read, Seek, SeekFrom};

use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::conv::IntoSample;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub const TARGET_SAMPLE_RATE: u32 = 16_000;

#[derive(Debug)]
pub enum AudioError {
    UnsupportedContainer,
    NoAudioTrack,
    DecodeFailed(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioError::UnsupportedContainer => write!(f, "no symphonia demuxer for this container"),
            AudioError::NoAudioTrack => write!(f, "no decodable audio track found"),
            AudioError::DecodeFailed(msg) => write!(f, "audio decode failed: {msg}"),
        }
    }
}

/// In-memory bytes symphonia can read/seek over. `MediaSource` needs
/// `Read + Seek + Send + Sync`, which `Cursor<Vec<u8>>` already satisfies -
/// this thin wrapper just supplies the two `MediaSource` trait methods.
struct InMemorySource(Cursor<Vec<u8>>);

impl Read for InMemorySource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl Seek for InMemorySource {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.0.seek(pos)
    }
}

impl MediaSource for InMemorySource {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.0.get_ref().len() as u64)
    }
}

/// Decodes `bytes` (any symphonia-supported container) into 16kHz mono f32
/// PCM samples, ready for `Transcriber::transcribe`.
pub fn decode_to_16k_mono_f32(bytes: &[u8]) -> Result<Vec<f32>, AudioError> {
    let source = Box::new(InMemorySource(Cursor::new(bytes.to_vec())));
    let mss = MediaSourceStream::new(source, Default::default());

    let probed = symphonia::default::get_probe()
        .format(&Hint::new(), mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|_| AudioError::UnsupportedContainer)?;
    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.sample_rate.is_some())
        .ok_or(AudioError::NoAudioTrack)?
        .clone();
    let source_rate = track.codec_params.sample_rate.ok_or(AudioError::NoAudioTrack)?;
    let channels = track
        .codec_params
        .channels
        .map(|c| c.count())
        .unwrap_or(1)
        .max(1);

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|_| AudioError::UnsupportedContainer)?;

    let mut mono: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(AudioError::DecodeFailed(e.to_string())),
        };
        if packet.track_id() != track.id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => append_mono_samples(&decoded, channels, &mut mono),
            Err(SymphoniaError::DecodeError(_)) => continue, // skip a bad frame, keep going
            Err(e) => return Err(AudioError::DecodeFailed(e.to_string())),
        }
    }

    if mono.is_empty() {
        return Err(AudioError::NoAudioTrack);
    }

    Ok(resample_linear(&mono, source_rate, TARGET_SAMPLE_RATE))
}

/// True iff `bytes` start with the OGG container magic (`OggS`). Telegram
/// voice notes are OGG/Opus; symphonia cannot decode Opus, so these route
/// through the ffmpeg transcode fallback in `decode_audio`.
pub fn is_ogg(bytes: &[u8]) -> bool {
    bytes.starts_with(b"OggS")
}

/// Decodes audio `bytes` to 16kHz mono f32 PCM, trying symphonia first and
/// falling back to an `ffmpeg` transcode for containers symphonia cannot
/// handle (chiefly OGG/Opus - see the module doc). `ffmpeg_bin` is the path to
/// an ffmpeg binary (`LIFEOS_FFMPEG_BIN`); when `None`, an unsupported
/// container stays an honest `UnsupportedContainer` error rather than a
/// silent failure.
pub async fn decode_audio(bytes: &[u8], ffmpeg_bin: Option<&str>) -> Result<Vec<f32>, AudioError> {
    match decode_to_16k_mono_f32(bytes) {
        Err(AudioError::UnsupportedContainer) => match ffmpeg_bin {
            Some(bin) => {
                let wav = ffmpeg_transcode_to_wav(bin, bytes).await?;
                decode_to_16k_mono_f32(&wav)
            }
            None => Err(AudioError::UnsupportedContainer),
        },
        other => other,
    }
}

/// Transcodes arbitrary audio `bytes` to a 16kHz mono WAV via an `ffmpeg`
/// subprocess (same shell-out DI shape as `TesseractOcr`). Uses temp files
/// (not pipes) so ffmpeg writes a seekable, well-formed WAV header symphonia
/// can then demux. A missing/failing ffmpeg surfaces as `DecodeFailed`, never
/// a panic.
async fn ffmpeg_transcode_to_wav(ffmpeg_bin: &str, bytes: &[u8]) -> Result<Vec<u8>, AudioError> {
    let input = tempfile::NamedTempFile::new()
        .map_err(|e| AudioError::DecodeFailed(format!("temp input: {e}")))?;
    let output = tempfile::Builder::new()
        .suffix(".wav")
        .tempfile()
        .map_err(|e| AudioError::DecodeFailed(format!("temp output: {e}")))?;
    std::fs::write(input.path(), bytes)
        .map_err(|e| AudioError::DecodeFailed(format!("write temp input: {e}")))?;

    let result = tokio::process::Command::new(ffmpeg_bin)
        .arg("-nostdin")
        .arg("-y")
        .arg("-i")
        .arg(input.path())
        .arg("-ac")
        .arg("1")
        .arg("-ar")
        .arg(TARGET_SAMPLE_RATE.to_string())
        .arg("-f")
        .arg("wav")
        .arg(output.path())
        .output()
        .await
        .map_err(|e| AudioError::DecodeFailed(format!("failed to spawn ffmpeg '{ffmpeg_bin}': {e}")))?;

    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(AudioError::DecodeFailed(format!("ffmpeg transcode failed: {stderr}")));
    }

    std::fs::read(output.path()).map_err(|e| AudioError::DecodeFailed(format!("read transcoded wav: {e}")))
}

/// Mixes down an interleaved multi-channel decoded buffer to mono f32.
fn append_mono_samples(decoded: &AudioBufferRef, channels: usize, out: &mut Vec<f32>) {
    macro_rules! mixdown {
        ($buf:expr) => {{
            let spec = $buf.spec();
            let frames = $buf.frames();
            for frame in 0..frames {
                let mut sum = 0.0f32;
                for ch in 0..spec.channels.count().max(1) {
                    let sample: f32 = IntoSample::<f32>::into_sample($buf.chan(ch)[frame]);
                    sum += sample;
                }
                out.push(sum / channels as f32);
            }
        }};
    }
    match decoded {
        AudioBufferRef::U8(b) => mixdown!(b),
        AudioBufferRef::U16(b) => mixdown!(b),
        AudioBufferRef::U24(b) => mixdown!(b),
        AudioBufferRef::U32(b) => mixdown!(b),
        AudioBufferRef::S8(b) => mixdown!(b),
        AudioBufferRef::S16(b) => mixdown!(b),
        AudioBufferRef::S24(b) => mixdown!(b),
        AudioBufferRef::S32(b) => mixdown!(b),
        AudioBufferRef::F32(b) => mixdown!(b),
        AudioBufferRef::F64(b) => mixdown!(b),
    }
}

/// Minimal linear-interpolation resampler. Good enough for whisper-rs at
/// tiny/base model quality - no need for a heavyweight resampling crate.
fn resample_linear(samples: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || samples.is_empty() {
        return samples.to_vec();
    }
    let ratio = from_rate as f64 / to_rate as f64;
    let out_len = ((samples.len() as f64) / ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = i as f64 * ratio;
        let idx = src_pos.floor() as usize;
        let frac = (src_pos - idx as f64) as f32;
        let a = samples[idx.min(samples.len() - 1)];
        let b = samples[(idx + 1).min(samples.len() - 1)];
        out.push(a + (b - a) * frac);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor as StdCursor;

    fn synth_wav_bytes(sample_rate: u32, freq_hz: f32, duration_secs: f32) -> Vec<u8> {
        let mut buf = StdCursor::new(Vec::new());
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::new(&mut buf, spec).unwrap();
        let n = (sample_rate as f32 * duration_secs) as u32;
        for i in 0..n {
            let t = i as f32 / sample_rate as f32;
            let sample = (t * freq_hz * std::f32::consts::TAU).sin() * i16::MAX as f32 * 0.5;
            writer.write_sample(sample as i16).unwrap();
        }
        writer.finalize().unwrap();
        buf.into_inner()
    }

    #[test]
    fn decodes_a_synthetic_wav_and_resamples_to_16k() {
        let bytes = synth_wav_bytes(44_100, 440.0, 0.5);
        let samples = decode_to_16k_mono_f32(&bytes).unwrap();
        let expected = (44_100.0 * 0.5 / (44_100.0 / TARGET_SAMPLE_RATE as f64)) as usize;
        assert!((samples.len() as i64 - expected as i64).unsigned_abs() < 200);
        assert!(samples.iter().any(|s| s.abs() > 0.01));
    }

    #[test]
    fn already_at_target_rate_is_a_passthrough_length() {
        let bytes = synth_wav_bytes(16_000, 220.0, 0.25);
        let samples = decode_to_16k_mono_f32(&bytes).unwrap();
        assert_eq!(samples.len(), 4_000);
    }

    #[test]
    fn malformed_bytes_are_rejected_not_panicked_on() {
        let result = decode_to_16k_mono_f32(b"not an audio file at all");
        assert!(result.is_err());
    }

    #[test]
    fn is_ogg_detects_the_ogg_magic_and_rejects_wav() {
        assert!(is_ogg(b"OggS\x00\x02..."));
        // A synthetic WAV starts with "RIFF", not "OggS".
        assert!(!is_ogg(&synth_wav_bytes(16_000, 220.0, 0.1)));
        assert!(!is_ogg(b""));
    }

    #[tokio::test]
    async fn decode_audio_passes_symphonia_decodable_wav_without_ffmpeg() {
        // A WAV symphonia can already decode never touches the ffmpeg path.
        let bytes = synth_wav_bytes(16_000, 220.0, 0.25);
        let samples = decode_audio(&bytes, None).await.unwrap();
        assert_eq!(samples.len(), 4_000);
    }

    #[tokio::test]
    async fn decode_audio_reports_unsupported_when_ogg_opus_and_no_ffmpeg() {
        // OGG/Opus with no ffmpeg configured is an honest gap, not a silent
        // empty transcript. (Bytes need only be un-decodable by symphonia.)
        let result = decode_audio(b"OggS\x00\x02 fake opus payload", None).await;
        assert!(matches!(result, Err(AudioError::UnsupportedContainer)));
    }

    #[tokio::test]
    async fn decode_audio_ffmpeg_fallback_fails_cleanly_when_binary_is_missing() {
        // With a bogus ffmpeg path the fallback must surface a DecodeFailed
        // error, never panic (proves the transcode wiring is exercised).
        let result = decode_audio(b"OggS\x00\x02 fake opus payload", Some("/nonexistent/ffmpeg")).await;
        assert!(matches!(result, Err(AudioError::DecodeFailed(_))));
    }
}

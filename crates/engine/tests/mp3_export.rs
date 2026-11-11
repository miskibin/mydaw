use wave_engine::sources::{to_i16_dithered, write_mp3};

fn sine(sr: u32, secs: f32, amp: f32) -> Vec<f32> {
    let n = (sr as f32 * secs) as usize;
    (0..n)
        .flat_map(|i| {
            let v = amp * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / sr as f32).sin();
            [v, v]
        })
        .collect()
}

#[test]
fn mp3_size_header_and_decode() {
    let dir = std::env::temp_dir().join("wave_mp3_test");
    for (sr, kbps) in [(44_100u32, 320u32), (48_000, 128)] {
        let path = dir.join(format!("t_{sr}_{kbps}.mp3"));
        write_mp3(&path, &sine(sr, 2.0, 0.5), sr, kbps, Some("My Song")).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..3], b"ID3", "ID3v2 tag leads the file");
        assert!(bytes.windows(7).any(|w| w == b"My Song"), "title is in the tag");
        let expect = kbps as f64 * 1000.0 / 8.0 * 2.0;
        let ratio = bytes.len() as f64 / expect;
        assert!((0.9..=1.1).contains(&ratio), "size {} vs {expect} (ratio {ratio})", bytes.len());
        let d = wave_audio_file::decode_stereo(&path, |_| None).unwrap();
        assert!((d.seconds() - 2.0).abs() < 0.1, "decoded {} s", d.seconds());
    }
}

#[test]
fn mp3_without_title_starts_with_frame_sync_and_rejects_bad_kbps() {
    let path = std::env::temp_dir().join("wave_mp3_test").join("notitle.mp3");
    write_mp3(&path, &sine(44_100, 1.0, 0.5), 44_100, 192, None).unwrap();
    let b = std::fs::read(&path).unwrap();
    assert!(&b[..3] == b"ID3" || (b[0] == 0xFF && b[1] & 0xE0 == 0xE0));
    assert!(write_mp3(&path, &[0.0; 4], 44_100, 100, None).is_err());
}

#[test]
fn dither_clips_and_survives_nan() {
    let q = to_i16_dithered(&[2.0, -2.0, f32::NAN, 0.0]);
    assert!(q[0] >= 32766);
    assert!(q[1] <= -32766);
    assert!(q[2].abs() <= 1 && q[3].abs() <= 1);
}

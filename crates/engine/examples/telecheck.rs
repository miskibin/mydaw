//! Runs the live path (engine + rt + telemetry) with a vocal and prints the tune meters per frame.
use std::sync::{Arc, Mutex};
use wave_engine::*;
fn main() {
    let wav = std::env::args().nth(1).unwrap();
    let e = Engine::new(Project::demo(), Arc::new(NoPlugins));
    let mut rt = e.create_rt(48000.0, 256);
    let p = e.project();
    let mut tune = wave_engine::catalog::new_fx(5000, FxKind::Tune, p.bpm);
    tune.params.insert("retune".into(), 0.0);
    e.apply(
        &[Op::PutFx {
            insert_id: 6,
            fx: tune,
            index: None,
        }],
        false,
        None,
    )
    .unwrap();
    e.import_audio(&[wav.into()], Some((5, 0.0))).unwrap();
    e.watch_fx(&[5000]);
    let frames: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
    let f2 = frames.clone();
    e.set_frame_sink(Some(Box::new(move |b| f2.lock().unwrap().push(b))));
    e.play(0.0);
    let (mut l, mut r) = (vec![0f32; 256], vec![0f32; 256]);
    let t0 = std::time::Instant::now();
    let mut n = 0u64;
    while t0.elapsed().as_secs_f64() < 3.0 {
        // real time pacing
        let due = n as f64 * 256.0 / 48000.0;
        if t0.elapsed().as_secs_f64() < due {
            std::thread::sleep(std::time::Duration::from_micros(500));
            continue;
        }
        rt.process(&mut l, &mut r, None);
        n += 1;
    }
    let fr = frames.lock().unwrap();
    if let Some(out) = std::env::args().nth(2) {
        // replay file: [u32 len][frame]... with ~16.7 ms between frames
        let mut buf = vec![];
        for b in fr.iter() {
            buf.extend_from_slice(&(b.len() as u32).to_le_bytes());
            buf.extend_from_slice(b);
        }
        std::fs::write(&out, buf).unwrap();
        eprintln!("wrote {} frames to {out}", fr.len());
    }
    for (k, b) in fr.iter().enumerate().step_by(6) {
        // header 36 bytes, then nIns*12
        let nins = u32::from_le_bytes(b[32..36].try_into().unwrap()) as usize;
        let mut o = 36 + nins * 12;
        let nfx = u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        o += 4;
        if nfx == 0 {
            continue;
        }
        let id = u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let m: Vec<f32> = (0..6)
            .map(|i| f32::from_le_bytes(b[o + 16 + i * 4..o + 20 + i * 4].try_into().unwrap()))
            .collect();
        let beat = f64::from_le_bytes(b[8..16].try_into().unwrap());
        println!(
            "frame {k:3} beat {beat:5.2} fx {id} in {:7.1}Hz tgt {:7.1} out {:7.1} conf {:.2}",
            m[0], m[1], m[2], m[3]
        );
    }
}

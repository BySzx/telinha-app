use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::State;

#[cfg_attr(not(windows), allow(dead_code))]
pub const SAMPLE_RATE: usize = 48_000;
#[cfg_attr(not(windows), allow(dead_code))]
pub const CHANNELS: usize = 2;
#[cfg_attr(not(windows), allow(dead_code))]
const CHUNK_FRAMES: usize = 960;

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AudioApp {
    pub pid: u32,
    pub name: String,
    pub active: bool,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AudioSource {
    System {
        #[serde(default)]
        exclude: Vec<u32>,
        #[serde(default, rename = "excludeNames")]
        exclude_names: Vec<String>,
    },
    Apps { pids: Vec<u32> },
}

#[derive(Default)]
pub struct AudioState {
    stop: Mutex<Option<Arc<AtomicBool>>>,
}

impl AudioState {
    fn stop_current(&self) {
        if let Some(flag) = self.stop.lock().unwrap_or_else(|e| e.into_inner()).take() {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

#[tauri::command]
pub fn list_audio_apps() -> Result<Vec<AudioApp>, String> {
    std::thread::spawn(platform::list_apps)
        .join()
        .map_err(|_| "falha ao listar apps de áudio".to_string())?
}

#[tauri::command]
pub fn start_audio(source: AudioSource, on_data: Channel, state: State<'_, AudioState>) -> Result<(), String> {
    state.stop_current();
    let stop = Arc::new(AtomicBool::new(false));
    *state.stop.lock().unwrap_or_else(|e| e.into_inner()) = Some(stop.clone());

    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    std::thread::Builder::new()
        .name("telinha-audio".into())
        .spawn(move || {
            platform::capture(source, &stop, ready_tx, move |bytes| {
                on_data.send(InvokeResponseBody::Raw(bytes)).is_ok()
            });
        })
        .map_err(|e| e.to_string())?;

    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(result) => result,
        Err(_) => Err("tempo esgotado ao iniciar a captura de áudio".into()),
    }
}

#[tauri::command]
pub fn stop_audio(state: State<'_, AudioState>) {
    state.stop_current();
}

#[cfg(windows)]
mod platform {
    use super::{AudioApp, AudioSource, CHANNELS, CHUNK_FRAMES, SAMPLE_RATE};
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Sender};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};
    use wasapi::{initialize_mta, AudioClient, Direction, SampleType, StreamMode, WaveFormat};
    use windows::core::Interface;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Media::Audio::{
        eConsole, eRender, AudioSessionStateActive, IAudioSessionControl2, IAudioSessionManager2,
        IMMDeviceEnumerator, MMDeviceEnumerator,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };

    struct ProcInfo {
        parent: u32,
        exe: String,
    }

    fn process_table() -> HashMap<u32, ProcInfo> {
        let mut table = HashMap::new();
        unsafe {
            let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
                return table;
            };
            let mut entry = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            let mut ok = Process32FirstW(snapshot, &mut entry).is_ok();
            while ok {
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
                table.insert(
                    entry.th32ProcessID,
                    ProcInfo {
                        parent: entry.th32ParentProcessID,
                        exe: String::from_utf16_lossy(&entry.szExeFile[..len]),
                    },
                );
                ok = Process32NextW(snapshot, &mut entry).is_ok();
            }
            let _ = CloseHandle(snapshot);
        }
        table
    }

    fn tree_root(pid: u32, table: &HashMap<u32, ProcInfo>) -> u32 {
        let mut current = pid;
        let mut seen = HashSet::new();
        while let Some(info) = table.get(&current) {
            if !seen.insert(current) {
                break;
            }
            match table.get(&info.parent) {
                Some(parent) if parent.exe.eq_ignore_ascii_case(&info.exe) => current = info.parent,
                _ => break,
            }
        }
        current
    }

    fn descends_from(pid: u32, ancestor: u32, table: &HashMap<u32, ProcInfo>) -> bool {
        let mut current = pid;
        let mut seen = HashSet::new();
        while seen.insert(current) {
            if current == ancestor {
                return true;
            }
            match table.get(&current) {
                Some(info) if info.parent != 0 => current = info.parent,
                _ => return false,
            }
        }
        false
    }

    fn display_name(exe: &str) -> String {
        let stem = exe.strip_suffix(".exe").or_else(|| exe.strip_suffix(".EXE")).unwrap_or(exe);
        let mut chars = stem.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().chain(chars).collect(),
            None => stem.to_string(),
        }
    }

    fn session_pids() -> Result<Vec<(u32, bool)>, String> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let result = (|| -> windows::core::Result<Vec<(u32, bool)>> {
                let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
                let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
                let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
                let sessions = manager.GetSessionEnumerator()?;
                let mut out = Vec::new();
                for i in 0..sessions.GetCount()? {
                    let control = sessions.GetSession(i)?;
                    let control2: IAudioSessionControl2 = control.cast()?;
                    let pid = control2.GetProcessId().unwrap_or(0);
                    if pid != 0 {
                        out.push((pid, control.GetState()? == AudioSessionStateActive));
                    }
                }
                Ok(out)
            })();
            result.map_err(|e| e.to_string())
        }
    }

    pub fn list_apps() -> Result<Vec<AudioApp>, String> {
        let sessions = session_pids()?;
        let table = process_table();
        let own = std::process::id();
        let mut apps: HashMap<u32, AudioApp> = HashMap::new();
        for (pid, active) in sessions {
            if descends_from(pid, own, &table) {
                continue;
            }
            let root = tree_root(pid, &table);
            let name = table.get(&root).map(|p| display_name(&p.exe)).unwrap_or_else(|| format!("PID {root}"));
            apps.entry(root)
                .and_modify(|app| app.active |= active)
                .or_insert(AudioApp { pid: root, name, active });
        }
        let mut list: Vec<AudioApp> = apps.into_values().collect();
        list.sort_by(|a, b| b.active.cmp(&a.active).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        Ok(list)
    }

    struct Source {
        buf: VecDeque<f32>,
        last: Instant,
    }

    #[derive(Default)]
    struct Mixer {
        sources: Mutex<HashMap<u32, Source>>,
        wake: Condvar,
    }

    impl Mixer {
        fn push(&self, id: u32, samples: impl Iterator<Item = f32>) {
            let mut sources = self.sources.lock().unwrap_or_else(|e| e.into_inner());
            let src = sources.entry(id).or_insert_with(|| Source { buf: VecDeque::new(), last: Instant::now() });
            src.buf.extend(samples);
            src.last = Instant::now();
            drop(sources);
            self.wake.notify_one();
        }
    }

    fn capture_one(id: u32, target_pid: u32, include_tree: bool, stop: Arc<AtomicBool>, mixer: Arc<Mixer>, ready: Sender<Result<(), String>>) {
        let _ = initialize_mta();
        let format = WaveFormat::new(32, 32, &SampleType::Float, SAMPLE_RATE, CHANNELS, None);

        let setup = (|| -> Result<_, Box<dyn std::error::Error>> {
            let mut client = AudioClient::new_application_loopback_client(target_pid, include_tree)?;
            client.initialize_client(
                &format,
                &Direction::Capture,
                &StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 0 },
            )?;
            let event = client.set_get_eventhandle()?;
            let capture = client.get_audiocaptureclient()?;
            client.start_stream()?;
            Ok((client, event, capture))
        })();

        let (client, event, capture) = match setup {
            Ok(parts) => {
                let _ = ready.send(Ok(()));
                parts
            }
            Err(err) => {
                let _ = ready.send(Err(format!("não foi possível capturar o áudio: {err}")));
                return;
            }
        };

        let mut bytes: VecDeque<u8> = VecDeque::with_capacity(CHUNK_FRAMES * CHANNELS * 4 * 8);
        while !stop.load(Ordering::Relaxed) {
            if event.wait_for_event(100).is_err() {
                continue;
            }
            loop {
                match capture.get_next_packet_size() {
                    Ok(Some(frames)) if frames > 0 => {
                        let before = bytes.len();
                        match capture.read_from_device_to_deque(&mut bytes) {
                            Ok(info) if info.flags.silent => {
                                for byte in bytes.iter_mut().skip(before) {
                                    *byte = 0;
                                }
                            }
                            Ok(_) => {}
                            Err(_) => break,
                        }
                    }
                    _ => break,
                }
            }
            let whole = bytes.len() / 4 * 4;
            if whole > 0 {
                let raw: Vec<u8> = bytes.drain(..whole).collect();
                mixer.push(id, raw.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])));
            }
        }
        let _ = client.stop_stream();
    }

    fn spawn_source(id: u32, pid: u32, include_tree: bool, stop: &Arc<AtomicBool>, mixer: &Arc<Mixer>) -> mpsc::Receiver<Result<(), String>> {
        let (tx, rx) = mpsc::channel();
        let stop = stop.clone();
        let mixer = mixer.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("telinha-audio-{pid}"))
            .spawn(move || capture_one(id, pid, include_tree, stop, mixer, tx.clone()));
        if let Err(err) = spawned {
            let (etx, erx) = mpsc::channel();
            let _ = etx.send(Err(err.to_string()));
            return erx;
        }
        rx
    }

    fn mix_loop(stop: &AtomicBool, mixer: &Mixer, mut emit: impl FnMut(Vec<u8>) -> bool) {
        let need = CHUNK_FRAMES * CHANNELS;
        let backlog = need * 6;
        let mut last_emit = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            let guard = mixer.sources.lock().unwrap_or_else(|e| e.into_inner());
            let (mut sources, _) = mixer
                .wake
                .wait_timeout(guard, Duration::from_millis(5))
                .unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            let mut any = false;
            let mut all_ready = true;
            let mut active = 0;
            for src in sources.values() {
                if !src.buf.is_empty() {
                    any = true;
                }
                if now.duration_since(src.last) < Duration::from_millis(80) {
                    active += 1;
                    if src.buf.len() < need {
                        all_ready = false;
                    }
                }
            }
            let ready = active > 0 && all_ready;
            let late = any && now.duration_since(last_emit) >= Duration::from_millis(30);
            if !ready && !late {
                continue;
            }
            let mut out = vec![0f32; need];
            for src in sources.values_mut() {
                if src.buf.len() > backlog {
                    let excess = (src.buf.len() - backlog) / CHANNELS * CHANNELS;
                    src.buf.drain(..excess);
                }
                let n = need.min(src.buf.len());
                for (slot, sample) in out.iter_mut().zip(src.buf.drain(..n)) {
                    *slot += sample;
                }
            }
            drop(sources);
            last_emit = now;
            let mut chunk = Vec::with_capacity(need * 4);
            for sample in out {
                chunk.extend_from_slice(&sample.clamp(-1.0, 1.0).to_le_bytes());
            }
            if !emit(chunk) {
                stop.store(true, Ordering::Relaxed);
            }
        }
    }

    pub fn capture(source: AudioSource, stop: &Arc<AtomicBool>, ready: Sender<Result<(), String>>, emit: impl FnMut(Vec<u8>) -> bool) {
        let mixer = Arc::new(Mixer::default());
        let own = std::process::id();
        match source {
            AudioSource::System { exclude, exclude_names } if exclude.is_empty() && exclude_names.is_empty() => {
                let rx = spawn_source(own, own, false, stop, &mixer);
                let result = rx.recv_timeout(Duration::from_secs(4)).unwrap_or_else(|_| Err("tempo esgotado".into()));
                let failed = result.is_err();
                let _ = ready.send(result);
                if failed {
                    return;
                }
            }
            AudioSource::Apps { pids } => {
                let receivers: Vec<_> = pids.iter().map(|&pid| spawn_source(pid, pid, true, stop, &mixer)).collect();
                let results: Vec<_> = receivers
                    .iter()
                    .map(|rx| rx.recv_timeout(Duration::from_secs(4)).unwrap_or_else(|_| Err("tempo esgotado".into())))
                    .collect();
                if !results.is_empty() && results.iter().all(|r| r.is_err()) {
                    let _ = ready.send(results.into_iter().next().unwrap());
                    return;
                }
                let _ = ready.send(Ok(()));
            }
            AudioSource::System { exclude, exclude_names } => {
                let excluded: HashSet<u32> = exclude.into_iter().collect();
                let excluded_names: HashSet<String> = exclude_names.into_iter().map(|n| n.to_lowercase()).collect();
                let _ = ready.send(Ok(()));
                let scan_stop = stop.clone();
                let scan_mixer = mixer.clone();
                let _ = std::thread::Builder::new().name("telinha-audio-scan".into()).spawn(move || {
                    let mut started: HashSet<u32> = HashSet::new();
                    while !scan_stop.load(Ordering::Relaxed) {
                        if let Ok(sessions) = session_pids() {
                            let table = process_table();
                            for (pid, _) in sessions {
                                if descends_from(pid, own, &table) {
                                    continue;
                                }
                                let root = tree_root(pid, &table);
                                let name = table.get(&root).map(|p| display_name(&p.exe).to_lowercase()).unwrap_or_default();
                                if excluded.contains(&root)
                                    || excluded_names.contains(&name)
                                    || excluded.iter().any(|&x| descends_from(pid, x, &table))
                                {
                                    continue;
                                }
                                if started.insert(root) {
                                    let _ = spawn_source(root, root, true, &scan_stop, &scan_mixer);
                                }
                            }
                        }
                        for _ in 0..20 {
                            if scan_stop.load(Ordering::Relaxed) {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                });
            }
        }
        mix_loop(stop, &mixer, emit);
    }
}

#[cfg(not(windows))]
mod platform {
    use super::{AudioApp, AudioSource};
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::Sender;
    use std::sync::Arc;

    pub fn list_apps() -> Result<Vec<AudioApp>, String> {
        Ok(Vec::new())
    }

    pub fn capture(
        _source: AudioSource,
        _stop: &Arc<AtomicBool>,
        ready: Sender<Result<(), String>>,
        _emit: impl FnMut(Vec<u8>) -> bool,
    ) {
        let _ = ready.send(Err("áudio nativo disponível só no Windows".into()));
    }
}

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

#[derive(Deserialize, Debug, Clone, Copy)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AudioSource {
    System,
    App { pid: u32 },
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

    let (target_pid, include_tree) = match source {
        AudioSource::System => (std::process::id(), false),
        AudioSource::App { pid } => (pid, true),
    };

    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    std::thread::Builder::new()
        .name("telinha-audio".into())
        .spawn(move || {
            platform::capture(target_pid, include_tree, &stop, ready_tx, |bytes| {
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
    use super::{AudioApp, CHANNELS, CHUNK_FRAMES, SAMPLE_RATE};
    use std::collections::{HashMap, HashSet, VecDeque};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::Sender;
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

    pub fn list_apps() -> Result<Vec<AudioApp>, String> {
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
            let sessions = result.map_err(|e| e.to_string())?;

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
    }

    pub fn capture(
        target_pid: u32,
        include_tree: bool,
        stop: &AtomicBool,
        ready: Sender<Result<(), String>>,
        mut emit: impl FnMut(Vec<u8>) -> bool,
    ) {
        let _ = initialize_mta();
        let format = WaveFormat::new(32, 32, &SampleType::Float, SAMPLE_RATE, CHANNELS, None);
        let block_align = format.get_blockalign() as usize;
        let chunk_bytes = CHUNK_FRAMES * block_align;

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

        let mut queue: VecDeque<u8> = VecDeque::with_capacity(chunk_bytes * 8);
        while !stop.load(Ordering::Relaxed) {
            if event.wait_for_event(100).is_err() {
                continue;
            }
            loop {
                match capture.get_next_packet_size() {
                    Ok(Some(frames)) if frames > 0 => {
                        let before = queue.len();
                        match capture.read_from_device_to_deque(&mut queue) {
                            Ok(info) if info.flags.silent => {
                                for byte in queue.iter_mut().skip(before) {
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
            while queue.len() >= chunk_bytes {
                let chunk: Vec<u8> = queue.drain(..chunk_bytes).collect();
                if !emit(chunk) {
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
            }
        }
        let _ = client.stop_stream();
    }
}

#[cfg(not(windows))]
mod platform {
    use super::AudioApp;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::Sender;

    pub fn list_apps() -> Result<Vec<AudioApp>, String> {
        Ok(Vec::new())
    }

    pub fn capture(
        _target_pid: u32,
        _include_tree: bool,
        _stop: &AtomicBool,
        ready: Sender<Result<(), String>>,
        _emit: impl FnMut(Vec<u8>) -> bool,
    ) {
        let _ = ready.send(Err("áudio nativo disponível só no Windows".into()));
    }
}

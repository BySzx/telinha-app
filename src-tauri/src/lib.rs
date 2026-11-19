mod audio;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(audio::AudioState::default())
        .invoke_handler(tauri::generate_handler![
            audio::list_audio_apps,
            audio::start_audio,
            audio::stop_audio
        ])
        .run(tauri::generate_context!())
        .expect("falha ao iniciar o Telinha");
}

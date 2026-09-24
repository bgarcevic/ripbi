#![forbid(unsafe_code)]
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod analysis;

use tauri_plugin_dialog::DialogExt;

#[tauri::command]
async fn export_analysis(app: tauri::AppHandle, contents: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let Some(file) = app
            .dialog()
            .file()
            .set_title("Export ripbi analysis")
            .set_file_name("ripbi-analysis.json")
            .add_filter("JSON", &["json"])
            .blocking_save_file()
        else {
            return Ok(false);
        };
        let path = file.into_path().map_err(|error| error.to_string())?;
        std::fs::write(path, contents)
            .map_err(|error| format!("Could not save analysis: {error}"))?;
        Ok(true)
    })
    .await
    .map_err(|error| format!("Export worker failed: {error}"))?
}

#[tauri::command]
async fn analyze(model: String, reports: Vec<String>) -> Result<analysis::Analysis, String> {
    tauri::async_runtime::spawn_blocking(move || analysis::run(&model, &reports))
        .await
        .map_err(|error| format!("Analysis worker failed: {error}"))?
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![analyze, export_analysis])
        .run(tauri::generate_context!())?;
    Ok(())
}

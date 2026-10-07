#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Some(result) = httpyac_lsp::run_if_requested() {
        if let Err(error) = result {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
    } else {
        eprintln!("Expected --httpyac-language-server");
        std::process::exit(2);
    }
}

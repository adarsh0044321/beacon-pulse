fn main() {
    let host_dir = "../../apps/ui/dist-host";
    let _ = std::fs::create_dir_all(host_dir);
    let host_idx = format!("{}/index.html", host_dir);
    if !std::path::Path::new(&host_idx).exists() {
        let _ = std::fs::write(host_idx, "<html></html>");
    }

    let player_dir = "../../apps/ui/dist-player";
    let _ = std::fs::create_dir_all(player_dir);
    let player_idx = format!("{}/index.html", player_dir);
    if !std::path::Path::new(&player_idx).exists() {
        let _ = std::fs::write(player_idx, "<html></html>");
    }
}
